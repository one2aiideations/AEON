//! Element types and row-major layouts.

use core::fmt;

/// Highest supported tensor rank.
pub const MAX_RANK: usize = 8;

/// Element type of a dense tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DType {
    /// 32-bit IEEE float.
    F32,
    /// bfloat16.
    BF16,
    /// 16-bit IEEE float.
    F16,
    /// 8-bit float, e4m3 encoding.
    Fp8E4M3,
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
}

impl DType {
    /// Bytes per element.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::F32 | Self::I32 => 4,
            Self::BF16 | Self::F16 | Self::I16 => 2,
            Self::Fp8E4M3 | Self::I8 => 1,
        }
    }

    /// Canonical upper-case name, as used in manifests.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::F32 => "F32",
            Self::BF16 => "BF16",
            Self::F16 => "F16",
            Self::Fp8E4M3 => "FP8_E4M3",
            Self::I8 => "I8",
            Self::I16 => "I16",
            Self::I32 => "I32",
        }
    }

    /// Parse a canonical name.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "F32" => Self::F32,
            "BF16" => Self::BF16,
            "F16" => Self::F16,
            "FP8_E4M3" => Self::Fp8E4M3,
            "I8" => Self::I8,
            "I16" => Self::I16,
            "I32" => Self::I32,
            _ => return None,
        })
    }
}

/// Errors from layout, tensor and view construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TensorError {
    /// Rank is 0 or above [`MAX_RANK`].
    BadRank,
    /// A dimension is zero.
    ZeroDim,
    /// Size arithmetic overflowed.
    Overflow,
    /// Axis or index out of range.
    OutOfRange,
    /// Buffer length does not match the layout.
    SizeMismatch,
}

impl fmt::Display for TensorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadRank => "tensor rank out of range",
            Self::ZeroDim => "tensor dimension is zero",
            Self::Overflow => "tensor size overflow",
            Self::OutOfRange => "axis or index out of range",
            Self::SizeMismatch => "buffer size does not match layout",
        })
    }
}

impl std::error::Error for TensorError {}

/// Shape, strides (in elements) and sizes of a tensor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    dtype: DType,
    shape: Vec<usize>,
    strides: Vec<usize>,
    elements: usize,
}

impl Layout {
    /// Contiguous row-major layout for `shape`.
    pub fn contiguous(dtype: DType, shape: &[usize]) -> Result<Self, TensorError> {
        if shape.is_empty() || shape.len() > MAX_RANK {
            return Err(TensorError::BadRank);
        }
        let elements = checked_product(shape)?;
        elements
            .checked_mul(dtype.size())
            .ok_or(TensorError::Overflow)?;
        let mut strides = vec![1usize; shape.len()];
        let mut acc = 1usize;
        for (s, &d) in strides.iter_mut().zip(shape).rev() {
            *s = acc;
            acc = acc.checked_mul(d).ok_or(TensorError::Overflow)?;
        }
        Ok(Self {
            dtype,
            shape: shape.to_vec(),
            strides,
            elements,
        })
    }

    /// Element type.
    #[must_use]
    pub fn dtype(&self) -> DType {
        self.dtype
    }

    /// Dimensions.
    #[must_use]
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }

    /// Strides in elements.
    #[must_use]
    pub fn strides(&self) -> &[usize] {
        &self.strides
    }

    /// Number of dimensions.
    #[must_use]
    pub fn rank(&self) -> usize {
        self.shape.len()
    }

    /// Number of elements.
    #[must_use]
    pub fn elements(&self) -> usize {
        self.elements
    }

    /// Number of bytes for a dense buffer (checked at construction).
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.elements * self.dtype.size()
    }

    /// Element offset of `indices`, or `None` if any index is out of range.
    #[must_use]
    pub fn offset_of(&self, indices: &[usize]) -> Option<usize> {
        if indices.len() != self.rank() {
            return None;
        }
        let mut off = 0usize;
        for ((&i, &n), &s) in indices.iter().zip(&self.shape).zip(&self.strides) {
            if i >= n {
                return None;
            }
            off = off.checked_add(i.checked_mul(s)?)?;
        }
        Some(off)
    }

    /// Number of elements between the first and last addressed element, plus one.
    pub fn span_elements(&self) -> Result<usize, TensorError> {
        let mut span = 1usize;
        for (&n, &s) in self.shape.iter().zip(&self.strides) {
            if n > 1 {
                let axis = (n - 1).checked_mul(s).ok_or(TensorError::Overflow)?;
                span = span.checked_add(axis).ok_or(TensorError::Overflow)?;
            }
        }
        Ok(span)
    }

    pub(crate) fn with_axis_fixed(&self, axis: usize) -> Self {
        let mut out = self.clone();
        out.elements /= out.shape[axis];
        out.shape[axis] = 1;
        out
    }
}

/// Product of `dims`, rejecting zero and overflow.
pub fn checked_product(dims: &[usize]) -> Result<usize, TensorError> {
    dims.iter().try_fold(1usize, |acc, &d| {
        if d == 0 {
            Err(TensorError::ZeroDim)
        } else {
            acc.checked_mul(d).ok_or(TensorError::Overflow)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_major_strides() {
        let l = Layout::contiguous(DType::F32, &[2, 3, 4]).unwrap();
        assert_eq!(l.strides(), &[12, 4, 1]);
        assert_eq!(l.elements(), 24);
        assert_eq!(l.bytes(), 96);
        assert_eq!(l.offset_of(&[1, 2, 3]), Some(23));
        assert_eq!(l.offset_of(&[2, 0, 0]), None);
    }

    #[test]
    fn rejects_bad_shapes() {
        assert_eq!(Layout::contiguous(DType::I8, &[]), Err(TensorError::BadRank));
        assert_eq!(
            Layout::contiguous(DType::I8, &[1; 9]),
            Err(TensorError::BadRank)
        );
        assert_eq!(
            Layout::contiguous(DType::I8, &[3, 0]),
            Err(TensorError::ZeroDim)
        );
        assert_eq!(
            Layout::contiguous(DType::F32, &[usize::MAX / 2, 3]),
            Err(TensorError::Overflow)
        );
    }

    #[test]
    fn dtype_names_round_trip() {
        for d in [
            DType::F32,
            DType::BF16,
            DType::F16,
            DType::Fp8E4M3,
            DType::I8,
            DType::I16,
            DType::I32,
        ] {
            assert_eq!(DType::from_name(d.name()), Some(d));
        }
        assert_eq!(DType::from_name("Q4"), None);
    }
}
