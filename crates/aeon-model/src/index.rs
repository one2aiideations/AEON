//! Named tensor table for one model file.

use crate::ModelError;
use aeon_tensor::{layout::checked_product, DType, MAX_RANK};

/// Maximum tensors in one index.
pub const MAX_TENSORS: usize = 4096;
/// Maximum tensor name length in bytes.
pub const NAME_MAX: usize = 95;

/// Dtypes allowed for stored model weights.
#[must_use]
pub fn is_model_dtype(d: DType) -> bool {
    matches!(
        d,
        DType::F32 | DType::BF16 | DType::F16 | DType::Fp8E4M3 | DType::I8
    )
}

/// Location and shape of one tensor inside a model file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorMeta {
    /// Unique name.
    pub name: String,
    /// Byte offset in the model file.
    pub offset: u64,
    /// Byte length.
    pub bytes: u64,
    /// Element type.
    pub dtype: DType,
    /// Dimensions.
    pub shape: Vec<u64>,
}

impl TensorMeta {
    /// Element count, checked.
    pub fn elements(&self) -> Result<u64, ModelError> {
        if self.shape.is_empty() || self.shape.len() > MAX_RANK {
            return Err(ModelError::BadShape);
        }
        let dims: Option<Vec<usize>> = self
            .shape
            .iter()
            .map(|&d| usize::try_from(d).ok())
            .collect();
        let dims = dims.ok_or(ModelError::BadShape)?;
        let n = checked_product(&dims).map_err(|_| ModelError::BadShape)?;
        u64::try_from(n).map_err(|_| ModelError::BadShape)
    }

    /// Bytes implied by shape and dtype.
    pub fn expected_bytes(&self) -> Result<u64, ModelError> {
        if !is_model_dtype(self.dtype) {
            return Err(ModelError::BadDType);
        }
        self.elements()?
            .checked_mul(self.dtype.size() as u64)
            .ok_or(ModelError::BadShape)
    }

    /// Check that `bytes` matches shape and dtype.
    pub fn validate_layout(&self) -> Result<(), ModelError> {
        if self.bytes == 0 || self.expected_bytes()? != self.bytes {
            return Err(ModelError::LayoutMismatch);
        }
        Ok(())
    }

    fn end(&self) -> Option<u64> {
        self.offset.checked_add(self.bytes)
    }
}

/// Table of tensors, unique by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelIndex {
    tensors: Vec<TensorMeta>,
}

impl ModelIndex {
    /// Empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tensor. Range checks against the file happen in [`Self::validate`].
    pub fn add(&mut self, meta: TensorMeta) -> Result<(), ModelError> {
        if self.tensors.len() >= MAX_TENSORS {
            return Err(ModelError::Full);
        }
        if meta.name.is_empty() || meta.name.len() > NAME_MAX || meta.name.contains('\0') {
            return Err(ModelError::BadName);
        }
        if !is_model_dtype(meta.dtype) {
            return Err(ModelError::BadDType);
        }
        if meta.shape.is_empty() || meta.shape.len() > MAX_RANK || meta.shape.contains(&0) {
            return Err(ModelError::BadShape);
        }
        if meta.bytes == 0 {
            return Err(ModelError::LayoutMismatch);
        }
        if self.find(&meta.name).is_some() {
            return Err(ModelError::DuplicateName);
        }
        self.tensors.push(meta);
        Ok(())
    }

    /// Check every tensor lies inside `model_bytes` and no two overlap.
    pub fn validate(&self, model_bytes: u64) -> Result<(), ModelError> {
        if model_bytes == 0 {
            return Err(ModelError::BadRange);
        }
        let mut spans: Vec<(u64, u64)> = Vec::with_capacity(self.tensors.len());
        for t in &self.tensors {
            let end = t.end().ok_or(ModelError::BadRange)?;
            if end > model_bytes {
                return Err(ModelError::BadRange);
            }
            spans.push((t.offset, end));
        }
        spans.sort_unstable();
        if spans
            .windows(2)
            .any(|w| w[1].0 < w[0].1 || w[1].0 == w[0].0)
        {
            return Err(ModelError::BadRange);
        }
        Ok(())
    }

    /// Look up by name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&TensorMeta> {
        self.tensors.iter().find(|t| t.name == name)
    }

    /// All tensors in insertion order.
    #[must_use]
    pub fn tensors(&self) -> &[TensorMeta] {
        &self.tensors
    }

    /// Number of tensors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tensors.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tensors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, offset: u64, bytes: u64) -> TensorMeta {
        TensorMeta {
            name: name.into(),
            offset,
            bytes,
            dtype: DType::I8,
            shape: vec![bytes],
        }
    }

    #[test]
    fn add_find_validate() {
        let mut ix = ModelIndex::new();
        ix.add(meta("a", 0, 4)).unwrap();
        ix.add(meta("b", 4, 4)).unwrap();
        assert_eq!(ix.find("b").unwrap().offset, 4);
        ix.validate(8).unwrap();
        assert_eq!(ix.validate(7), Err(ModelError::BadRange));
    }

    #[test]
    fn rejects_duplicates_and_overlap() {
        let mut ix = ModelIndex::new();
        ix.add(meta("a", 0, 4)).unwrap();
        assert_eq!(ix.add(meta("a", 8, 4)), Err(ModelError::DuplicateName));
        ix.add(meta("b", 3, 4)).unwrap();
        assert_eq!(ix.validate(100), Err(ModelError::BadRange));
    }

    #[test]
    fn layout_check() {
        let mut m = meta("w", 0, 8);
        m.dtype = DType::F32;
        m.shape = vec![2];
        m.validate_layout().unwrap();
        m.bytes = 9;
        assert_eq!(m.validate_layout(), Err(ModelError::LayoutMismatch));
        m.dtype = DType::I32;
        assert_eq!(m.expected_bytes(), Err(ModelError::BadDType));
    }
}
