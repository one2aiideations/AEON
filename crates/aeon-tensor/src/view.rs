//! Owned tensors and borrowed, sliceable views.

use crate::layout::{Layout, TensorError};

/// A dense tensor that owns its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tensor {
    layout: Layout,
    data: Vec<u8>,
}

impl Tensor {
    /// Zero-filled tensor with the given layout.
    #[must_use]
    pub fn zeroed(layout: Layout) -> Self {
        let data = vec![0u8; layout.bytes()];
        Self { layout, data }
    }

    /// Wrap existing bytes; the length must equal `layout.bytes()`.
    pub fn from_bytes(layout: Layout, data: Vec<u8>) -> Result<Self, TensorError> {
        if data.len() != layout.bytes() {
            return Err(TensorError::SizeMismatch);
        }
        Ok(Self { layout, data })
    }

    /// Layout.
    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Raw bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Mutable raw bytes.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Set every byte to zero.
    pub fn zero(&mut self) {
        self.data.fill(0);
    }

    /// Full view over this tensor.
    #[must_use]
    pub fn view(&self) -> TensorView<'_> {
        TensorView {
            layout: self.layout.clone(),
            byte_offset: 0,
            owner: &self.data,
        }
    }
}

/// A borrowed window into a tensor's bytes.
///
/// The view keeps a reference to the whole owner buffer plus a byte offset,
/// so its bounds can always be re-checked.
#[derive(Debug, Clone)]
pub struct TensorView<'a> {
    layout: Layout,
    byte_offset: usize,
    owner: &'a [u8],
}

impl<'a> TensorView<'a> {
    /// Layout of the view (strides are those of the owner).
    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Byte offset of the first element inside the owner buffer.
    #[must_use]
    pub fn byte_offset(&self) -> usize {
        self.byte_offset
    }

    /// Fix `axis` at `index`, keeping rank (that axis becomes size 1).
    pub fn slice(&self, axis: usize, index: usize) -> Result<Self, TensorError> {
        let shape = self.layout.shape();
        if axis >= shape.len() || index >= shape[axis] {
            return Err(TensorError::OutOfRange);
        }
        let item = self.layout.dtype().size();
        let delta = index
            .checked_mul(self.layout.strides()[axis])
            .and_then(|e| e.checked_mul(item))
            .and_then(|b| b.checked_add(self.byte_offset))
            .ok_or(TensorError::Overflow)?;
        let out = Self {
            layout: self.layout.with_axis_fixed(axis),
            byte_offset: delta,
            owner: self.owner,
        };
        out.validate()?;
        Ok(out)
    }

    /// Check that every addressed element lies inside the owner buffer.
    pub fn validate(&self) -> Result<(), TensorError> {
        let span = self
            .layout
            .span_elements()?
            .checked_mul(self.layout.dtype().size())
            .ok_or(TensorError::Overflow)?;
        if self.byte_offset > self.owner.len() || span > self.owner.len() - self.byte_offset {
            return Err(TensorError::OutOfRange);
        }
        Ok(())
    }

    /// Bytes of the element at `indices` (relative to this view).
    #[must_use]
    pub fn element(&self, indices: &[usize]) -> Option<&'a [u8]> {
        let item = self.layout.dtype().size();
        let start = self
            .layout
            .offset_of(indices)?
            .checked_mul(item)?
            .checked_add(self.byte_offset)?;
        self.owner.get(start..start.checked_add(item)?)
    }

    /// Bytes from the first element to the end of the view's span.
    pub fn span_bytes(&self) -> Result<&'a [u8], TensorError> {
        self.validate()?;
        let len = self.layout.span_elements()? * self.layout.dtype().size();
        Ok(&self.owner[self.byte_offset..self.byte_offset + len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::DType;

    fn counting(shape: &[usize]) -> Tensor {
        let l = Layout::contiguous(DType::I8, shape).unwrap();
        let data = (0..l.bytes()).map(|i| i as u8).collect();
        Tensor::from_bytes(l, data).unwrap()
    }

    #[test]
    fn slice_rows_and_columns() {
        let t = counting(&[3, 4]);
        let row = t.view().slice(0, 2).unwrap();
        assert_eq!(row.layout().shape(), &[1, 4]);
        assert_eq!(row.span_bytes().unwrap(), &[8, 9, 10, 11]);
        let col = t.view().slice(1, 1).unwrap();
        assert_eq!(col.element(&[2, 0]), Some(&[9u8][..]));
        assert_eq!(col.layout().elements(), 3);
    }

    #[test]
    fn slice_out_of_range() {
        let t = counting(&[2, 2]);
        assert_eq!(t.view().slice(2, 0).unwrap_err(), TensorError::OutOfRange);
        assert_eq!(t.view().slice(0, 2).unwrap_err(), TensorError::OutOfRange);
    }

    #[test]
    fn from_bytes_checks_len() {
        let l = Layout::contiguous(DType::F32, &[2]).unwrap();
        assert_eq!(
            Tensor::from_bytes(l, vec![0; 7]).unwrap_err(),
            TensorError::SizeMismatch
        );
    }

    #[test]
    fn zero_clears() {
        let mut t = counting(&[4]);
        t.zero();
        assert!(t.bytes().iter().all(|&b| b == 0));
    }
}
