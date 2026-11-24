//! Simple 4-bit packed weights with one scale per matrix.
//!
//! Two weights per byte, low nibble first, stored offset by 8 (so nibble
//! `n` means `n - 8`). This is the minimal format the retired C core used;
//! GGUF block formats (Q4_0 with per-block f16 scales, K-quants) live in
//! `mouli-gguf` and get their own kernels.

use crate::OpError;

/// Unpack `count` signed 4-bit values.
pub fn unpack_nibbles(packed: &[u8], count: usize) -> Result<Vec<i8>, OpError> {
    if packed.len() < count.div_ceil(2) {
        return Err(OpError::Shape);
    }
    Ok((0..count).map(|i| nibble(packed, i)).collect())
}

fn nibble(packed: &[u8], i: usize) -> i8 {
    let b = packed[i / 2];
    let n = if i % 2 == 1 { b >> 4 } else { b & 0x0f };
    // n is 0..=15, so the result is in -8..=7.
    n as i8 - 8
}

/// `out[r] = scale * sum_c q[r][c] * x[c]` over packed row-major weights.
pub fn gemv_nibbles(packed: &[u8], x: &[f32], scale: f32, out: &mut [f32]) -> Result<(), OpError> {
    let cols = x.len();
    if cols == 0 || out.is_empty() {
        return Err(OpError::Param);
    }
    let n = cols.checked_mul(out.len()).ok_or(OpError::Param)?;
    if packed.len() < n.div_ceil(2) {
        return Err(OpError::Shape);
    }
    for (r, o) in out.iter_mut().enumerate() {
        let base = r * cols;
        let acc: f32 = x
            .iter()
            .enumerate()
            .map(|(c, &xv)| f32::from(nibble(packed, base + c)) * xv)
            .sum();
        *o = acc * scale;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_and_gemv() {
        // values: [-8, 7, 0, 1] -> nibbles [0, 15, 8, 9]
        let packed = [0xf0u8, 0x98];
        assert_eq!(unpack_nibbles(&packed, 4).unwrap(), vec![-8, 7, 0, 1]);
        let mut out = [0.0f32; 2];
        gemv_nibbles(&packed, &[1.0, 1.0], 0.5, &mut out).unwrap();
        assert_eq!(out, [-0.5, 0.5]);
        assert!(unpack_nibbles(&packed, 5).is_err());
    }
}
