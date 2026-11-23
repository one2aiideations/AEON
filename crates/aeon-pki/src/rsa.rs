//! RSA signature verification: PKCS#1 v1.5 and PSS.

use crate::bignum::Uint;
use crate::der::{self, tag};
use crate::Hash;
use std::cmp::Ordering;

/// Smallest modulus accepted, in bits.
pub const MIN_MODULUS_BITS: usize = 2048;
/// Largest modulus accepted, in bits.
pub const MAX_MODULUS_BITS: usize = 8192;

/// An RSA public key.
#[derive(Clone, Debug)]
pub struct PublicKey {
    n: Uint,
    e: Uint,
    len: usize,
}

impl PublicKey {
    /// Parse `RSAPublicKey ::= SEQUENCE { modulus, publicExponent }`.
    #[must_use]
    pub fn from_der(bytes: &[u8]) -> Option<Self> {
        let mut seq = der::single(bytes, tag::SEQUENCE).ok()?;
        let n = der::unsigned_integer(seq.expect(tag::INTEGER).ok()?.content).ok()?;
        let e = der::unsigned_integer(seq.expect(tag::INTEGER).ok()?.content).ok()?;
        seq.finish().ok()?;
        Self::from_parts(n, e)
    }

    /// Build from big-endian modulus and exponent.
    #[must_use]
    pub fn from_parts(n: &[u8], e: &[u8]) -> Option<Self> {
        let (n, e) = (Uint::from_be(n), Uint::from_be(e));
        let bits = n.bits();
        // Odd modulus of a sane size, and an exponent of at least 3 that is odd.
        if !(MIN_MODULUS_BITS..=MAX_MODULUS_BITS).contains(&bits) || !n.is_odd() {
            return None;
        }
        if !e.is_odd() || e.cmp_to(&Uint::from_u64(3)) == Ordering::Less || e.bits() > 64 {
            return None;
        }
        Some(Self {
            n,
            e,
            len: bits.div_ceil(8),
        })
    }

    /// Modulus length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Never empty; present for the usual pairing with `len`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The signature as the encoded message `s^e mod n`, `len` bytes long.
    fn recover(&self, signature: &[u8]) -> Option<Vec<u8>> {
        if signature.len() != self.len {
            return None;
        }
        let s = Uint::from_be(signature);
        if s.cmp_to(&self.n) != Ordering::Less {
            return None;
        }
        s.mod_pow(&self.e, &self.n).to_be_padded(self.len)
    }

    /// Verify an RSASSA-PKCS1-v1_5 signature over `message`.
    #[must_use]
    pub fn verify_pkcs1(&self, hash: Hash, message: &[u8], signature: &[u8]) -> bool {
        let Some(em) = self.recover(signature) else {
            return false;
        };
        // EM = 00 01 FF..FF 00 DigestInfo, compared against the exact expected encoding.
        let digest_info = hash.digest_info(&hash.digest(message));
        if em.len() < digest_info.len() + 11 {
            return false;
        }
        let pad = em.len() - digest_info.len() - 3;
        let mut expected = Vec::with_capacity(em.len());
        expected.extend([0x00, 0x01]);
        expected.extend(std::iter::repeat_n(0xFF, pad));
        expected.push(0x00);
        expected.extend(digest_info);
        crate::ct_eq(&em, &expected)
    }

    /// Verify an RSASSA-PSS signature (MGF1 with the same hash, salt length `salt_len`).
    #[must_use]
    pub fn verify_pss(
        &self,
        hash: Hash,
        salt_len: usize,
        message: &[u8],
        signature: &[u8],
    ) -> bool {
        let Some(em) = self.recover(signature) else {
            return false;
        };
        let em_bits = self.n.bits() - 1;
        let em_len = em_bits.div_ceil(8);
        // The recovered value has `len` bytes; when em_bits is a multiple of 8 the top byte is dropped.
        let Some(em) = em.get(self.len - em_len..) else {
            return false;
        };
        let h_len = hash.len();
        if em_len < h_len + salt_len + 2 || em[em_len - 1] != 0xBC {
            return false;
        }
        let (masked_db, rest) = em.split_at(em_len - h_len - 1);
        let h = &rest[..h_len];
        let top_clear = 8 * em_len - em_bits;
        if top_clear > 0 && masked_db[0] >> (8 - top_clear) != 0 {
            return false;
        }
        let mask = mgf1(hash, h, masked_db.len());
        let mut db: Vec<u8> = masked_db.iter().zip(&mask).map(|(a, b)| a ^ b).collect();
        if top_clear > 0 {
            db[0] &= 0xFF >> top_clear;
        }
        let split = db.len() - salt_len - 1;
        if db[..split].iter().any(|&b| b != 0) || db[split] != 1 {
            return false;
        }
        let salt = &db[split + 1..];
        let mut m_prime = vec![0u8; 8];
        m_prime.extend(hash.digest(message));
        m_prime.extend(salt);
        crate::ct_eq(&hash.digest(&m_prime), h)
    }
}

fn mgf1(hash: Hash, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + hash.len());
    let mut counter = 0u32;
    while out.len() < len {
        let mut block = seed.to_vec();
        block.extend(counter.to_be_bytes());
        out.extend(hash.digest(&block));
        counter += 1;
    }
    out.truncate(len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_weak_or_odd_keys() {
        let e = [1, 0, 1];
        let small = vec![0xFF; 128]; // 1024 bits
        assert!(PublicKey::from_parts(&small, &e).is_none());
        let mut even = vec![0xFF; 256];
        *even.last_mut().unwrap() = 0xFE;
        assert!(PublicKey::from_parts(&even, &e).is_none());
        let ok = vec![0xFF; 256];
        assert!(PublicKey::from_parts(&ok, &e).is_some());
        assert!(PublicKey::from_parts(&ok, &[1]).is_none()); // e = 1
        assert!(PublicKey::from_parts(&ok, &[2]).is_none()); // even
    }

    #[test]
    fn mgf1_known_length() {
        assert_eq!(mgf1(Hash::Sha256, b"seed", 70).len(), 70);
        assert_eq!(
            mgf1(Hash::Sha256, b"seed", 70)[..32],
            mgf1(Hash::Sha256, b"seed", 32)[..]
        );
    }
}
