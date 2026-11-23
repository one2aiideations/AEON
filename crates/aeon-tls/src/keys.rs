//! The TLS 1.3 key schedule (RFC 8446 section 7) for SHA-256, and record protection keys.

use aeon_crypto::hmac::{expand_label, hkdf_extract, hmac_sha256};
use sha2::{Digest, Sha256};

/// SHA-256 of `data`.
pub fn hash(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn to32(v: Vec<u8>) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    out
}

/// `Derive-Secret(secret, label, transcript)`.
pub fn derive_secret(secret: &[u8; 32], label: &str, transcript_hash: &[u8; 32]) -> [u8; 32] {
    to32(expand_label(secret, label, transcript_hash, 32))
}

/// The handshake secret from the key-exchange output.
pub fn handshake_secret(shared: &[u8; 32]) -> [u8; 32] {
    let early = hkdf_extract(&[], &[0u8; 32]);
    let derived = derive_secret(&early, "derived", &hash(b""));
    hkdf_extract(&derived, shared)
}

/// The master secret from the handshake secret.
pub fn master_secret(handshake: &[u8; 32]) -> [u8; 32] {
    let derived = derive_secret(handshake, "derived", &hash(b""));
    hkdf_extract(&derived, &[0u8; 32])
}

/// `verify_data` for a Finished message.
pub fn finished(traffic_secret: &[u8; 32], transcript_hash: &[u8; 32]) -> [u8; 32] {
    let key = expand_label(traffic_secret, "finished", &[], 32);
    hmac_sha256(&key, transcript_hash)
}

/// Protection state for one direction of the record layer.
pub struct Direction {
    secret: [u8; 32],
    key: [u8; 32],
    iv: [u8; 12],
    seq: u64,
}

impl Direction {
    /// Keys derived from a traffic secret.
    pub fn new(secret: [u8; 32]) -> Self {
        let key = to32(expand_label(&secret, "key", &[], 32));
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&expand_label(&secret, "iv", &[], 12));
        Self {
            secret,
            key,
            iv,
            seq: 0,
        }
    }

    /// The key.
    pub fn key(&self) -> &[u8; 32] {
        &self.key
    }

    /// The per-record nonce, then advance the sequence number. `None` when it would wrap
    /// (the connection must be closed long before that).
    pub fn next_nonce(&mut self) -> Option<[u8; 12]> {
        let seq = self.seq;
        self.seq = seq.checked_add(1)?;
        let mut nonce = self.iv;
        for (n, s) in nonce[4..].iter_mut().zip(seq.to_be_bytes()) {
            *n ^= s;
        }
        Some(nonce)
    }

    /// Move to the next generation of keys (KeyUpdate).
    pub fn update(&mut self) {
        *self = Self::new(to32(expand_label(&self.secret, "traffic upd", &[], 32)));
    }
}

/// Fill `buf` from the operating system's random source.
///
/// # Errors
/// When `/dev/urandom` cannot be read.
pub fn os_random(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_changes_and_xors_the_sequence() {
        let mut d = Direction::new([7; 32]);
        let n0 = d.next_nonce().unwrap();
        let n1 = d.next_nonce().unwrap();
        assert_ne!(n0, n1);
        assert_eq!(n0[..4], n1[..4]);
        assert_eq!(n0[11] ^ n1[11], 1);
    }

    #[test]
    fn update_changes_keys_and_resets_sequence() {
        let mut d = Direction::new([9; 32]);
        let first_key = *d.key();
        d.next_nonce().unwrap();
        d.update();
        assert_ne!(*d.key(), first_key);
        let fresh = Direction::new(to32(expand_label(&[9; 32], "traffic upd", &[], 32)));
        assert_eq!(d.key(), fresh.key());
        assert_eq!(d.seq, 0);
    }

    #[test]
    fn schedule_is_deterministic_and_separates_labels() {
        let hs = handshake_secret(&[1; 32]);
        assert_eq!(hs, handshake_secret(&[1; 32]));
        assert_ne!(hs, handshake_secret(&[2; 32]));
        let t = hash(b"transcript");
        assert_ne!(
            derive_secret(&hs, "c hs traffic", &t),
            derive_secret(&hs, "s hs traffic", &t)
        );
        assert_ne!(master_secret(&hs), hs);
    }
}
