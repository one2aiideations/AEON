//! Singular values, effective rank and low-rank approximation error.
//!
//! Singular values are computed as the square roots of the eigenvalues of the smaller
//! Gram matrix (`AᵀA` or `AAᵀ`) with a cyclic Jacobi solver in `f64`. That is accurate
//! enough for analysis (small singular values lose relative precision) and needs no
//! linear-algebra dependency.

use crate::{AnalysisError, Matrix};

/// Largest matrix side analysed by default; bigger inputs are cropped to the top-left block.
pub const DEFAULT_MAX_DIM: usize = 256;

const MAX_SWEEPS: usize = 64;

/// Eigenvalues of a symmetric `n x n` matrix (row-major, destroyed), unordered.
fn jacobi_eigenvalues(a: &mut [f64], n: usize) -> Vec<f64> {
    for _ in 0..MAX_SWEEPS {
        let mut off = 0.0f64;
        let mut diag = 0.0f64;
        for i in 0..n {
            diag += a[i * n + i] * a[i * n + i];
            for j in i + 1..n {
                off += a[i * n + j] * a[i * n + j];
            }
        }
        if off <= 1e-24 * (diag + off).max(f64::MIN_POSITIVE) {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
            }
        }
    }
    (0..n).map(|i| a[i * n + i]).collect()
}

/// Squared singular values in descending order, from the top-left
/// `min(rows, max_dim) x min(cols, max_dim)` block of `m`.
pub fn singular_values_squared(m: Matrix<'_>, max_dim: usize) -> Result<Vec<f64>, AnalysisError> {
    if max_dim == 0 {
        return Err(AnalysisError::BadParameter("max_dim must be at least 1"));
    }
    let (r, c) = (m.rows.min(max_dim), m.cols.min(max_dim));
    let at = |i: usize, j: usize| f64::from(m.data[i * m.cols + j]);
    // Gram matrix of the smaller side.
    let n = r.min(c);
    let mut g = vec![0.0f64; n * n];
    if c <= r {
        for i in 0..r {
            for p in 0..c {
                let aip = at(i, p);
                if aip != 0.0 {
                    for q in p..c {
                        g[p * n + q] += aip * at(i, q);
                    }
                }
            }
        }
    } else {
        for p in 0..r {
            for q in p..r {
                let mut s = 0.0;
                for j in 0..c {
                    s += at(p, j) * at(q, j);
                }
                g[p * n + q] = s;
            }
        }
    }
    for p in 0..n {
        for q in 0..p {
            g[p * n + q] = g[q * n + p];
        }
    }
    let mut ev = jacobi_eigenvalues(&mut g, n);
    for e in &mut ev {
        if *e < 0.0 {
            *e = 0.0; // rounding noise
        }
    }
    ev.sort_by(|a, b| b.total_cmp(a));
    Ok(ev)
}

/// Smallest rank whose singular values capture at least `energy_threshold` of the total
/// squared energy (the original's `searchsorted(...) + 1`). `0` for an all-zero matrix.
/// `energy_threshold` must be in `(0, 1]`.
pub fn effective_rank(
    m: Matrix<'_>,
    energy_threshold: f64,
    max_dim: usize,
) -> Result<usize, AnalysisError> {
    if !(energy_threshold > 0.0 && energy_threshold <= 1.0) {
        return Err(AnalysisError::BadParameter(
            "energy_threshold must be in (0, 1]",
        ));
    }
    let power = singular_values_squared(m, max_dim)?;
    let total: f64 = power.iter().sum();
    if total <= 0.0 {
        return Ok(0);
    }
    let mut cum = 0.0;
    for (i, p) in power.iter().enumerate() {
        cum += p;
        if cum / total >= energy_threshold - 1e-12 {
            return Ok(i + 1);
        }
    }
    Ok(power.len())
}

/// Rank used for a given parameter ratio: `max(1, floor(min(rows, cols) * ratio))`.
/// `ratio` must be in `(0, 1]`.
pub fn rank_for_ratio(rows: usize, cols: usize, ratio: f64) -> Result<usize, AnalysisError> {
    if !(ratio > 0.0 && ratio <= 1.0) {
        return Err(AnalysisError::BadParameter("ratio must be in (0, 1]"));
    }
    Ok(((rows.min(cols) as f64 * ratio) as usize).max(1))
}

