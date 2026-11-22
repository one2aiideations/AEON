//! AEON cognitive primitives.
//!
//! Small, allocation-light building blocks the agent runtime composes:
//!
//! * [`vsa`]: 2048-bit binary hypervectors (bind, bundle, permute, distance)
//!   and bipolar cleanup memory.
//! * [`snn`]: Izhikevich spiking population with neuromodulator traces.
//! * [`inference`]: free-energy bookkeeping and active-inference action choice.
//! * [`causal`]: weighted cause -> effect graph with a `do()` query.
//! * [`memory`]: ring-buffer episodic memory with EWC-style consolidation.
//! * [`workspace`]: global workspace (bounded, salience-ranked broadcast).
//!
//! Ported from the retired C core (`aeon_cognitive_core`, `aeon_cognitive_engine`).
#![forbid(unsafe_code)]

pub mod causal;
pub mod inference;
pub mod memory;
pub mod snn;
pub mod vsa;
pub mod workspace;

use core::fmt;

/// Errors for invalid inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CogError {
    /// Non-finite or out-of-range number.
    BadValue,
    /// Container is full.
    Full,
    /// Nothing to select from.
    Empty,
    /// Slice lengths disagree.
    Shape,
}

impl fmt::Display for CogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadValue => "value out of range",
            Self::Full => "container full",
            Self::Empty => "empty",
            Self::Shape => "length mismatch",
        })
    }
}

impl std::error::Error for CogError {}

pub(crate) fn finite(x: f32) -> Result<f32, CogError> {
    if x.is_finite() {
        Ok(x)
    } else {
        Err(CogError::BadValue)
    }
}

pub(crate) fn non_negative(x: f32) -> Result<f32, CogError> {
    if x.is_finite() && x >= 0.0 {
        Ok(x)
    } else {
        Err(CogError::BadValue)
    }
}
