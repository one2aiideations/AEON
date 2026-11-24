//! Magnitude pruning plans.

use crate::AnalysisError;

/// The global magnitude threshold that would prune the smallest `ratio` of all weights:
/// the `k`-th smallest absolute value with `k = clamp(floor(ratio * n), 1, n)`.
///
/// This is a *plan*. Weights with `|w| <= threshold` are the pruning candidates; whether
/// the pruned model still works is a separate experiment. `ratio` must be in `[0, 1]`.
/// `None` when there are no weights.
pub fn global_magnitude_threshold(
    tensors: &[&[f32]],
    ratio: f64,
) -> Result<Option<f32>, AnalysisError> {
    if !(0.0..=1.0).contains(&ratio) || ratio.is_nan() {
        return Err(AnalysisError::BadParameter("ratio must be in [0, 1]"));
    }
    let mut all: Vec<f32> = Vec::with_capacity(tensors.iter().map(|t| t.len()).sum());
    for t in tensors {
        for v in *t {
            if !v.is_finite() {
                return Err(AnalysisError::NonFinite);
            }
            all.push(v.abs());
        }
    }
    if all.is_empty() {
        return Ok(None);
    }
    let n = all.len();
    let k = ((ratio * n as f64) as usize).clamp(1, n);
    let (_, kth, _) = all.select_nth_unstable_by(k - 1, |a, b| a.total_cmp(b));
    Ok(Some(*kth))
}

/// Fraction of weights with `|w| <= threshold` (the share a plan would prune).
pub fn pruned_fraction(tensors: &[&[f32]], threshold: f32) -> f64 {
    let total: usize = tensors.iter().map(|t| t.len()).sum();
    if total == 0 {
        return 0.0;
    }
    let pruned: usize = tensors
        .iter()
        .map(|t| t.iter().filter(|v| v.abs() <= threshold).count())
        .sum();
    pruned as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_the_kth_smallest_magnitude() {
        let a = [1.0f32, -2.0, 3.0];
        let b = [-4.0f32, 5.0, 0.5, 6.0, -7.0, 8.0, 9.0];
        let t = [&a[..], &b[..]];
        // sorted magnitudes: 0.5 1 2 3 4 5 6 7 8 9 ; 10 weights
        assert_eq!(global_magnitude_threshold(&t, 0.5).unwrap(), Some(4.0));
        assert_eq!(global_magnitude_threshold(&t, 1.0).unwrap(), Some(9.0));
        assert_eq!(global_magnitude_threshold(&t, 0.0).unwrap(), Some(0.5)); // k clamps to 1
        assert_eq!(global_magnitude_threshold(&t, 0.05).unwrap(), Some(0.5));
    }

    #[test]
    fn pruned_fraction_matches_the_plan() {
        let w: Vec<f32> = (1..=100).map(|i| i as f32).collect();
        let t = [&w[..]];
        let th = global_magnitude_threshold(&t, 0.3).unwrap().unwrap();
        assert_eq!(th, 30.0);
        assert!((pruned_fraction(&t, th) - 0.3).abs() < 1e-12);
        assert_eq!(pruned_fraction(&[], 1.0), 0.0);
    }

    #[test]
    fn empty_and_invalid() {
        assert_eq!(global_magnitude_threshold(&[], 0.5).unwrap(), None);
        assert_eq!(global_magnitude_threshold(&[&[]], 0.5).unwrap(), None);
        assert!(global_magnitude_threshold(&[&[1.0]], -0.1).is_err());
        assert!(global_magnitude_threshold(&[&[1.0]], 1.1).is_err());
        assert!(global_magnitude_threshold(&[&[1.0]], f64::NAN).is_err());
        assert_eq!(
            global_magnitude_threshold(&[&[f32::NAN]], 0.5).unwrap_err(),
            AnalysisError::NonFinite
        );
    }

    #[test]
    fn ties_are_handled() {
        let w = [2.0f32; 10];
        assert_eq!(
            global_magnitude_threshold(&[&w[..]], 0.5).unwrap(),
            Some(2.0)
        );
        assert_eq!(pruned_fraction(&[&w[..]], 2.0), 1.0);
    }
}
