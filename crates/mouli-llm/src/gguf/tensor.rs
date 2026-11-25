//! Tensor descriptors.
#![forbid(unsafe_code)]

use crate::gguf::error::GgufError;
use crate::gguf::types::GgmlType;
use std::ops::Range;

/// One tensor's description from the GGUF tensor info table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorInfo {
    /// Unique, non-empty name.
    pub name: String,
    /// Dimensions, 1 to 4 entries, each at least 1. `dims[0]` is the contiguous one.
    pub dims: Vec<u64>,
    /// Element type.
    pub ty: GgmlType,
    /// Offset of the data from the start of the tensor data section.
    pub offset: u64,
    /// Total element count (product of `dims`).
    pub n_elements: u64,
    /// Data size in bytes.
    pub n_bytes: u64,
}

impl TensorInfo {
    /// Absolute byte range of this tensor's data, given where the data section starts.
    pub fn data_range(&self, data_start: u64) -> Result<Range<u64>, GgufError> {
        let start = data_start
            .checked_add(self.offset)
            .ok_or(GgufError::Overflow)?;
        let end = start.checked_add(self.n_bytes).ok_or(GgufError::Overflow)?;
        Ok(start..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(offset: u64, n_bytes: u64) -> TensorInfo {
        TensorInfo {
            name: "t".into(),
            dims: vec![4],
            ty: GgmlType::F32,
            offset,
            n_elements: 4,
            n_bytes,
        }
    }

    #[test]
    fn range_adds_start_offset_and_size() {
        assert_eq!(info(64, 16).data_range(128).unwrap(), 192..208);
        assert_eq!(info(0, 16).data_range(0).unwrap(), 0..16);
    }

    #[test]
    fn range_overflow_is_an_error() {
        assert!(matches!(
            info(u64::MAX, 1).data_range(1),
            Err(GgufError::Overflow)
        ));
        assert!(matches!(
            info(u64::MAX - 1, 16).data_range(0),
            Err(GgufError::Overflow)
        ));
    }
}
