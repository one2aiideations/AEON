//! MOULI's own cryptographic primitives, the base of the in-house TLS client that lets the OS
//! talk to hosted models without an external TLS library.
//!
//! What is here, each checked against the published test vectors (RFC 4231, 5869, 7748, 8439)
//! and against an independent implementation (see the tests):
//!
//! - [`hmac`]: HMAC-SHA-256, HKDF extract and expand, and the TLS 1.3 `HKDF-Expand-Label`.
//! - [`chacha20`]: the ChaCha20 stream cipher.
//! - [`poly1305`]: the Poly1305 one-time authenticator.
//! - [`aead`]: ChaCha20-Poly1305 authenticated encryption (RFC 8439).
//! - [`x25519`]: Diffie-Hellman on Curve25519 (RFC 7748).
//!
//! SHA-256 itself comes from the `sha2` crate already used across the workspace.
//!
//! Honest limits: this is hand-written cryptography. Test vectors prove it computes the right
//! answers; they do not prove it is free of side channels. Arithmetic is written without
//! secret-dependent branches or table lookups, but that has not been audited or measured, and
//! it should get an independent review before it protects anything valuable. Not here yet:
//! AES-GCM, ECDSA, RSA, certificate parsing and the TLS protocol itself.
#![forbid(unsafe_code)]

pub mod aead;
pub mod chacha20;
pub mod hmac;
pub mod poly1305;
pub mod x25519;

/// Compare two byte strings without stopping at the first difference.
#[must_use]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Decode lower- or upper-case hex (test helper, also handy for key files).
///
/// # Errors
/// `None` for an odd length or a non-hex character.
#[must_use]
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.chunks(2)
        .map(|p| {
            let hi = (p[0] as char).to_digit(16)?;
            let lo = (p[1] as char).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd") && !ct_eq(b"abc", b"ab") && ct_eq(b"", b""));
    }

    #[test]
    fn hex_decoding() {
        assert_eq!(from_hex("00ff 10").unwrap(), [0, 255, 16]);
        assert!(from_hex("abc").is_none() && from_hex("zz").is_none());
    }
}
