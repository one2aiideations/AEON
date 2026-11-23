//! ChaCha20-Poly1305 authenticated encryption (RFC 8439 section 2.8), the cipher AEON's TLS
//! client offers.
//!
//! A (key, nonce) pair must never encrypt two different messages; TLS guarantees that with a
//! per-record sequence number.

use crate::chacha20;
use crate::ct_eq;
use crate::poly1305::{poly1305, TAG_LEN};

/// Key size in bytes.
pub const KEY_LEN: usize = 32;
/// Nonce size in bytes.
pub const NONCE_LEN: usize = 12;

fn pad16(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(16) {
        v.push(0);
    }
}

fn tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; TAG_LEN] {
    let block0 = chacha20::block(key, 0, nonce);
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&block0[..32]);
    let mut mac = Vec::with_capacity(aad.len() + ct.len() + 32);
    mac.extend_from_slice(aad);
    pad16(&mut mac);
    mac.extend_from_slice(ct);
    pad16(&mut mac);
    mac.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    mac.extend_from_slice(&(ct.len() as u64).to_le_bytes());
    poly1305(&otk, &mac)
}

/// Encrypt `plaintext`; returns the ciphertext with the 16-byte tag appended.
#[must_use]
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let mut out = plaintext.to_vec();
    chacha20::xor(key, 1, nonce, &mut out);
    let t = tag(key, nonce, aad, &out);
    out.extend_from_slice(&t);
    out
}

/// Check the tag and decrypt. Returns `None` when the message was altered, the associated data
/// differs, or the input is shorter than a tag. Nothing is decrypted before the tag checks out.
#[must_use]
pub fn open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let split = sealed.len().checked_sub(TAG_LEN)?;
    let (ct, got) = sealed.split_at(split);
    if !ct_eq(&tag(key, nonce, aad, ct), got) {
        return None;
    }
    let mut out = ct.to_vec();
    chacha20::xor(key, 1, nonce, &mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::from_hex;

    const PT: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";

    fn vectors() -> ([u8; 32], [u8; 12], Vec<u8>) {
        let key: [u8; 32] =
            from_hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f")
                .unwrap()
                .try_into()
                .unwrap();
        let nonce: [u8; 12] = from_hex("070000004041424344454647")
            .unwrap()
            .try_into()
            .unwrap();
        (key, nonce, from_hex("50515253c0c1c2c3c4c5c6c7").unwrap())
    }

    #[test]
    fn rfc8439_aead_vector() {
        let (key, nonce, aad) = vectors();
        let sealed = seal(&key, &nonce, &aad, PT);
        let expect = from_hex("d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b61161ae10b594f09e26a7e902ecbd0600691").unwrap();
        assert_eq!(sealed, expect);
        assert_eq!(open(&key, &nonce, &aad, &sealed).unwrap(), PT);
    }

    #[test]
    fn any_change_is_rejected() {
        let (key, nonce, aad) = vectors();
        let sealed = seal(&key, &nonce, &aad, PT);
        for i in [0, 50, sealed.len() - 1] {
            let mut bad = sealed.clone();
            bad[i] ^= 1;
            assert!(open(&key, &nonce, &aad, &bad).is_none(), "byte {i}");
        }
        assert!(open(&key, &nonce, b"other aad", &sealed).is_none());
        let mut n2 = nonce;
        n2[0] ^= 1;
        assert!(open(&key, &n2, &aad, &sealed).is_none());
        assert!(open(&key, &nonce, &aad, &sealed[..10]).is_none());
        assert!(open(&key, &nonce, &aad, &[]).is_none());
    }

    #[test]
    fn empty_plaintext_and_associated_data() {
        let (key, nonce, _) = vectors();
        let sealed = seal(&key, &nonce, &[], &[]);
        assert_eq!(sealed.len(), TAG_LEN);
        assert_eq!(open(&key, &nonce, &[], &sealed).unwrap(), Vec::<u8>::new());
    }
}