/// Relative Frobenius error of the best rank-`rank` approximation:
/// `‖A - A_k‖_F / (‖A‖_F + 1e-12)`. By the Eckart–Young theorem this equals
/// `sqrt(Σ_{i>k} σᵢ²)` over the full norm, so no reconstruction is needed.
/// Computed on the same cropped block as [`singular_values_squared`].
pub fn low_rank_relative_error(
    m: Matrix<'_>,
    rank: usize,
    max_dim: usize,
) -> Result<f64, AnalysisError> {
    if rank == 0 {
        return Err(AnalysisError::BadParameter("rank must be at least 1"));
    }
    let power = singular_values_squared(m, max_dim)?;
    let total: f64 = power.iter().sum();
    let tail: f64 = power.iter().skip(rank).sum();
    Ok(tail.sqrt() / (total.sqrt() + 1e-12))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mat(data: &[f32], r: usize, c: usize) -> Matrix<'_> {
        Matrix::new(data, r, c).unwrap()
    }

    #[test]
    fn diagonal_matrix_singular_values() {
        let d = [3.0f32, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 1.0];
        let s = singular_values_squared(mat(&d, 3, 3), 256).unwrap();
        for (got, want) in s.iter().zip([16.0, 9.0, 1.0]) {
            assert!((got - want).abs() < 1e-9, "{s:?}");
        }
    }

    #[test]
    fn tall_and_wide_agree() {
        let a: Vec<f32> = (0..24).map(|i| ((i * 5) % 11) as f32 - 4.0).collect();
        let tall = singular_values_squared(mat(&a, 6, 4), 256).unwrap();
        // transpose
        let mut t = vec![0.0f32; 24];
        for i in 0..6 {
            for j in 0..4 {
                t[j * 6 + i] = a[i * 4 + j];
            }
        }
        let wide = singular_values_squared(mat(&t, 4, 6), 256).unwrap();
        assert_eq!(tall.len(), 4);
        for (x, y) in tall.iter().zip(&wide) {
            assert!((x - y).abs() < 1e-6 * x.max(1.0), "{tall:?} vs {wide:?}");
        }
    }

    #[test]
    fn energy_equals_squared_frobenius_norm() {
        let a: Vec<f32> = (0..30).map(|i| (i as f32).sin() * 3.0).collect();
        let s = singular_values_squared(mat(&a, 5, 6), 256).unwrap();
        let frob: f64 = a.iter().map(|v| f64::from(*v).powi(2)).sum();
        assert!((s.iter().sum::<f64>() - frob).abs() < 1e-6 * frob);
    }

    #[test]
    fn rank_one_matrix_has_effective_rank_one() {
        // outer product u v^T
        let u = [1.0f32, 2.0, 3.0, 4.0];
        let v = [2.0f32, -1.0, 0.5];
        let mut a = Vec::new();
        for x in u {
            for y in v {
                a.push(x * y);
            }
        }
        assert_eq!(effective_rank(mat(&a, 4, 3), 0.99, 256).unwrap(), 1);
        let s = singular_values_squared(mat(&a, 4, 3), 256).unwrap();
        assert!(s[1] < 1e-9 * s[0]);
    }

    #[test]
    fn identity_needs_every_dimension_for_full_energy() {
        let mut i = vec![0.0f32; 16];
        for k in 0..4 {
            i[k * 4 + k] = 1.0;
        }
        assert_eq!(effective_rank(mat(&i, 4, 4), 1.0, 256).unwrap(), 4);
        assert_eq!(effective_rank(mat(&i, 4, 4), 0.5, 256).unwrap(), 2);
        assert_eq!(effective_rank(mat(&i, 4, 4), 0.99, 256).unwrap(), 4);
    }

    #[test]
    fn zero_matrix_has_rank_zero() {
        assert_eq!(effective_rank(mat(&[0.0; 6], 2, 3), 0.99, 256).unwrap(), 0);
    }

    #[test]
    fn low_rank_error_is_exact_for_a_diagonal() {
        let d = [3.0f32, 0.0, 0.0, 4.0];
        // keep the largest (4): tail is 3 over sqrt(25) = 5
        let e = low_rank_relative_error(mat(&d, 2, 2), 1, 256).unwrap();
        assert!((e - 0.6).abs() < 1e-9, "{e}");
        assert!(low_rank_relative_error(mat(&d, 2, 2), 2, 256).unwrap() < 1e-9);
        assert!(low_rank_relative_error(mat(&d, 2, 2), 5, 256).unwrap() < 1e-9);
    }

    #[test]
    fn error_decreases_with_rank() {
        let a: Vec<f32> = (0..64).map(|i| ((i * 13) % 17) as f32 - 8.0).collect();
        let m = mat(&a, 8, 8);
        let mut last = f64::MAX;
        for r in 1..=8 {
            let e = low_rank_relative_error(m, r, 256).unwrap();
            assert!(e <= last + 1e-12);
            last = e;
        }
        assert!(last < 1e-6);
    }

    #[test]
    fn cropping_to_max_dim() {
        let a: Vec<f32> = (0..100).map(|i| (i % 9) as f32).collect();
        let cropped = singular_values_squared(mat(&a, 10, 10), 3).unwrap();
        assert_eq!(cropped.len(), 3);
        assert!(singular_values_squared(mat(&a, 10, 10), 0).is_err());
    }

    #[test]
    fn rank_for_ratio_rules() {
        assert_eq!(rank_for_ratio(10, 20, 0.5).unwrap(), 5);
        assert_eq!(rank_for_ratio(10, 20, 0.01).unwrap(), 1);
        assert_eq!(rank_for_ratio(10, 20, 1.0).unwrap(), 10);
        assert!(rank_for_ratio(10, 20, 0.0).is_err());
        assert!(rank_for_ratio(10, 20, 1.5).is_err());
    }

    #[test]
    fn parameter_validation() {
        let m = mat(&[1.0, 0.0, 0.0, 1.0], 2, 2);
        assert!(effective_rank(m, 0.0, 8).is_err());
        assert!(effective_rank(m, 1.5, 8).is_err());
        assert!(low_rank_relative_error(m, 0, 8).is_err());
    }
}
