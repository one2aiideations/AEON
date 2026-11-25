//! Distribution statistics of weights.

use crate::{AnalysisError, Matrix};

/// Mean and sample standard deviation (n - 1). `None` for fewer than two values.
pub fn mean_std(x: &[f32]) -> Option<(f64, f64)> {
    if x.len() < 2 {
        return None;
    }
    let n = x.len() as f64;
    let mean = x.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let var = x
        .iter()
        .map(|v| (f64::from(*v) - mean).powi(2))
        .sum::<f64>()
        / (n - 1.0);
    Some((mean, var.sqrt()))
}

/// Shannon entropy in bits of an equal-width histogram over `[min, max]`.
///
/// `None` for empty input; `0.0` when all values are (nearly) equal. The last bin
/// includes the maximum. `bins` must be at least 1.
pub fn entropy_bits(x: &[f32], bins: usize) -> Result<Option<f64>, AnalysisError> {
    if bins == 0 {
        return Err(AnalysisError::BadParameter("bins must be at least 1"));
    }
    if x.is_empty() {
        return Ok(None);
    }
    if x.iter().any(|v| !v.is_finite()) {
        return Err(AnalysisError::NonFinite);
    }
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in x {
        lo = lo.min(f64::from(*v));
        hi = hi.max(f64::from(*v));
    }
    // numpy.allclose(min, max) with its default tolerances.
    if (lo - hi).abs() <= 1e-8 + 1e-5 * hi.abs() {
        return Ok(Some(0.0));
    }
    let width = (hi - lo) / bins as f64;
    let mut counts = vec![0u64; bins];
    for v in x {
        let idx = (((f64::from(*v) - lo) / width) as usize).min(bins - 1);
        counts[idx] += 1;
    }
    let total = x.len() as f64;
    let h = counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / total;
            -p * p.log2()
        })
        .sum();
    Ok(Some(h))
}

/// Fraction of values with `|x| > |mean| + sigma * std`. `None` when the std is zero or
/// there are fewer than two values.
pub fn outlier_fraction(x: &[f32], sigma: f64) -> Option<f64> {
    let (mean, std) = mean_std(x)?;
    if std <= 0.0 {
        return None;
    }
    let limit = mean.abs() + sigma * std;
    let n = x.iter().filter(|v| f64::from(v.abs()) > limit).count();
    Some(n as f64 / x.len() as f64)
}

/// Fraction of adjacent (after ordering) unit-normalised rows whose cosine similarity
/// is at least `threshold`, over at most `sample_count` evenly strided rows.
///
/// This is the original's approximation: sorting by the first coordinate and comparing
/// neighbours avoids an N² comparison. Zero rows are ignored. `None` with fewer than
/// two usable rows.
pub fn near_duplicate_fraction(m: Matrix<'_>, threshold: f64, sample_count: usize) -> Option<f64> {
    if m.rows < 2 || sample_count == 0 {
        return None;
    }
    let count = sample_count.min(m.rows);
    let mut rows: Vec<Vec<f64>> = Vec::with_capacity(count);
    for i in 0..count {
        let r = m.row(i * m.rows / count);
        let norm = r.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>().sqrt();
        if norm > 1e-12 {
            rows.push(r.iter().map(|v| f64::from(*v) / norm).collect());
        }
    }
    if rows.len() < 2 {
        return None;
    }
    rows.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let hits = rows
        .windows(2)
        .filter(|w| w[0].iter().zip(&w[1]).map(|(a, b)| a * b).sum::<f64>() >= threshold)
        .count();
    Some(hits as f64 / (rows.len() - 1) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_std_is_sample_std() {
        let (m, s) = mean_std(&[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert!((m - 2.5).abs() < 1e-12);
        assert!((s - (5.0f64 / 3.0).sqrt()).abs() < 1e-12);
        assert!(mean_std(&[1.0]).is_none());
        assert!(mean_std(&[]).is_none());
    }

    #[test]
    fn entropy_of_uniform_and_degenerate() {
        // 4 bins, one value each at the bin centres -> exactly 2 bits.
        let h = entropy_bits(&[0.0, 1.0, 2.0, 3.0], 4).unwrap().unwrap();
        assert!((h - 2.0).abs() < 1e-12);
        assert_eq!(entropy_bits(&[5.0, 5.0, 5.0], 16).unwrap(), Some(0.0));
        assert_eq!(entropy_bits(&[], 16).unwrap(), None);
        // everything in one bin except the max: two occupied bins
        let h = entropy_bits(&[0.0, 0.0, 0.0, 1.0], 2).unwrap().unwrap();
        assert!((h - 0.811_278_124_459_132_9).abs() < 1e-9);
    }

    #[test]
    fn entropy_rejects_bad_input() {
        assert!(entropy_bits(&[1.0], 0).is_err());
        assert_eq!(
            entropy_bits(&[1.0, f32::NAN], 4).unwrap_err(),
            AnalysisError::NonFinite
        );
    }

    #[test]
    fn entropy_never_exceeds_log2_bins() {
        let x: Vec<f32> = (0..1000).map(|i| ((i * 7919) % 1000) as f32).collect();
        let h = entropy_bits(&x, 128).unwrap().unwrap();
        assert!(h > 6.5 && h <= 7.0 + 1e-9, "{h}");
    }

    #[test]
    fn outliers_use_abs_mean_plus_sigma_std() {
        let mut x = vec![0.0f32; 99];
        x.push(100.0);
        // mean 1, std ~10; limit = 1 + 3*10.05 ~ 31 -> only the 100 exceeds
        let f = outlier_fraction(&x, 3.0).unwrap();
        assert!((f - 0.01).abs() < 1e-12);
        assert_eq!(outlier_fraction(&[2.0, 2.0, 2.0], 3.0), None);
        assert_eq!(outlier_fraction(&[1.0], 3.0), None);
    }

    #[test]
    fn near_duplicates_detects_repeated_rows() {
        let data = [1.0f32, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0];
        let m = Matrix::new(&data, 4, 2).unwrap();
        // sorted by first coord: (0,1),(0,1),(1,0),(1,0): pairs 1 and 3 are duplicates, 2 is not
        let f = near_duplicate_fraction(m, 0.995, 4096).unwrap();
        assert!((f - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn near_duplicates_edge_cases() {
        let one = Matrix::new(&[1.0, 2.0], 1, 2).unwrap();
        assert_eq!(near_duplicate_fraction(one, 0.99, 10), None);
        let zeros = Matrix::new(&[0.0; 6], 3, 2).unwrap();
        assert_eq!(near_duplicate_fraction(zeros, 0.99, 10), None);
        let m = Matrix::new(&[1.0, 0.0, 0.0, 1.0], 2, 2).unwrap();
        assert_eq!(near_duplicate_fraction(m, 0.99, 0), None);
        assert_eq!(near_duplicate_fraction(m, 0.99, 10), Some(0.0));
    }

    #[test]
    fn sampling_is_deterministic_and_strided() {
        let data: Vec<f32> = (0..200).map(|i| (i % 7) as f32 + 1.0).collect();
        let m = Matrix::new(&data, 100, 2).unwrap();
        assert_eq!(
            near_duplicate_fraction(m, 0.99, 10),
            near_duplicate_fraction(m, 0.99, 10)
        );
    }
}
