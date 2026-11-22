//! Scalar reference kernels (f32).
//!
//! Ported from the retired C `aeon_transformer_compute`. These favour
//! clarity over speed and are the oracle for optimised backends.

use crate::OpError;

/// In-place, numerically stable softmax.
pub fn softmax(x: &mut [f32]) -> Result<(), OpError> {
    if x.is_empty() {
        return Err(OpError::Param);
    }
    let max = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    for v in x.iter_mut() {
        *v = (*v - max).exp();
        sum += *v;
    }
    if !positive(sum) || !sum.is_finite() {
        return Err(OpError::NonFinite);
    }
    for v in x.iter_mut() {
        *v /= sum;
    }
    Ok(())
}

/// RMS normalisation: `out = x / sqrt(mean(x^2) + eps) * weight`.
///
/// `weight` may be empty to skip the learned scale.
pub fn rmsnorm(x: &[f32], weight: &[f32], eps: f32, out: &mut [f32]) -> Result<(), OpError> {
    if x.is_empty() || eps.is_nan() || eps < 0.0 {
        return Err(OpError::Param);
    }
    if out.len() != x.len() || !(weight.is_empty() || weight.len() == x.len()) {
        return Err(OpError::Shape);
    }
    let ss: f32 = x.iter().map(|v| v * v).sum();
    let inv = 1.0 / (ss / x.len() as f32 + eps).sqrt();
    if !inv.is_finite() {
        return Err(OpError::NonFinite);
    }
    for (i, (o, v)) in out.iter_mut().zip(x).enumerate() {
        let w = weight.get(i).copied().unwrap_or(1.0);
        *o = v * inv * w;
    }
    Ok(())
}

/// Rotary position embedding on interleaved pairs, applied to `q` and `k`
/// (each `heads * head_dim`), with frequency base `theta` (10000 for Llama).
pub fn rope(
    q: &mut [f32],
    k: &mut [f32],
    heads: usize,
    head_dim: usize,
    position: usize,
    theta: f32,
) -> Result<(), OpError> {
    if heads == 0 || head_dim == 0 || !head_dim.is_multiple_of(2) || !positive(theta) {
        return Err(OpError::Param);
    }
    let n = heads.checked_mul(head_dim).ok_or(OpError::Param)?;
    if q.len() != n || k.len() != n {
        return Err(OpError::Shape);
    }
    for j in (0..head_dim).step_by(2) {
        let angle = position as f32 * theta.powf(-(j as f32) / head_dim as f32);
        let (s, c) = angle.sin_cos();
        for h in 0..heads {
            let b = h * head_dim + j;
            for t in [&mut *q, &mut *k] {
                let (a0, a1) = (t[b], t[b + 1]);
                t[b] = a0 * c - a1 * s;
                t[b + 1] = a0 * s + a1 * c;
            }
        }
    }
    Ok(())
}

/// Single-query attention over `tokens` cached keys/values of width `dim`.
///
/// `scratch` must hold at least `tokens` floats; no allocation happens.
pub fn attention(
    q: &[f32],
    k: &[f32],
    v: &[f32],
    tokens: usize,
    scale: f32,
    scratch: &mut [f32],
    out: &mut [f32],
) -> Result<(), OpError> {
    let dim = q.len();
    if dim == 0 || tokens == 0 || !positive(scale) {
        return Err(OpError::Param);
    }
    let kv = tokens.checked_mul(dim).ok_or(OpError::Param)?;
    if k.len() != kv || v.len() != kv || out.len() != dim || scratch.len() < tokens {
        return Err(OpError::Shape);
    }
    let scores = &mut scratch[..tokens];
    for (s, krow) in scores.iter_mut().zip(k.chunks_exact(dim)) {
        *s = dot(q, krow) * scale;
    }
    softmax(scores)?;
    out.fill(0.0);
    for (&p, vrow) in scores.iter().zip(v.chunks_exact(dim)) {
        for (o, &x) in out.iter_mut().zip(vrow) {
            *o += p * x;
        }
    }
    Ok(())
}

