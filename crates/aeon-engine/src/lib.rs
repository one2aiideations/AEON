//! AEON inference engine.
//!
//! This crate replaces llama.cpp inside AEON. It starts with scalar CPU
//! reference kernels ([`ops`]); SIMD and Vulkan backends must match these
//! outputs within tolerance.
#![forbid(unsafe_code)]

pub mod ops;

use core::fmt;

/// Kernel errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpError {
    /// Slice lengths do not match the given dimensions.
    Shape,
    /// A parameter is out of range (zero size, odd head dim, bad epsilon).
    Param,
    /// The result is not finite.
    NonFinite,
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shape => "slice length does not match dimensions",
            Self::Param => "parameter out of range",
            Self::NonFinite => "non-finite result",
        })
    }
}

impl std::error::Error for OpError {}
