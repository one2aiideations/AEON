//! Error type shared by the interface crate.

use std::fmt;

/// Failure of an interface operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Input failed validation.
    Invalid(String),
    /// Policy or role forbids the operation.
    Denied(String),
    /// A named object does not exist.
    NotFound(String),
    /// The object exists or was already decided.
    Conflict(String),
    /// A configured limit was exceeded.
    Limit(String),
    /// The operation is not legal in the current state.
    State(String),
    /// An application handler failed.
    Handler(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, msg) = match self {
            Self::Invalid(m) => ("invalid", m),
            Self::Denied(m) => ("denied", m),
            Self::NotFound(m) => ("not found", m),
            Self::Conflict(m) => ("conflict", m),
            Self::Limit(m) => ("limit", m),
            Self::State(m) => ("state", m),
            Self::Handler(m) => ("handler", m),
        };
        write!(f, "{kind}: {msg}")
    }
}

impl std::error::Error for Error {}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
