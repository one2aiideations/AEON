//! Dequantisation of GGUF tensor data to `f32`.
//!
//! Written from the published GGUF block layouts, not from any other implementation.
//! Supported element types: F32, F16, BF16, Q4_0, Q4_1, Q5_0, Q5_1, Q8_0, Q4_K, Q5_K, Q6_K.
//! Other types return [`QuantError::Unsupported`].
//!
//! **Verification status:** the tests build blocks by hand from the layout description and
//! compare with values computed directly from the formulas. They have *not* been compared
//! with output from a reference engine on real model files; engine milestone E1 requires that
//! comparison against the oracle before these routines are trusted for inference.
//!
//! All functions are safe, bounds-checked and panic-free on arbitrary input.

mod float;
mod kquant;
mod legacy;

use aeon_gguf::types::GgmlType;
use std::fmt;

pub use float::{bf16_to_f32, f16_to_f32};

/// Dequantisation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantError {
    /// The element type is not implemented here.
    Unsupported(GgmlType),
    /// `n` is not a whole number of blocks for the type.
    NotBlockAligned,
    /// The byte slice length does not match `n` elements of this type.
    WrongByteLength {
        /// Bytes required.
        expected: usize,
        /// Bytes supplied.
        found: usize,
    },
    /// The output slice does not hold exactly `n` elements.
    WrongOutputLength,
    /// A size computation overflowed.
    Overflow,
}

impl fmt::Display for QuantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QuantError::Unsupported(t) => {
                write!(f, "dequantisation of {} is not supported", t.name())
            }
            QuantError::NotBlockAligned => {
                f.write_str("element count is not a multiple of the block size")
            }
            QuantError::WrongByteLength { expected, found } => {
                write!(f, "expected {expected} bytes, got {found}")
            }
            QuantError::WrongOutputLength => {
                f.write_str("output length does not match the element count")
            }
            QuantError::Overflow => f.write_str("size computation overflowed"),
        }
    }
}

impl std::error::Error for QuantError {}

/// Pass-through that keeps block sizes out of constant position, so slicing loops stay generic over
/// the block size (and clippy does not ask for `as_chunks` on toolchains that predate it).
#[inline]
pub(crate) fn sz(n: usize) -> usize {
    n
}

/// `true` if [`dequantize`] supports `ty`.
pub fn is_supported(ty: GgmlType) -> bool {
    matches!(
        ty,
        GgmlType::F32
            | GgmlType::F16
            | GgmlType::BF16
            | GgmlType::Q4_0
            | GgmlType::Q4_1
            | GgmlType::Q5_0
            | GgmlType::Q5_1
            | GgmlType::Q8_0
            | GgmlType::Q4_K
            | GgmlType::Q5_K
            | GgmlType::Q6_K
    )
}

/// Bytes occupied by `n` elements of `ty`.
pub fn byte_len(ty: GgmlType, n: usize) -> Result<usize, QuantError> {
    let block = usize::try_from(ty.block_size()).map_err(|_| QuantError::Overflow)?;
    if block == 0 || !n.is_multiple_of(block) {
        return Err(QuantError::NotBlockAligned);
    }
    let size = usize::try_from(ty.type_size()).map_err(|_| QuantError::Overflow)?;
    (n / block).checked_mul(size).ok_or(QuantError::Overflow)
}

/// Decode `n = out.len()` elements of type `ty` from `bytes` into `out`.
///
/// `bytes` must be exactly [`byte_len`]`(ty, out.len())` long.
pub fn dequantize(ty: GgmlType, bytes: &[u8], out: &mut [f32]) -> Result<(), QuantError> {
    if !is_supported(ty) {
        return Err(QuantError::Unsupported(ty));
    }
    let expected = byte_len(ty, out.len())?;
    if bytes.len() != expected {
        return Err(QuantError::WrongByteLength {
            expected,
            found: bytes.len(),
        });
    }
    match ty {
        GgmlType::F32 => float::f32s(bytes, out),
        GgmlType::F16 => float::f16s(bytes, out),
        GgmlType::BF16 => float::bf16s(bytes, out),
        GgmlType::Q4_0 => legacy::q4_0(bytes, out),
        GgmlType::Q4_1 => legacy::q4_1(bytes, out),
        GgmlType::Q5_0 => legacy::q5_0(bytes, out),
        GgmlType::Q5_1 => legacy::q5_1(bytes, out),
        GgmlType::Q8_0 => legacy::q8_0(bytes, out),
        GgmlType::Q4_K => kquant::q4_k(bytes, out),
        GgmlType::Q5_K => kquant::q5_k(bytes, out),
        GgmlType::Q6_K => kquant::q6_k(bytes, out),
        other => return Err(QuantError::Unsupported(other)),
    }
    Ok(())
}

/// Dequantise into a new vector of `n` elements.
pub fn dequantize_vec(ty: GgmlType, bytes: &[u8], n: usize) -> Result<Vec<f32>, QuantError> {
    let mut out = vec![0.0f32; n];
    dequantize(ty, bytes, &mut out)?;
    Ok(out)
}

