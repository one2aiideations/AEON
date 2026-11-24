//! MOULI gateway: how applications and agents outside the console use the governed interface.
//!
//! The console chat in `moulid` talks to the model directly. Anything else (a desktop shell, a
//! web front end, an agent in another process) connects to a Unix socket and speaks
//! newline-delimited JSON: one request per line, a stream of event lines back, so replies can
//! be shown as they are generated. Every request goes through
//! [`mouli_interface::Workspace`], so policy, approvals, data washing and audit apply to every
//! caller the same way.
//!
//! - [`json`]: a small JSON reader and writer (no external crate).
//! - [`gateway`]: the [`Gateway`] protocol engine and the [`Backend`] trait that supplies model
//!   replies.
//! - [`server`]: the Unix-socket listener (mode 0600) and connection loop.
//!
//! Trust model: the socket's file permissions decide who may connect; the `tenant`, `user` and
//! `role` in a request are taken as claimed by that caller. Binding them to authenticated
//! identities (see `mouli-auth` envelopes) is not done yet. The server handles one connection at
//! a time; no real front end or model backend has been attached, and nothing here has been run
//! outside tests.
#![forbid(unsafe_code)]

pub mod gateway;
pub mod json;
pub mod server;

pub use gateway::{Backend, Gateway};
