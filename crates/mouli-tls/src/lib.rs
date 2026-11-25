//! MOULI's own TLS 1.3 client.
//!
//! One cipher suite (`TLS_CHACHA20_POLY1305_SHA256`), one key exchange (X25519), and strict
//! certificate checking through `mouli-pki`: the chain must lead to a trusted root, the host
//! name must match, and the server's handshake signature must verify. No session resumption,
//! no 0-RTT, no client certificates. A server that supports only AES-GCM cannot be reached.
//!
//! The cryptography is hand-written in `mouli-crypto`: checked against published and OpenSSL
//! vectors, not audited for side channels.
#![forbid(unsafe_code)]

mod conn;
mod keys;
mod msg;

pub use conn::{Config, Connection};
pub use keys::os_random;

use mouli_pki::verify::VerifyError;

/// Why a TLS connection failed.
#[derive(Debug)]
pub enum Error {
    /// The transport failed.
    Io(std::io::Error),
    /// The peer broke the protocol.
    Protocol(&'static str),
    /// The peer sent a fatal alert (its description code).
    Alert(u8),
    /// The peer offered something this client does not implement.
    Unsupported(&'static str),
    /// The server's certificate was refused.
    Certificate(VerifyError),
    /// The server's handshake signature or Finished message did not verify.
    BadSignature(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "connection error: {e}"),
            Self::Protocol(m) => write!(f, "TLS protocol error: {m}"),
            Self::Alert(c) => write!(f, "the server sent a TLS alert ({c})"),
            Self::Unsupported(m) => write!(f, "not supported: {m}"),
            Self::Certificate(e) => write!(f, "certificate refused: {e}"),
            Self::BadSignature(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            other => Self::other(other.to_string()),
        }
    }
}
