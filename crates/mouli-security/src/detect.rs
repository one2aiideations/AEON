//! Anomaly detectors. All are pure functions over values the caller supplies.

use std::collections::{HashMap, HashSet};

/// Spread used instead of zero so a flat history still flags any change (finite, deterministic).
const MIN_SPREAD: f64 = 1e-6;
/// Default z-score alarm level.
pub const Z_THRESHOLD: f64 = 3.0;
/// Default modified-z (MAD) alarm level (Iglewicz and Hoaglin).
pub const MAD_THRESHOLD: f64 = 3.5;

/// A comparison for [`check_thresholds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Gte,
    /// Less than.
    Lt,
    /// Less than or equal.
    Lte,
    /// Equal.
    Eq,
    /// Not equal.
    Neq,
}

/// A metric limit with a severity label.
#[derive(Debug, Clone, PartialEq)]
pub struct Threshold {
    /// Metric the limit applies to.
    pub metric: String,
    /// Comparison that signals a breach.
    pub cmp: Cmp,
    /// Limit value.
    pub value: f64,
    /// Severity label reported on breach.
    pub severity: String,
}

/// A breached threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct Breach {
    /// The threshold that was breached.
    pub threshold: Threshold,
    /// The observed value.
    pub actual: f64,
    /// Human-readable reason.
    pub explanation: String,
}

/// Returns every threshold for `metric` that `actual` breaches. A non-finite `actual` breaches
/// nothing.
pub fn check_thresholds(thresholds: &[Threshold], metric: &str, actual: f64) -> Vec<Breach> {
    if !actual.is_finite() {
        return Vec::new();
    }
    thresholds
        .iter()
        .filter(|t| t.metric == metric)
        .filter(|t| match t.cmp {
            Cmp::Gt => actual > t.value,
            Cmp::Gte => actual >= t.value,
            Cmp::Lt => actual < t.value,
            Cmp::Lte => actual <= t.value,
            Cmp::Eq => actual == t.value,
            Cmp::Neq => actual != t.value,
        })
        .map(|t| Breach {
            explanation: format!(
                "{} breached: actual {actual}, limit {:?} {}",
                t.metric, t.cmp, t.value
            ),
            threshold: t.clone(),
            actual,
        })
        .collect()
}

/// Too many events inside a time window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Velocity {
    /// Events found in the window.
    pub count: usize,
    /// Count that triggers the alarm.
    pub threshold: usize,
    /// Window length in seconds.
    pub window_secs: i64,
    /// Human-readable reason.
    pub explanation: String,
}

/// Counts `timestamps` (epoch seconds) in `[now - window_secs, now]`; alarms at `threshold` or
/// more. Timestamps in the future are ignored. A zero threshold or non-positive window never
/// alarms.
pub fn velocity(
    timestamps: &[i64],
    now: i64,
    window_secs: i64,
    threshold: usize,
) -> Option<Velocity> {
    if threshold == 0 || window_secs <= 0 {
        return None;
    }
    let start = now.saturating_sub(window_secs);
    let count = timestamps
        .iter()
        .filter(|t| **t >= start && **t <= now)
        .count();
    (count >= threshold).then(|| Velocity {
        count,
        threshold,
        window_secs,
        explanation: format!("{count} events within {window_secs}s (threshold {threshold})"),
    })
}

/// A step that appeared before a step that should have preceded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderViolation {
    /// The step seen too early.
    pub early: String,
    /// The step that should have come first.
    pub should_precede: String,
    /// Human-readable reason.
    pub explanation: String,
}

/// Compares an observed sequence with the expected order. Each out-of-order pair is reported
/// once. Steps that are not in `expected` are ignored; repeats use their first occurrence.
pub fn check_sequence(expected: &[&str], actual: &[&str]) -> Vec<OrderViolation> {
    let rank = |s: &str| expected.iter().position(|e| *e == s);
    let mut seen: Vec<(&str, usize)> = Vec::new();
    let mut out = Vec::new();
    let mut reported: HashSet<(String, String)> = HashSet::new();
    for step in actual {
        let Some(r) = rank(step) else { continue };
        if seen.iter().any(|(s, _)| s == step) {
            continue;
        }
        for (prev, prev_rank) in &seen {
            if *prev_rank > r && reported.insert(((*prev).to_string(), (*step).to_string())) {
                out.push(OrderViolation {
                    early: (*prev).to_string(),
                    should_precede: (*step).to_string(),
                    explanation: format!(
                        "{prev} appeared before {step}, but {step} should come first"
                    ),
                });
            }
        }
        seen.push((step, r));
    }
    out
}

