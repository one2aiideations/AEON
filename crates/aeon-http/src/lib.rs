//! AEON's own HTTP/1.1 client.
//!
//! One request per connection (`Connection: close`), strict response parsing (no bare line
//! feeds, no folded headers, conflicting lengths refused), bodies read as a stream whether they
//! are fixed-length, chunked or run until close. Redirects are not followed; the caller decides.
//! `https` goes through `aeon-tls` with certificate checking; plain `http` is allowed only to
//! loopback addresses.
#![forbid(unsafe_code)]

mod client;
mod message;
mod url;

pub use client::Client;
pub use message::{send, Body, Request, Response};
pub use url::{Scheme, Url};

/// Why a request failed.
#[derive(Debug)]
pub enum Error {
    /// The URL is not usable.
    Url(&'static str),
    /// A header or request line contains forbidden characters.
    BadRequest(&'static str),
    /// The transport failed.
    Io(std::io::Error),
    /// TLS failed (certificate, handshake, record).
    Tls(aeon_tls::Error),
    /// The server's response is malformed.
    Protocol(&'static str),
    /// The response head is larger than allowed.
    TooLarge,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(m) => write!(f, "bad URL: {m}"),
            Self::BadRequest(m) => write!(f, "bad request: {m}"),
            Self::Io(e) => write!(f, "network error: {e}"),
            Self::Tls(e) => write!(f, "{e}"),
            Self::Protocol(m) => write!(f, "bad HTTP response: {m}"),
            Self::TooLarge => write!(f, "HTTP response head too large"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<aeon_tls::Error> for Error {
    fn from(e: aeon_tls::Error) -> Self {
        Self::Tls(e)
    }
}
