//! AEON's own certificate handling for the TLS client.
//!
//! Everything here works on public data (certificates, signatures, public keys), so none of it
//! needs to be constant time. Verification is strict: anything unexpected is an error.
#![forbid(unsafe_code)]

pub mod bignum;
pub mod der;
pub mod ecdsa;
pub mod pem;
pub mod rsa;
pub mod verify;
pub mod x509;

use sha2::{Digest, Sha256, Sha384, Sha512};

/// Hash functions allowed in signatures. SHA-1 and MD5 are deliberately absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hash {
    /// SHA-256.
    Sha256,
    /// SHA-384.
    Sha384,
    /// SHA-512.
    Sha512,
}

impl Hash {
    /// Digest length in bytes.
    #[must_use]
    pub fn len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    /// Never empty; present for the usual pairing with `len`.
    #[must_use]
    pub fn is_empty(self) -> bool {
        false
    }

    /// Hash `data`.
    #[must_use]
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha384 => Sha384::digest(data).to_vec(),
            Self::Sha512 => Sha512::digest(data).to_vec(),
        }
    }

    /// The PKCS#1 `DigestInfo` encoding of `digest`.
    #[must_use]
    pub fn digest_info(self, digest: &[u8]) -> Vec<u8> {
        let prefix: &[u8] = match self {
            Self::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            Self::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            Self::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
        };
        let mut v = prefix.to_vec();
        v.extend_from_slice(digest);
        v
    }
}

pub(crate) use aeon_crypto::ct_eq;

/// Why certificate handling failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Malformed DER.
    Der(&'static str),
    /// Valid but not something this client accepts.
    Unsupported(&'static str),
    /// A public key that fails validation.
    BadKey(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Der(m) => write!(f, "bad DER: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported: {m}"),
            Self::BadKey(m) => write!(f, "bad key: {m}"),
        }
    }
}

impl std::error::Error for Error {}