/// Dot product of one quantised row with `x` (dequantises the row first).
pub fn dot_row(ty: GgmlType, row: &[u8], x: &[f32]) -> Result<f32, QuantError> {
    let mut tmp = vec![0.0f32; x.len()];
    dequantize(ty, row, &mut tmp)?;
    Ok(tmp.iter().zip(x).map(|(a, b)| a * b).sum())
}

/// Matrix-vector product with a quantised row-major matrix: `out[r] = row_r · x`.
///
/// `rows * cols` elements are expected in `bytes`; `cols` must be block aligned for `ty`.
pub fn matvec(
    ty: GgmlType,
    bytes: &[u8],
    rows: usize,
    cols: usize,
    x: &[f32],
    out: &mut [f32],
) -> Result<(), QuantError> {
    if x.len() != cols || out.len() != rows {
        return Err(QuantError::WrongOutputLength);
    }
    let row_bytes = byte_len(ty, cols)?;
    let total = row_bytes.checked_mul(rows).ok_or(QuantError::Overflow)?;
    if bytes.len() != total {
        return Err(QuantError::WrongByteLength {
            expected: total,
            found: bytes.len(),
        });
    }
    let mut tmp = vec![0.0f32; cols];
    for (r, o) in out.iter_mut().enumerate() {
        dequantize(ty, &bytes[r * row_bytes..(r + 1) * row_bytes], &mut tmp)?;
        *o = tmp.iter().zip(x).map(|(a, b)| a * b).sum();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_table() {
        assert!(is_supported(GgmlType::Q4_K));
        assert!(!is_supported(GgmlType::Q2_K));
        assert!(!is_supported(GgmlType::I32));
        let mut out = [0.0f32; 256];
        assert_eq!(
            dequantize(GgmlType::Q2_K, &[0; 84], &mut out),
            Err(QuantError::Unsupported(GgmlType::Q2_K))
        );
    }

    #[test]
    fn byte_len_checks_alignment() {
        assert_eq!(byte_len(GgmlType::F32, 3), Ok(12));
        assert_eq!(byte_len(GgmlType::Q8_0, 64), Ok(68));
        assert_eq!(byte_len(GgmlType::Q4_K, 512), Ok(288));
        assert_eq!(
            byte_len(GgmlType::Q8_0, 33),
            Err(QuantError::NotBlockAligned)
        );
        assert_eq!(
            byte_len(GgmlType::F32, usize::MAX),
            Err(QuantError::Overflow)
        );
    }

    #[test]
    fn length_mismatches_are_errors() {
        let mut out = [0.0f32; 32];
        assert_eq!(
            dequantize(GgmlType::Q8_0, &[0; 33], &mut out),
            Err(QuantError::WrongByteLength {
                expected: 34,
                found: 33
            })
        );
        let mut odd = [0.0f32; 31];
        assert_eq!(
            dequantize(GgmlType::Q8_0, &[0; 34], &mut odd),
            Err(QuantError::NotBlockAligned)
        );
    }

    #[test]
    fn dot_and_matvec_agree_with_manual_sum() {
        // two Q8_0 rows of 32: scale 0.5, values 1..=32 and all 2
        fn row(scale: u16, f: impl Fn(usize) -> i8) -> Vec<u8> {
            let mut b = scale.to_le_bytes().to_vec();
            b.extend((0..32).map(|i| f(i) as u8));
            b
        }
        let half = 0x3800u16; // 0.5 in f16
        let r0 = row(half, |i| (i + 1) as i8);
        let r1 = row(half, |_| 2);
        let x: Vec<f32> = (0..32).map(|i| i as f32 * 0.1).collect();
        let want0: f32 = (0..32).map(|i| 0.5 * (i + 1) as f32 * x[i]).sum();
        let want1: f32 = (0..32).map(|i| 0.5 * 2.0 * x[i]).sum();
        assert!((dot_row(GgmlType::Q8_0, &r0, &x).unwrap() - want0).abs() < 1e-4);
        let mut bytes = r0.clone();
        bytes.extend_from_slice(&r1);
        let mut out = [0.0f32; 2];
        matvec(GgmlType::Q8_0, &bytes, 2, 32, &x, &mut out).unwrap();
        assert!((out[0] - want0).abs() < 1e-4 && (out[1] - want1).abs() < 1e-4);
        assert!(matvec(GgmlType::Q8_0, &bytes, 2, 32, &x[..31], &mut out).is_err());
        assert!(matvec(GgmlType::Q8_0, &bytes[..60], 2, 32, &x, &mut out).is_err());
    }

    #[test]
    fn errors_display() {
        assert!(QuantError::Unsupported(GgmlType::Q2_K)
            .to_string()
            .contains("Q2_K"));
        assert!(QuantError::WrongByteLength {
            expected: 1,
            found: 2
        }
        .to_string()
        .contains('2'));
    }
}
