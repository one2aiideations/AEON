//! Model selection by robust score.

/// One evaluated model.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Name.
    pub name: String,
    /// Parameter count, if known.
    pub parameters: Option<u64>,
    /// Robust score S.
    pub robust_s: f64,
}

/// Rank by S (highest first) and recommend: the cheapest model with
/// `S >= threshold`, or the highest S when no threshold is given.
#[must_use]
pub fn leaderboard(mut rows: Vec<Entry>, threshold: Option<f64>) -> (Vec<Entry>, Option<Entry>) {
    rows.sort_by(|a, b| b.robust_s.total_cmp(&a.robust_s));
    let rec = match threshold {
        Some(t) => rows
            .iter()
            .filter(|r| r.robust_s >= t)
            .filter_map(|r| r.parameters.map(|p| (p, r)))
            .min_by_key(|(p, _)| *p)
            .map(|(_, r)| r.clone()),
        None => rows.first().cloned(),
    };
    (rows, rec)
}

/// Pick the merge weight `t` with the highest S; ties go to the `t` closest to 0.5.
#[must_use]
pub fn choose_merge_weight(trials: &[(f64, f64)]) -> Option<f64> {
    trials
        .iter()
        .filter(|(t, s)| t.is_finite() && s.is_finite())
        .max_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then((b.0 - 0.5).abs().total_cmp(&(a.0 - 0.5).abs()))
        })
        .map(|&(t, _)| t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, p: u64, s: f64) -> Entry {
        Entry {
            name: name.into(),
            parameters: Some(p),
            robust_s: s,
        }
    }

    #[test]
    fn cheapest_above_threshold() {
        let rows = vec![e("big", 70, 0.8), e("mid", 8, 0.62), e("tiny", 1, 0.4)];
        let (ranked, rec) = leaderboard(rows.clone(), Some(0.6));
        assert_eq!(ranked[0].name, "big");
        assert_eq!(rec.unwrap().name, "mid");
        assert_eq!(leaderboard(rows.clone(), None).1.unwrap().name, "big");
        assert!(leaderboard(rows, Some(0.9)).1.is_none());
    }

    #[test]
    fn merge_weight_ties_prefer_half() {
        assert_eq!(choose_merge_weight(&[(0.2, 0.7), (0.5, 0.7), (0.9, 0.6)]), Some(0.5));
        assert_eq!(choose_merge_weight(&[(0.3, 0.1), (0.8, 0.9)]), Some(0.8));
        assert_eq!(choose_merge_weight(&[]), None);
    }
}
