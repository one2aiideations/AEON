//! MOULI theory layer, from "Towards a Mathematical Theory of Intelligence".
//!
//! MOULI uses these measures to choose, route and merge models by how they
//! behave under shift, not by a single familiar benchmark number.
//!
//! * [`robust`]: closure conditions, robust score S (worst case), shortcut
//!   reliance and the gap law that predicts accuracy under shift.
//! * [`calibration`]: ECE, Brier score and the self-knowledge loss.
//! * [`cascade`]: cheapest-first two-model cascades and their Pareto frontier.
//! * [`select`]: leaderboard ranking and robust choice of a merge weight.
//! * [`merge`]: task-vector screening and TIES / linear merging.
//!
//! Rule kept from the research engine: nothing is reported as measured
//! unless it was measured; these functions only transform measurements.
#![forbid(unsafe_code)]

pub mod calibration;
pub mod cascade;
pub mod merge;
pub mod robust;
pub mod select;

/// Options per multiple-choice item used throughout (A-D).
pub const K: usize = 4;
