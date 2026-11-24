//! Hosted models: a streaming chat client for OpenAI-compatible endpoints, over MOULI's own
//! HTTPS, plus storage for the API key.
//!
//! The key is a [`Secret`]: never printed, never in a URL or request body, only sent in the
//! `Authorization` header over a verified TLS connection (plain `http` works only to loopback).
//! Error messages from servers have any echo of the key removed before they reach the caller.
//! This module only talks to a provider; whether a request may be sent, and what is washed out
//! of it first, is the job of the governed interface that calls it.
#![forbid(unsafe_code)]

pub mod chat;
pub mod keyfile;

pub use chat::{chat_stream, Provider, Secret};

/// Why a hosted request failed.
#[derive(Debug)]
pub enum Error {
    /// The request could not be made (network, TLS, certificate).
    Http(mouli_http::Error),
    /// The provider answered with an error status.
    Api {
        /// HTTP status.
        status: u16,
        /// The provider's message, with the key removed.
        message: String,
    },
    /// The reply was not in the expected format or ended early.
    BadReply(&'static str),
    /// The key or provider settings are unusable.
    Config(&'static str),
    /// Reading the reply failed part way.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "{e}"),
            Self::Api {
                status: 401 | 403, ..
            } => {
                write!(
                    f,
                    "the provider rejected the API key (check the key and the account)"
                )
            }
            Self::Api { status: 429, .. } => {
                write!(f, "the provider says: too many requests or no credit left")
            }
            Self::Api { status, message } => write!(f, "the provider returned {status}: {message}"),
            Self::BadReply(m) => write!(f, "unexpected reply from the provider: {m}"),
            Self::Config(m) => write!(f, "hosted model settings: {m}"),
            Self::Io(e) => write!(f, "connection error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<mouli_http::Error> for Error {
    fn from(e: mouli_http::Error) -> Self {
        Self::Http(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