/// `out = silu(gate) * up`.
pub fn swiglu(gate: &[f32], up: &[f32], out: &mut [f32]) -> Result<(), OpError> {
    if gate.is_empty() {
        return Err(OpError::Param);
    }
    if up.len() != gate.len() || out.len() != gate.len() {
        return Err(OpError::Shape);
    }
    for ((o, &g), &u) in out.iter_mut().zip(gate).zip(up) {
        *o = silu(g) * u;
    }
    Ok(())
}

/// Row-major matrix-vector product: `out[r] = sum_c w[r][c] * x[c]`.
pub fn matvec(w: &[f32], x: &[f32], out: &mut [f32]) -> Result<(), OpError> {
    let cols = x.len();
    if cols == 0 || out.is_empty() {
        return Err(OpError::Param);
    }
    if w.len() != cols.checked_mul(out.len()).ok_or(OpError::Param)? {
        return Err(OpError::Shape);
    }
    for (o, row) in out.iter_mut().zip(w.chunks_exact(cols)) {
        *o = dot(row, x);
    }
    Ok(())
}

/// True for finite or infinite values strictly above zero (false for NaN).
fn positive(x: f32) -> bool {
    x > 0.0
}

/// SiLU activation.
#[must_use]
pub fn silu(x: f32) -> f32 {
    x / (1.0 + (-x).exp())
}

/// Dot product of equal-length slices (extra elements are ignored).
#[must_use]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn softmax_sums_to_one() {
        let mut x = [1.0, 2.0, 3.0];
        softmax(&mut x).unwrap();
        assert!(close(x.iter().sum(), 1.0));
        assert!(x[2] > x[1] && x[1] > x[0]);
        assert_eq!(softmax(&mut []), Err(OpError::Param));
    }

    #[test]
    fn rmsnorm_unit() {
        let mut out = [0.0; 2];
        rmsnorm(&[3.0, 4.0], &[], 0.0, &mut out).unwrap();
        let r = (12.5f32).sqrt();
        assert!(close(out[0], 3.0 / r) && close(out[1], 4.0 / r));
        rmsnorm(&[3.0, 4.0], &[2.0, 0.0], 0.0, &mut out).unwrap();
        assert!(close(out[0], 6.0 / r) && close(out[1], 0.0));
        assert_eq!(rmsnorm(&[1.0], &[], -1.0, &mut [0.0]), Err(OpError::Param));
    }

    #[test]
    fn rope_position_zero_is_identity_and_preserves_norm() {
        let mut q = [1.0, 2.0, 3.0, 4.0];
        let mut k = q;
        rope(&mut q, &mut k, 1, 4, 0, 10000.0).unwrap();
        assert_eq!(q, [1.0, 2.0, 3.0, 4.0]);
        rope(&mut q, &mut k, 1, 4, 7, 10000.0).unwrap();
        assert!(close(q[0] * q[0] + q[1] * q[1], 5.0));
        assert_eq!(q, k);
        assert_eq!(rope(&mut q, &mut k, 1, 3, 0, 10000.0), Err(OpError::Param));
    }

    #[test]
    fn attention_picks_matching_key() {
        let q = [10.0, 0.0];
        let k = [1.0, 0.0, 0.0, 1.0];
        let v = [5.0, 5.0, -5.0, -5.0];
        let mut s = [0.0; 2];
        let mut out = [0.0; 2];
        attention(&q, &k, &v, 2, 1.0, &mut s, &mut out).unwrap();
        assert!(out[0] > 4.99);
        assert_eq!(
            attention(&q, &k, &v, 2, 1.0, &mut [0.0; 1], &mut out),
            Err(OpError::Shape)
        );
    }

    #[test]
    fn swiglu_and_matvec() {
        let mut out = [0.0; 1];
        swiglu(&[0.0], &[3.0], &mut out).unwrap();
        assert!(close(out[0], 0.0));
        let mut y = [0.0; 2];
        matvec(&[1.0, 2.0, 3.0, 4.0], &[1.0, 1.0], &mut y).unwrap();
        assert_eq!(y, [3.0, 7.0]);
        assert_eq!(matvec(&[1.0], &[1.0, 1.0], &mut y), Err(OpError::Shape));
    }
}
