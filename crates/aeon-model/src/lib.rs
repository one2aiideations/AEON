//! AEON model storage layer.
//!
//! * [`index`]: named tensor table with overlap and bounds checks.
//! * [`manifest`]: text manifest (`name | offset | bytes | dtype | rank | d0,d1,..`).
//! * [`source`]: random-access byte sources (memory, file) and a bounded stream.
//! * [`binding`]: a cursor that can only read inside one tensor's bytes.
//! * [`cache`]: byte-budgeted LRU cache of fully loaded tensors.
//! * [`experts`]: tiered (fast / RAM LRU / disk) mixture-of-experts store and routing.
//!
//! Ported from the retired C core (`aeon_model_*`, `aeon_tensor_binding`,
//! `aeon_tensor_layout`, `aeon_expert_store`).
#![forbid(unsafe_code)]

pub mod binding;
pub mod cache;
pub mod experts;
pub mod index;
pub mod manifest;
pub mod source;

use core::fmt;

/// Errors from the model storage layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// Tensor name empty or too long.
    BadName,
    /// Tensor name already present.
    DuplicateName,
    /// Unsupported or unknown dtype.
    BadDType,
    /// Rank or shape invalid.
    BadShape,
    /// Declared byte count does not match shape and dtype.
    LayoutMismatch,
    /// Range outside the model or overlapping another tensor.
    BadRange,
    /// Index is full.
    Full,
    /// Tensor not found.
    NotFound,
    /// Manifest line malformed (1-based line number).
    Manifest(usize),
    /// Manifest has no records.
    EmptyManifest,
    /// Read would go past the end of the source or tensor.
    OutOfBounds,
    /// Underlying I/O failure.
    Io(String),
    /// Expert key outside the configured layers/experts.
    BadExpert,
    /// Expert is masked out.
    Denied,
    /// Every slot is pinned.
    NoSlot,
    /// Bad configuration or arguments.
    BadConfig,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadName => f.write_str("bad tensor name"),
            Self::DuplicateName => f.write_str("duplicate tensor name"),
            Self::BadDType => f.write_str("unsupported dtype"),
            Self::BadShape => f.write_str("bad tensor shape"),
            Self::LayoutMismatch => f.write_str("byte count does not match shape"),
            Self::BadRange => f.write_str("tensor range out of bounds or overlapping"),
            Self::Full => f.write_str("model index full"),
            Self::NotFound => f.write_str("tensor not found"),
            Self::Manifest(line) => write!(f, "manifest line {line} malformed"),
            Self::EmptyManifest => f.write_str("manifest has no records"),
            Self::OutOfBounds => f.write_str("read out of bounds"),
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::BadExpert => f.write_str("expert key out of range"),
            Self::Denied => f.write_str("expert not allowed"),
            Self::NoSlot => f.write_str("no evictable expert slot"),
            Self::BadConfig => f.write_str("bad configuration"),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<std::io::Error> for ModelError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}