/// Which state changes are allowed, per state.
#[derive(Debug, Clone, Default)]
pub struct TransitionRules {
    allowed: HashMap<String, HashSet<String>>,
}

/// An unexpected state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadTransition {
    /// Previous state.
    pub from: String,
    /// New state.
    pub to: String,
    /// Human-readable reason.
    pub explanation: String,
}

impl TransitionRules {
    /// An empty rule set (every known-state change is then a violation).
    pub fn new() -> Self {
        Self::default()
    }

    /// Allows `from` to move to each state in `to`.
    pub fn allow(mut self, from: &str, to: &[&str]) -> Self {
        self.allowed
            .entry(from.to_string())
            .or_default()
            .extend(to.iter().map(|s| (*s).to_string()));
        self
    }

    /// Checks one change. No change, or a change out of a state the rules do not mention, is
    /// not judged (the rules say nothing about it).
    pub fn check(&self, from: &str, to: &str) -> Option<BadTransition> {
        if from == to {
            return None;
        }
        let allowed = self.allowed.get(from)?;
        (!allowed.contains(to)).then(|| BadTransition {
            from: from.to_string(),
            to: to.to_string(),
            explanation: format!("invalid state transition {from} -> {to}"),
        })
    }
}

/// A value far from its history.
#[derive(Debug, Clone, PartialEq)]
pub struct Deviation {
    /// `"z_score"` or `"mad"`.
    pub method: &'static str,
    /// The observed value.
    pub actual: f64,
    /// The history's centre (mean or median).
    pub expected: f64,
    /// Signed score (z or modified z).
    pub score: f64,
    /// The alarm level that was exceeded.
    pub threshold: f64,
    /// Human-readable reason.
    pub explanation: String,
}

fn clean(history: &[f64]) -> Vec<f64> {
    history.iter().copied().filter(|v| v.is_finite()).collect()
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    match n {
        0 => 0.0,
        _ if n % 2 == 1 => sorted.get(n / 2).copied().unwrap_or(0.0),
        _ => {
            let (a, b) = (sorted.get(n / 2 - 1), sorted.get(n / 2));
            (a.copied().unwrap_or(0.0) + b.copied().unwrap_or(0.0)) / 2.0
        }
    }
}

/// Z-score against the sample mean and standard deviation of `history`. Needs at least 2 finite
/// history values and a finite `current`; alarms when `|z| > threshold`.
pub fn z_score(history: &[f64], current: f64, threshold: f64) -> Option<Deviation> {
    let h = clean(history);
    if h.len() < 2 || !current.is_finite() {
        return None;
    }
    let n = h.len() as f64;
    let mean = h.iter().sum::<f64>() / n;
    let var = h.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let sd = var.sqrt().max(MIN_SPREAD);
    let z = (current - mean) / sd;
    (z.abs() > threshold).then(|| Deviation {
        method: "z_score",
        actual: current,
        expected: mean,
        score: z,
        threshold,
        explanation: format!("z-score {z:.2} exceeds {threshold}; mean {mean:.2}"),
    })
}

