//! MOULI-AUTH / MOULIONE core: the enterprise authority for identity, tenancy, authorisation,
//! approvals and audit, rebuilt in Rust from the MOULI-AUTH specification and the MOULIONE and
//! AgentBox boundary document.
//!
//! The MOULIONE repository itself was not available (the source tree holds only specifications),
//! so nothing here is derived from its code and **nothing is verified against the real MOULIONE**.
//! In particular the envelope scheme is HMAC-SHA256, not the JWKS-style signature MOULIONE is
//! documented to use; see [`envelope`].
//!
//! - [`org`]: tenant > company > plant hierarchy.
//! - [`principal`]: people, services and agents.
//! - [`policy`]: deny-by-default RBAC with attribute, scope and amount conditions.
//! - [`makerchecker`]: tiered approvals with segregation of duties and escalation.
//! - [`envelope`]: signed, short-lived, audience-bound authorisation envelopes with replay and
//!   revocation checks.
//! - [`authority`]: [`Authority`], which decides, audits and issues envelopes.
//!
//! Not here: login, MFA, SSO, sessions, persistence (`mouli-data`), policy lifecycle and
//! administrative APIs, and any network service.

pub mod authority;
pub mod envelope;
pub mod makerchecker;
pub mod org;
pub mod policy;
pub mod principal;

pub use authority::{Authority, Authorization};
pub use envelope::{Claims, Verifier};
