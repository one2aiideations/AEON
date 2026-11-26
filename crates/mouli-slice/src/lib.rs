#![forbid(unsafe_code)]

//! # mouli-slice
//!
//! Domain-aware neuron profiling, deterministic MLP slicing, and quality
//! gating for MOULI.
//!
//! This crate operates on dense SwiGLU feed-forward layers:
//!
//! - `Wg`: row-major `[d_ff, d_model]`
//! - `Wu`: row-major `[d_ff, d_model]`
//! - `Wd`: row-major `[d_model, d_ff]`
//!
//! A neuron is removed consistently from the corresponding rows of `Wg`
//! and `Wu` and column of `Wd`.
//!
//! # Architecture
//!
//! The slicing logic has no dependency on the inference engine: the engine implements
//! [`ActivationSink`] and [`Scorer`]. Only [`gguf_slice`] reads and writes GGUF files, through
//! `mouli-llm`.
//!
//! The profiling process records activation statistics. The planning
//! process selects neurons. The slicing process transforms in-memory
//! f32 matrices. The gate evaluates full and sliced models on identical
//! held-out items.
//!
//! # Important limitations
//!
//! This crate does not execute a model, calibrate quantization, write
//! GGUF files (apart from [`gguf_slice`]), measure inference latency, or prove generalization.
//!
//! The engine side must re-quantize sliced matrices and write the resulting
//! GGUF file. Its `feed_forward_length` metadata must equal the number of
//! kept neurons. Under `Rule::Budget`, this number can differ between
//! layers. The engine must support per-layer feed-forward widths, or the
//! caller must use `Rule::Uniform`.
//!
//! Successful slicing proves only that the requested matrix transformation
//! was performed. It does not prove that the sliced model produces equivalent
//! outputs or retains its quality.
//!
//! Only scores actually returned by an engine implementation of [`Scorer`]
//! and evaluated by the gate constitute quality evidence. Such evidence
//! applies only to the supplied held-out items, not to arbitrary prompts,
//! other domains, or future inputs.
//!
//! The crate does not measure speed. Fewer neurons do not, by themselves,
//! establish a particular latency or memory improvement.

pub mod closure;
pub mod gate;
pub mod gguf_slice;
pub mod plan;
pub mod profile;
pub mod report;
pub mod run;
pub mod slice;

use std::error::Error;
use std::fmt;

/// Errors returned by MOULI slicing operations.
///
/// All malformed external input is reported through this type rather
/// than being silently accepted.
#[derive(Debug, Clone, PartialEq)]
pub enum SliceError {
    /// A matrix, vector, or collection has an incompatible shape.
    BadShape(&'static str),

    /// A parameter is outside its permitted range.
    BadParameter(&'static str),

    /// A supplied or computed value is not finite.
    NonFinite(&'static str),

    /// A requested layer does not exist.
    LayerOutOfRange {
        /// Requested layer index.
        layer: usize,
        /// Number of available layers.
        layers: usize,
    },

    /// An operation requires non-empty input.
    EmptyInput(&'static str),

    /// A requested quality tolerance was not satisfied.
    ToleranceNotMet {
        /// Reference score.
        full_score: f64,
        /// Candidate score.
        sliced_score: f64,
    },
}

impl fmt::Display for SliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadShape(message) => write!(f, "bad shape: {message}"),
            Self::BadParameter(parameter) => {
                write!(f, "invalid parameter: {parameter}")
            }
            Self::NonFinite(context) => {
                write!(f, "non-finite value: {context}")
            }
            Self::LayerOutOfRange { layer, layers } => {
                write!(f, "layer {layer} is out of range (layers: {layers})")
            }
            Self::EmptyInput(context) => {
                write!(f, "empty input: {context}")
            }
            Self::ToleranceNotMet {
                full_score,
                sliced_score,
            } => write!(
                f,
                "tolerance not met: full={full_score}, sliced={sliced_score}"
            ),
        }
    }
}

impl Error for SliceError {}

/// Receives the activation vector of each MLP layer for every processed
/// token.
///
/// The engine calls [`ActivationSink::observe`] once per layer per token.
/// The activation vector must contain exactly `d_ff` finite values.
///
/// Implementations must report malformed observations as errors.
pub trait ActivationSink {
    /// Records one layer's neuron activations.
    ///
    /// # Errors
    /// A [`SliceError`] for a bad layer, length or non-finite value.
    fn observe(&mut self, layer: usize, activations: &[f32]) -> Result<(), SliceError>;
}

/// Scores a model on held-out multiple-choice items.
///
/// The returned vector contains one score in `0..=1` per item.
/// A score of `1.0` means correct. The vector must use the same item
/// ordering when comparing full and sliced models.
///
/// This trait does not prescribe how the engine performs inference,
/// evaluates answers, or constructs held-out items.
pub trait Scorer {
    /// Scores all configured held-out items.
    ///
    /// # Errors
    /// A [`SliceError`] when the model cannot be scored.
    fn score_items(&mut self) -> Result<Vec<f64>, SliceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_implement_display_and_error() {
        let error = SliceError::BadShape("test");
        assert_eq!(error.to_string(), "bad shape: test");
        let _: &dyn Error = &error;
    }

    #[test]
    fn tolerance_error_displays_scores() {
        let error = SliceError::ToleranceNotMet {
            full_score: 0.9,
            sliced_score: 0.7,
        };
        assert!(error.to_string().contains("0.9"));
        assert!(error.to_string().contains("0.7"));
    }
}
