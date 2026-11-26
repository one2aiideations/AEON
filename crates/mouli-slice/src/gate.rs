//! Held-out quality evaluation and acceptance gating.
//!
//! The robust score is the mean of the lowest ceil(25%) of item scores.
//! Acceptance requires both mean-score and robust-score drops to remain
//! within tolerance.
//!
//! The gate refuses datasets containing fewer than 20 items. This is a
//! conservative minimum, not a statistical guarantee of sufficient power.
//! Acceptance does not prove generalization beyond the supplied items.

use crate::SliceError;

/// Minimum number of held-out items required for acceptance.
pub const MIN_ITEMS: usize = 20;

/// Result of comparing full-model and sliced-model scores.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// Whether the candidate passes every acceptance condition.
    pub accepted: bool,

    /// Mean full-model score.
    pub full_mean: f64,

    /// Mean sliced-model score.
    pub sliced_mean: f64,

    /// Robust full-model score.
    pub full_robust: f64,

    /// Robust sliced-model score.
    pub sliced_robust: f64,

    /// Mean-score drop, calculated as full minus sliced.
    ///
    /// Negative values indicate an improvement.
    pub drop_mean: f64,

    /// Robust-score drop, calculated as full minus sliced.
    ///
    /// Negative values indicate an improvement.
    pub drop_robust: f64,

    /// Human-readable explanation of the decision.
    pub note: String,
}

/// Calculates the mean of the lowest 25% of item scores.
///
/// The number of selected items is `ceil(items.len() / 4)`, with at least
/// one item. Scores must be finite and within `0..=1`.
///
/// This statistic is a conservative summary, not a guarantee against
/// failures on untested inputs.
///
/// # Errors
/// A [`SliceError`] for empty input or a score outside `0..=1`.
pub fn robust_score(items: &[f64]) -> Result<f64, SliceError> {
    if items.is_empty() {
        return Err(SliceError::EmptyInput("robust score items"));
    }

    validate_scores(items)?;

    let mut sorted = items.to_vec();
    sorted.sort_by(f64::total_cmp);

    let count = items.len() / 4 + usize::from(!items.len().is_multiple_of(4));

    let sum: f64 = sorted.iter().take(count).sum();
    let result = sum / count as f64;

    if !result.is_finite() {
        return Err(SliceError::NonFinite("robust score"));
    }

    Ok(result)
}

/// Compares full and sliced model scores.
///
/// Both score arrays must be non-empty, equal in length, and contain
/// finite values in `0..=1`.
///
/// `tolerance` must be finite and in `0..=1`.
///
/// Acceptance requires:
///
/// - `drop_mean <= tolerance`
/// - `drop_robust <= tolerance`
/// - at least [`MIN_ITEMS`] held-out items
///
/// An improvement is represented by a negative drop and is acceptable.
///
/// Failing the item-count requirement refuses the candidate even if
/// both numerical comparisons pass. The reason is included in `note`.
///
/// This function evaluates only the supplied scores. It does not establish
/// generalization or measure runtime performance.
///
/// # Errors
/// A [`SliceError`] for bad tolerance, empty or unequal score lists, or
/// an invalid score.
pub fn decide(full: &[f64], sliced: &[f64], tolerance: f64) -> Result<Decision, SliceError> {
    if !tolerance.is_finite() {
        return Err(SliceError::NonFinite("tolerance"));
    }

    if !(0.0..=1.0).contains(&tolerance) {
        return Err(SliceError::BadParameter("tolerance"));
    }

    if full.is_empty() || sliced.is_empty() {
        return Err(SliceError::EmptyInput("gate scores"));
    }

    if full.len() != sliced.len() {
        return Err(SliceError::BadShape("full and sliced score counts differ"));
    }

    validate_scores(full)?;
    validate_scores(sliced)?;

    let full_mean = mean(full)?;
    let sliced_mean = mean(sliced)?;

    let full_robust = robust_score(full)?;
    let sliced_robust = robust_score(sliced)?;

    let drop_mean = full_mean - sliced_mean;
    let drop_robust = full_robust - sliced_robust;

    if !drop_mean.is_finite() || !drop_robust.is_finite() {
        return Err(SliceError::NonFinite("score difference"));
    }

    let enough_items = full.len() >= MIN_ITEMS;

    let accepted = enough_items && drop_mean <= tolerance && drop_robust <= tolerance;

    let note = if !enough_items {
        format!(
            "Refused: only {} held-out items; with so few items the gate cannot tell loss from noise (minimum {}).",
            full.len(),
            MIN_ITEMS
        )
    } else if drop_mean > tolerance && drop_robust > tolerance {
        "Refused: both mean and robust score drops exceed tolerance.".to_string()
    } else if drop_mean > tolerance {
        "Refused: mean score drop exceeds tolerance.".to_string()
    } else if drop_robust > tolerance {
        "Refused: robust score drop exceeds tolerance.".to_string()
    } else {
        "Accepted: mean and robust score drops are within tolerance.".to_string()
    };

    Ok(Decision {
        accepted,
        full_mean,
        sliced_mean,
        full_robust,
        sliced_robust,
        drop_mean,
        drop_robust,
        note,
    })
}

