//! MOULI-SECURITY core logic: pure, deterministic building blocks with no database, network or
//! clock. Callers pass timestamps (seconds since the Unix epoch) and persist results themselves.
//!
//! * [`rules`]: boolean rule trees over JSON events (and / or / not plus 13 comparisons).
//! * [`detect`]: threshold, velocity, sequence, state-transition and statistical (z-score, MAD)
//!   anomaly detectors.
//! * [`scoring`]: inherent and residual risk scores and risk levels.
//! * [`cases`]: the case and investigation state machine with a tamper-evident event history.
//! * [`evidence`]: canonical hashing, integrity checks, bundles and legal-hold-aware retention.
//!
//! The ideas come from an earlier Python prototype; this is a clean re-implementation with the
//! prototype's defects removed, not a translation. MOULIONE remains the identity and
//! authorization authority: nothing here decides who may act.
#![forbid(unsafe_code)]

pub mod cases;
pub mod detect;
pub mod evidence;
pub mod rules;
pub mod scoring;