/// Modified z-score from the median absolute deviation (robust to outliers in `history`). Needs
/// at least 3 finite history values and a finite `current`; alarms when `|score| > threshold`.
pub fn mad(history: &[f64], current: f64, threshold: f64) -> Option<Deviation> {
    let mut h = clean(history);
    if h.len() < 3 || !current.is_finite() {
        return None;
    }
    h.sort_by(f64::total_cmp);
    let med = median(&h);
    let mut devs: Vec<f64> = h.iter().map(|v| (v - med).abs()).collect();
    devs.sort_by(f64::total_cmp);
    let spread = median(&devs).max(MIN_SPREAD);
    let score = 0.6745 * (current - med) / spread;
    (score.abs() > threshold).then(|| Deviation {
        method: "mad",
        actual: current,
        expected: med,
        score,
        threshold,
        explanation: format!("modified z {score:.2} exceeds {threshold}; median {med:.2}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn th(cmp: Cmp, value: f64) -> Threshold {
        Threshold {
            metric: "amount".into(),
            cmp,
            value,
            severity: "HIGH".into(),
        }
    }

    #[test]
    fn thresholds_each_comparison_at_the_boundary() {
        let ts = [
            th(Cmp::Gt, 10.0),
            th(Cmp::Gte, 10.0),
            th(Cmp::Lt, 10.0),
            th(Cmp::Lte, 10.0),
            th(Cmp::Eq, 10.0),
            th(Cmp::Neq, 10.0),
        ];
        let at = |v: f64| {
            check_thresholds(&ts, "amount", v)
                .iter()
                .map(|b| b.threshold.cmp)
                .collect::<Vec<_>>()
        };
        assert_eq!(at(10.0), vec![Cmp::Gte, Cmp::Lte, Cmp::Eq]);
        assert_eq!(at(11.0), vec![Cmp::Gt, Cmp::Gte, Cmp::Neq]);
        assert_eq!(at(9.0), vec![Cmp::Lt, Cmp::Lte, Cmp::Neq]);
    }

    #[test]
    fn thresholds_ignore_other_metrics_and_nonfinite_values() {
        let ts = [th(Cmp::Gt, 1.0)];
        assert!(check_thresholds(&ts, "other", 99.0).is_empty());
        assert!(check_thresholds(&ts, "amount", f64::NAN).is_empty());
        assert!(check_thresholds(&ts, "amount", f64::INFINITY).is_empty());
        let b = check_thresholds(&ts, "amount", 5.0);
        assert_eq!(b[0].actual, 5.0);
        assert!(b[0].explanation.contains("amount breached"));
    }

    #[test]
    fn velocity_window_edges_are_inclusive() {
        let now = 1_000;
        let stamps = [1_000 - 60, 1_000 - 59, 1_000, 1_000 + 1, 1_000 - 61];
        // In [940, 1000]: 940, 941, 1000 are in; 1001 (future) and 939 are out.
        let v = velocity(&stamps, now, 60, 3).unwrap();
        assert_eq!(v.count, 3);
        assert!(velocity(&stamps, now, 60, 4).is_none());
        assert_eq!(velocity(&stamps, now, 60, 1).unwrap().count, 3);
    }

    #[test]
    fn velocity_degenerate_settings_never_alarm() {
        assert!(velocity(&[1, 2, 3], 3, 60, 0).is_none());
        assert!(velocity(&[1, 2, 3], 3, 0, 1).is_none());
        assert!(velocity(&[1, 2, 3], 3, -5, 1).is_none());
        assert!(velocity(&[], 3, 60, 1).is_none());
        assert!(velocity(&[i64::MIN], i64::MIN, 60, 1).is_some());
    }

    const PO: [&str; 5] = [
        "CREATE_PO",
        "APPROVE_PO",
        "RECEIVE_GOODS",
        "INVOICE",
        "PAYMENT",
    ];

    #[test]
    fn sequence_in_order_and_with_gaps_is_fine() {
        assert!(check_sequence(&PO, &PO).is_empty());
        assert!(check_sequence(&PO, &["CREATE_PO", "INVOICE"]).is_empty());
        assert!(check_sequence(&PO, &[]).is_empty());
        assert!(check_sequence(&PO, &["UNRELATED", "CREATE_PO"]).is_empty());
    }

    #[test]
    fn sequence_inversions_are_reported_once_per_pair() {
        let v = check_sequence(&PO, &["CREATE_PO", "PAYMENT", "APPROVE_PO", "INVOICE"]);
        let pairs: Vec<_> = v
            .iter()
            .map(|x| (x.early.as_str(), x.should_precede.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("PAYMENT", "APPROVE_PO"), ("PAYMENT", "INVOICE")]
        );
        let rep = check_sequence(&PO, &["PAYMENT", "CREATE_PO", "PAYMENT", "CREATE_PO"]);
        assert_eq!(rep.len(), 1);
        assert!(rep[0]
            .explanation
            .contains("PAYMENT appeared before CREATE_PO"));
    }

    #[test]
    fn transitions_judge_only_known_states() {
        let rules = TransitionRules::new()
            .allow("ACTIVE", &["BLOCKED", "REVIEW"])
            .allow("BLOCKED", &["REVIEW", "TERMINATED"]);
        assert!(rules.check("ACTIVE", "BLOCKED").is_none());
        assert!(rules.check("BLOCKED", "REVIEW").is_none());
        let bad = rules.check("BLOCKED", "ACTIVE").unwrap();
        assert_eq!((bad.from.as_str(), bad.to.as_str()), ("BLOCKED", "ACTIVE"));
        assert!(rules.check("ACTIVE", "TERMINATED").is_some());
        assert!(rules.check("ACTIVE", "ACTIVE").is_none());
        assert!(rules.check("MYSTERY", "ACTIVE").is_none());
        assert!(TransitionRules::new().check("A", "B").is_none());
    }

    #[test]
    fn z_score_needs_history_and_flags_outliers() {
        let h = [10.0, 11.0, 9.0, 10.0, 10.5, 9.5];
        assert!(z_score(&h, 10.2, Z_THRESHOLD).is_none());
        let d = z_score(&h, 40.0, Z_THRESHOLD).unwrap();
        assert_eq!(d.method, "z_score");
        assert!(d.score > 3.0 && (d.expected - 10.0).abs() < 1e-9);
        assert!(z_score(&h, -20.0, Z_THRESHOLD).unwrap().score < -3.0);
        assert!(z_score(&[5.0], 99.0, Z_THRESHOLD).is_none());
        assert!(z_score(&h, f64::NAN, Z_THRESHOLD).is_none());
    }

    #[test]
    fn z_score_exact_value() {
        // mean 5, sample sd = sqrt(((-1)^2 + 0 + 1^2) / 2) = 1; z = (9 - 5) / 1 = 4.
        let d = z_score(&[4.0, 5.0, 6.0], 9.0, 3.0).unwrap();
        assert!((d.score - 4.0).abs() < 1e-12 && (d.expected - 5.0).abs() < 1e-12);
    }

    #[test]
    fn flat_history_flags_any_change_but_not_sameness() {
        let flat = [7.0, 7.0, 7.0, 7.0];
        assert!(z_score(&flat, 7.0, Z_THRESHOLD).is_none());
        assert!(z_score(&flat, 7.5, Z_THRESHOLD).is_some());
        assert!(mad(&flat, 7.0, MAD_THRESHOLD).is_none());
        assert!(mad(&flat, 7.5, MAD_THRESHOLD).is_some());
    }

    #[test]
    fn nonfinite_history_values_are_dropped() {
        let h = [10.0, f64::NAN, 11.0, f64::INFINITY, 9.0, 10.0];
        assert!(z_score(&h, 10.0, Z_THRESHOLD).is_none());
        assert!(z_score(&[f64::NAN, 1.0, f64::NAN], 5.0, 3.0).is_none());
    }

    #[test]
    fn mad_resists_outliers_that_fool_the_z_score() {
        // One huge past value inflates the standard deviation, hiding a moderate spike.
        let h = [10.0, 10.0, 11.0, 9.0, 10.0, 10.0, 1000.0];
        assert!(z_score(&h, 40.0, Z_THRESHOLD).is_none());
        let d = mad(&h, 40.0, MAD_THRESHOLD).unwrap();
        assert_eq!(d.method, "mad");
        assert!((d.expected - 10.0).abs() < 1e-12);
    }

    #[test]
    fn mad_exact_value_and_minimum_history() {
        // median 5, deviations [1, 0, 1, 4] -> sorted [0, 1, 1, 4], MAD 1; score = 0.6745 * 10 / 1.
        let d = mad(&[4.0, 5.0, 6.0, 9.0], 15.0, 3.5).unwrap();
        assert!((d.expected - 5.5).abs() < 1e-12 || (d.expected - 5.0).abs() < 1e-12);
        assert!(mad(&[1.0, 2.0], 99.0, 3.5).is_none());
        assert!(mad(&[1.0, 2.0, 3.0], f64::INFINITY, 3.5).is_none());
    }

    #[test]
    fn detectors_never_panic_on_extremes() {
        let h = [f64::MAX, f64::MIN, 0.0, f64::MAX];
        let _ = z_score(&h, f64::MAX, 3.0);
        let _ = mad(&h, f64::MIN, 3.5);
        let _ = velocity(&[i64::MAX, i64::MIN], i64::MAX, i64::MAX, 1);
        let _ = check_sequence(&[], &["A", "B"]);
    }
}
