//! Robust score, shortcut reliance and the gap law.
//!
//! A closure test runs the same model under controlled hidden conditions:
//! a clean reference, and shortcuts (answer position, planted hint) that
//! point to the right answer with probability `p`. The robust score S is the
//! worst accuracy over the closure conditions.

use crate::K;
use std::collections::BTreeMap;

/// Shortcut validity on familiar data.
pub const FAMILIAR: f64 = 0.95;

/// Kind of planted shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Shortcut {
    /// Correct answer tends to sit at a fixed position.
    Position,
    /// A hint names an answer.
    Hint,
}

/// One test condition: no shortcut, or a shortcut that is right with probability `p` (in percent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Condition {
    /// No shortcut.
    Clean,
    /// Shortcut right with `percent`% probability.
    Cue(Shortcut, u8),
}

impl Condition {
    /// Probability that the cue points to the correct answer.
    #[must_use]
    pub fn p(self) -> Option<f64> {
        match self {
            Self::Clean => None,
            Self::Cue(_, pct) => Some(f64::from(pct) / 100.0),
        }
    }
}

/// Conditions whose worst case defines S.
pub const CLOSURE: [Condition; 5] = [
    Condition::Clean,
    Condition::Cue(Shortcut::Position, 95),
    Condition::Cue(Shortcut::Position, 0),
    Condition::Cue(Shortcut::Hint, 95),
    Condition::Cue(Shortcut::Hint, 0),
];

/// Held-out shift conditions (not used for S).
pub const HELDOUT: [Condition; 2] = [
    Condition::Cue(Shortcut::Position, 25),
    Condition::Cue(Shortcut::Hint, 25),
];

/// Cue informativeness: `(p - 1/K) / (1 - 1/K)`; 1 = always right, 0 = chance.
#[must_use]
pub fn q_of(p: f64) -> f64 {
    let k = K as f64;
    (p - 1.0 / k) / (1.0 - 1.0 / k)
}

/// Interventional reliance on a shortcut, measured on familiar items:
/// how much more often the model follows the cue when it is present than in
/// the counterfactual prompt without it, rescaled to [0, 1].
#[must_use]
pub fn reliance(follow: usize, follow_counterfactual: usize, n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let k = K as f64;
    (follow as f64 - follow_counterfactual as f64) / n as f64 / (1.0 - 1.0 / k)
}

/// Gap law: predicted accuracy when the cue validity shifts from familiar to `p_new`.
///
/// `predicted = acc_familiar + reliance * (q(p_new) - q(FAMILIAR)) * (1 - 1/K)`.
#[must_use]
pub fn gap_law(acc_familiar: f64, reliance: f64, p_new: f64) -> f64 {
    let k = K as f64;
    acc_familiar + reliance * (q_of(p_new) - q_of(FAMILIAR)) * (1.0 - 1.0 / k)
}

/// Gap-law check for one condition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GapCheck {
    /// Gap-law prediction.
    pub predicted: f64,
    /// Measured accuracy.
    pub actual: f64,
}

impl GapCheck {
    /// `|predicted - actual|`.
    #[must_use]
    pub fn abs_error(&self) -> f64 {
        (self.predicted - self.actual).abs()
    }
}

/// Summary of a closure test.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Worst closure accuracy (robust score S).
    pub robust_s: Option<f64>,
    /// Mean closure accuracy.
    pub closure_mean: Option<f64>,
    /// Mean held-out accuracy.
    pub heldout_mean: Option<f64>,
    /// Mean accuracy on the two familiar (95%) conditions.
    pub familiar_mean: Option<f64>,
    /// Gap-law predictions vs. measurements for the shifted conditions.
    pub gap: BTreeMap<Condition, GapCheck>,
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

/// Summarise per-condition accuracy and measured reliance.
#[must_use]
pub fn summarise(acc: &BTreeMap<Condition, f64>, rel: &BTreeMap<Shortcut, f64>) -> Summary {
    let pick =
        |cs: &[Condition]| -> Vec<f64> { cs.iter().filter_map(|c| acc.get(c).copied()).collect() };
    let closure = pick(&CLOSURE);
    let held = pick(&HELDOUT);
    let familiar = pick(&[
        Condition::Cue(Shortcut::Position, 95),
        Condition::Cue(Shortcut::Hint, 95),
    ]);
    let mut gap = BTreeMap::new();
    for (&s, &r) in rel {
        let Some(&fam) = acc.get(&Condition::Cue(s, 95)) else {
            continue;
        };
        for pct in [25u8, 0] {
            let c = Condition::Cue(s, pct);
            if let Some(&actual) = acc.get(&c) {
                gap.insert(
                    c,
                    GapCheck {
                        predicted: gap_law(fam, r, f64::from(pct) / 100.0),
                        actual,
                    },
                );
            }
        }
    }
    Summary {
        robust_s: closure.iter().copied().reduce(f64::min),
        closure_mean: mean(&closure),
        heldout_mean: mean(&held),
        familiar_mean: mean(&familiar),
        gap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_and_gap_law() {
        assert!((q_of(1.0) - 1.0).abs() < 1e-12);
        assert!(q_of(0.25).abs() < 1e-12);
        // No reliance: accuracy does not move under shift.
        assert!((gap_law(0.8, 0.0, 0.0) - 0.8).abs() < 1e-12);
        // Full reliance, cue flips from 95% right to never right.
        let drop = gap_law(0.95, 1.0, 0.0);
        assert!((drop - (0.95 + (q_of(0.0) - q_of(0.95)) * 0.75)).abs() < 1e-12);
        assert!(drop.abs() < 1e-9);
    }

    #[test]
    fn reliance_scale() {
        assert!((reliance(100, 25, 100) - 1.0).abs() < 1e-12);
        assert_eq!(reliance(5, 5, 10), 0.0);
        assert_eq!(reliance(1, 0, 0), 0.0);
    }

    #[test]
    fn summary_takes_worst_case() {
        let mut acc = BTreeMap::new();
        acc.insert(Condition::Clean, 0.7);
        acc.insert(Condition::Cue(Shortcut::Position, 95), 0.9);
        acc.insert(Condition::Cue(Shortcut::Position, 0), 0.4);
        acc.insert(Condition::Cue(Shortcut::Position, 25), 0.6);
        acc.insert(Condition::Cue(Shortcut::Hint, 95), 0.8);
        acc.insert(Condition::Cue(Shortcut::Hint, 0), 0.6);
        let mut rel = BTreeMap::new();
        rel.insert(Shortcut::Position, 0.5);
        let s = summarise(&acc, &rel);
        assert_eq!(s.robust_s, Some(0.4));
        assert!((s.familiar_mean.unwrap() - 0.85).abs() < 1e-12);
        assert_eq!(s.heldout_mean, Some(0.6));
        assert_eq!(s.gap.len(), 2);
        let g = s.gap[&Condition::Cue(Shortcut::Position, 0)];
        assert!((g.predicted - gap_law(0.9, 0.5, 0.0)).abs() < 1e-12);
        assert!((g.abs_error() - (g.predicted - 0.4).abs()).abs() < 1e-12);
    }
}
