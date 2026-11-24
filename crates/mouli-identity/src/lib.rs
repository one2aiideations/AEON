//! MOULI identity layer.
//!
//! Semantic messages are subject/predicate/object triples. A message may
//! leave or enter the machine only when:
//!
//! 1. its SHA-256 digest matches the triple,
//! 2. the sender's DID is registered, active and human-verified,
//! 3. an Ed25519 signature over the digest verifies with the registered key,
//! 4. a human-presence ("friction") challenge was answered with a signed
//!    attestation bound to the same context,
//! 5. it carries a credential hash and a content address (CID).
//!
//! Modules: [`registry`], [`semantic`], [`friction`], [`wire`], [`gate`], [`hooks`].
//! Ported from the retired C core (`mouli_identity*`, `mouli_semantic*`,
//! `mouli_friction`, `mouli_syscall`, `mouli_io_gate`, `mouli_process_gate`,
//! `mouli_net_policy`).
#![forbid(unsafe_code)]

pub mod friction;
pub mod gate;
pub mod hooks;
pub mod registry;
pub mod semantic;
pub mod wire;

use core::fmt;

/// Identity-layer errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdError {
    /// A field is empty, too long or contains NUL.
    BadField,
    /// Registry is full.
    Full,
    /// DID already registered.
    Duplicate,
    /// Record is not active or not human-verified.
    NotEligible,
    /// DID unknown or key mismatch.
    UnknownIdentity,
    /// Digest does not match the triple.
    Digest,
    /// Signature invalid.
    Signature,
    /// Friction challenge missing, mismatched or unsigned.
    Friction,
    /// Credential hash missing.
    NoCredential,
    /// Content address missing.
    NoCid,
    /// Wire bytes malformed.
    Malformed,
    /// Process table full.
    TooManyProcesses,
    /// Bad argument.
    BadArg,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadField => "bad field",
            Self::Full => "registry full",
            Self::Duplicate => "duplicate identity",
            Self::NotEligible => "identity not active or not human-verified",
            Self::UnknownIdentity => "unknown identity or key mismatch",
            Self::Digest => "digest mismatch",
            Self::Signature => "invalid signature",
            Self::Friction => "human-presence check failed",
            Self::NoCredential => "missing credential hash",
            Self::NoCid => "missing content address",
            Self::Malformed => "malformed wire data",
            Self::TooManyProcesses => "process table full",
            Self::BadArg => "bad argument",
        })
    }
}

impl std::error::Error for IdError {}

/// Longest DID, CID or triple field in bytes.
pub const FIELD_MAX: usize = 255;

pub(crate) fn check_field(s: &str) -> Result<(), IdError> {
    if s.is_empty() || s.len() > FIELD_MAX || s.contains('\0') {
        Err(IdError::BadField)
    } else {
        Ok(())
    }
}
