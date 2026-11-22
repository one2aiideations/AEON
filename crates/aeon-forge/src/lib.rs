//! AEON agent forge: create, version, mutate and evaluate agents.
//!
//! Normalised to Rust from the AgentBox Python backend (registry, candidate
//! generation, evaluation scoring). Three rules carry over unchanged:
//!
//! 1. Agent versions are immutable: the same version with different content
//!    is a conflict, never an overwrite.
//! 2. Candidate generation uses a small registered set of bounded operators and
//!    never touches identity, permissions or tools.
//! 3. Evaluation is declarative and deterministic: cases, assertions and
//!    weights are data, and scoring runs no agent code.
//!
//! Clocks are not read here: callers pass timestamps, so every function is
//! deterministic and testable.

pub mod candidate;
pub mod evaluation;
pub mod manifest;
pub mod registry;

pub use candidate::{generate, Candidate, CandidateError, Operator};
pub use evaluation::{
    score, Assertion, Case, EvalError, Gates, Profile, Score, Scoring, Submitted, Value,
};
pub use manifest::{Manifest, ManifestError, Origin};
pub use registry::{Record, Registry, RegistryError};
