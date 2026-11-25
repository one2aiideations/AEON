//! Weight analysis for compression planning.
//!
//! Model-independent numerics normalised from the 55D research engine. Each function
//! takes plain `f32` slices, so it works on tensors read by `mouli-llm::gguf` or produced
//! anywhere else. These functions *measure*: none changes a model, and a plan (for
//! example a pruning threshold) is not evidence that the planned model still works.
//!
//! Differences from the Python original, all deliberate:
//!
//! * Row sampling in [`stats::near_duplicate_fraction`] is evenly strided, not random,
//!   so results are reproducible without an RNG.
//! * Singular values come from a Jacobi eigen-solver on the Gram matrix, so large
//!   matrices are cropped to `max_dim` (default [`lowrank::DEFAULT_MAX_DIM`]) before
//!   analysis; the original cropped to 2048 using LAPACK.

pub mod lowrank;
pub mod prune;
pub mod quant;
pub mod rvq;
pub mod stats;

use std::fmt;

/// Invalid input to an analysis function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisError {
    /// `data.len()` is not `rows * cols`, or a dimension is zero.
    BadShape,
    /// A parameter is out of range (the text says which).
    BadParameter(&'static str),
    /// The input contains NaN or infinity.
    NonFinite,
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnalysisError::BadShape => f.write_str("data length does not match the shape"),
            AnalysisError::BadParameter(p) => write!(f, "invalid parameter: {p}"),
            AnalysisError::NonFinite => f.write_str("input contains NaN or infinity"),
        }
    }
}

impl std::error::Error for AnalysisError {}

/// A row-major matrix view.
#[derive(Debug, Clone, Copy)]
pub struct Matrix<'a> {
    /// Row-major elements.
    pub data: &'a [f32],
    /// Row count (at least 1).
    pub rows: usize,
    /// Column count (at least 1).
    pub cols: usize,
}

impl<'a> Matrix<'a> {
    /// Validate and wrap a row-major buffer.
    pub fn new(data: &'a [f32], rows: usize, cols: usize) -> Result<Self, AnalysisError> {
        if rows == 0 || cols == 0 || rows.checked_mul(cols) != Some(data.len()) {
            return Err(AnalysisError::BadShape);
        }
        if data.iter().any(|x| !x.is_finite()) {
            return Err(AnalysisError::NonFinite);
        }
        Ok(Matrix { data, rows, cols })
    }

    /// One row.
    pub fn row(&self, r: usize) -> &'a [f32] {
        &self.data[r * self.cols..(r + 1) * self.cols]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_validates_shape_and_values() {
        assert!(Matrix::new(&[1.0, 2.0, 3.0, 4.0], 2, 2).is_ok());
        assert_eq!(
            Matrix::new(&[1.0; 3], 2, 2).unwrap_err(),
            AnalysisError::BadShape
        );
        assert_eq!(Matrix::new(&[], 0, 2).unwrap_err(), AnalysisError::BadShape);
        assert_eq!(
            Matrix::new(&[1.0, f32::NAN], 1, 2).unwrap_err(),
            AnalysisError::NonFinite
        );
        assert_eq!(
            Matrix::new(&[1.0, f32::INFINITY], 1, 2).unwrap_err(),
            AnalysisError::NonFinite
        );
        let m = Matrix::new(&[1.0, 2.0, 3.0, 4.0], 2, 2).unwrap();
        assert_eq!(m.row(1), &[3.0, 4.0]);
    }
}