/// Finds the smallest accepted retention fraction among the candidates.
///
/// Each candidate is evaluated independently. Candidates are not assumed
/// to have monotonic quality as retention increases; every candidate is
/// checked.
///
/// Returns `None` when no candidate passes.
///
/// Invalid candidate fractions, score arrays, or tolerances return errors.
/// This function does not prove that any untested retention fraction works.
///
/// # Errors
/// A [`SliceError`] for a bad fraction or from [`decide`].
pub fn smallest_accepted(
    candidates: &[(f64, Vec<f64>)],
    full: &[f64],
    tolerance: f64,
) -> Result<Option<f64>, SliceError> {
    let mut smallest: Option<f64> = None;

    for (fraction, scores) in candidates {
        if !fraction.is_finite() {
            return Err(SliceError::NonFinite("candidate keep fraction"));
        }

        if !(0.0 < *fraction && *fraction <= 1.0) {
            return Err(SliceError::BadParameter("candidate keep fraction"));
        }

        let decision = decide(full, scores, tolerance)?;

        if decision.accepted && smallest.is_none_or(|current| *fraction < current) {
            smallest = Some(*fraction);
        }
    }

    Ok(smallest)
}

fn validate_scores(scores: &[f64]) -> Result<(), SliceError> {
    for &score in scores {
        if !score.is_finite() {
            return Err(SliceError::NonFinite("item score"));
        }

        if !(0.0..=1.0).contains(&score) {
            return Err(SliceError::BadParameter("item score"));
        }
    }

    Ok(())
}

fn mean(scores: &[f64]) -> Result<f64, SliceError> {
    if scores.is_empty() {
        return Err(SliceError::EmptyInput("mean score"));
    }

    let sum: f64 = scores.iter().sum();

    if !sum.is_finite() {
        return Err(SliceError::NonFinite("mean score sum"));
    }

    let result = sum / scores.len() as f64;

    if !result.is_finite() {
        return Err(SliceError::NonFinite("mean score"));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perfect_scores() -> Vec<f64> {
        vec![1.0; 20]
    }

    #[test]
    fn robust_score_uses_lowest_quarter() {
        let scores = vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];

        assert!(robust_score(&scores).unwrap().abs() < f64::EPSILON);
    }

    #[test]
    fn robust_score_rounds_up() {
        // Five items: ceil(5 / 4) = 2 items, the mean of 0.2 and 0.4.
        let scores = vec![0.2, 0.4, 0.6, 0.8, 1.0];

        assert!((robust_score(&scores).unwrap() - 0.3).abs() < 1e-12);
    }

    #[test]
    fn empty_robust_input_is_rejected() {
        assert!(robust_score(&[]).is_err());
    }

    #[test]
    fn invalid_scores_are_rejected() {
        assert!(robust_score(&[f64::NAN]).is_err());
        assert!(robust_score(&[f64::INFINITY]).is_err());
        assert!(robust_score(&[-0.1]).is_err());
        assert!(robust_score(&[1.1]).is_err());
    }

    #[test]
    fn improvement_is_accepted() {
        let full = perfect_scores();
        let sliced = vec![1.0; 20];

        let decision = decide(&full, &sliced, 0.05).unwrap();

        assert!(decision.accepted);
        assert!(decision.drop_mean.abs() < f64::EPSILON);
        assert!(decision.drop_robust.abs() < f64::EPSILON);
    }

    #[test]
    fn improvement_over_full_is_accepted() {
        let full = vec![0.8; 20];
        let sliced = vec![0.9; 20];

        let decision = decide(&full, &sliced, 0.05).unwrap();

        assert!(decision.accepted);
        assert!(decision.drop_mean < 0.0);
        assert!(decision.drop_robust < 0.0);
    }

    #[test]
    fn mean_drop_beyond_tolerance_is_refused() {
        let full = vec![1.0; 20];
        let sliced = vec![0.8; 20];

        let decision = decide(&full, &sliced, 0.05).unwrap();

        assert!(!decision.accepted);
        assert!(decision.drop_mean > 0.05);
    }

    #[test]
    fn robust_drop_beyond_tolerance_is_refused() {
        let full = vec![1.0; 20];
        let mut sliced = vec![1.0; 20];

        // The lowest quarter collapses while the rest stay perfect.
        // The mean drop is only 0.25.
        for item in sliced.iter_mut().take(5) {
            *item = 0.0;
        }

        let decision = decide(&full, &sliced, 0.30).unwrap();

        assert!(decision.drop_mean <= 0.30);
        assert!(decision.drop_robust > 0.30);
        assert!(!decision.accepted);
    }

    #[test]
    fn low_quarter_collapse_is_refused_despite_mean_within_tolerance() {
        let full = vec![1.0; 20];
        let mut sliced = vec![1.0; 20];

        for item in sliced.iter_mut().take(5) {
            *item = 0.0;
        }

        // Tolerance 0.30: the mean (drop 0.25) passes, the worst quarter does not.
        let decision = decide(&full, &sliced, 0.30).unwrap();

        assert!((decision.sliced_mean - 0.75).abs() < 1e-12);
        assert!(decision.drop_mean <= 0.30);
        assert!(decision.sliced_robust.abs() < f64::EPSILON);
        assert!(!decision.accepted);
        assert!(decision.note.contains("robust"));
    }

    #[test]
    fn too_few_items_are_refused() {
        let full = vec![1.0; 10];
        let sliced = vec![1.0; 10];

        let decision = decide(&full, &sliced, 0.5).unwrap();

        assert!(!decision.accepted);
        assert!(decision.note.contains("cannot tell loss from noise"));
    }

    #[test]
    fn unequal_score_lengths_are_rejected() {
        assert!(decide(&[1.0, 1.0], &[1.0], 0.1).is_err());
    }

    #[test]
    fn invalid_tolerance_is_rejected() {
        let scores = perfect_scores();

        assert!(decide(&scores, &scores, -0.1).is_err());
        assert!(decide(&scores, &scores, 1.1).is_err());
        assert!(decide(&scores, &scores, f64::NAN).is_err());
    }

    #[test]
    fn smallest_accepted_checks_all_candidates() {
        let full = perfect_scores();

        let candidates = vec![
            (0.8, vec![1.0; 20]),
            (0.3, vec![0.0; 20]),
            (0.5, vec![1.0; 20]),
        ];

        assert_eq!(
            smallest_accepted(&candidates, &full, 0.05).unwrap(),
            Some(0.5)
        );
    }

    #[test]
    fn no_accepted_candidate_returns_none() {
        let full = perfect_scores();

        let candidates = vec![(0.3, vec![0.0; 20]), (0.5, vec![0.0; 20])];

        assert_eq!(smallest_accepted(&candidates, &full, 0.05).unwrap(), None);
    }

    #[test]
    fn invalid_candidate_fraction_is_rejected() {
        let full = perfect_scores();
        let candidates = vec![(0.0, vec![1.0; 20])];

        assert!(smallest_accepted(&candidates, &full, 0.1).is_err());
    }
}
