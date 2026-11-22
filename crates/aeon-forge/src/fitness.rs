//! Evidence-aware fitness selection.
//!
//! Aggregates evaluation observations per agent version under one profile
//! (matched by profile content hash) and ranks them for improvement:
//! lowest mean score first. A version needs `min_samples` observations to appear,
//! and its priority is discounted by how little evidence there is.

use std::collections::BTreeMap;
use std::fmt;

/// One completed evaluation of one agent version.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// Agent id.
    pub agent_id: String,
    /// Agent version.
    pub agent_version: String,
    /// Content hash of that version.
    pub agent_content_hash: String,
    /// Content hash of the evaluation profile used.
    pub profile_content_hash: String,
    /// Evaluation score.
    pub score: f64,
    /// Gates passed.
    pub passed: bool,
    /// Fraction of cases passed.
    pub case_pass_rate: f64,
    /// Average latency, ms.
    pub avg_latency_ms: f64,
    /// Total cost, USD.
    pub total_cost_usd: f64,
    /// Observation time (RFC 3339, sortable text).
    pub observed_at: String,
}

/// Aggregated fitness of one agent version.
#[derive(Debug, Clone, PartialEq)]
pub struct Fitness {
    /// Agent id.
    pub agent_id: String,
    /// Agent version.
    pub agent_version: String,
    /// Content hash of that version.
    pub agent_content_hash: String,
    /// Observations aggregated.
    pub sample_count: usize,
    /// Mean score.
    pub mean_score: f64,
    /// Fraction of observations that passed their gates.
    pub pass_rate: f64,
    /// Mean case pass rate.
    pub mean_case_pass_rate: f64,
    /// Mean latency, ms.
    pub mean_latency_ms: f64,
    /// Mean cost, USD.
    pub mean_cost_usd: f64,
    /// Latest observation time.
    pub last_observed_at: String,
    /// `n / (n + 3)`: how much to trust the mean.
    pub sample_confidence: f64,
    /// `(1 - mean_score) * confidence`: higher means improve sooner.
    pub improvement_priority: f64,
}

/// Invalid selection parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FitnessError(pub &'static str);

impl fmt::Display for FitnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for FitnessError {}

fn round(x: f64, places: i32) -> f64 {
    let p = 10f64.powi(places);
    (x * p).round() / p
}

/// Rank agent versions for one profile.
///
/// Order: mean score ascending, then sample count descending, then most recent first.
pub fn select_fitness(
    observations: &[Observation],
    profile_content_hash: &str,
    min_samples: usize,
    limit: usize,
) -> Result<Vec<Fitness>, FitnessError> {
    if !(1..=1000).contains(&min_samples) || !(1..=100).contains(&limit) {
        return Err(FitnessError(
            "min_samples must be 1..1000 and limit must be 1..100",
        ));
    }
    let mut groups: BTreeMap<(&str, &str, &str), Vec<&Observation>> = BTreeMap::new();
    for o in observations
        .iter()
        .filter(|o| o.profile_content_hash == profile_content_hash)
    {
        groups
            .entry((&o.agent_id, &o.agent_version, &o.agent_content_hash))
            .or_default()
            .push(o);
    }
    let mut out: Vec<Fitness> = groups
        .into_iter()
        .filter(|(_, v)| v.len() >= min_samples)
        .map(|((agent, version, hash), v)| {
            let n = v.len();
            let nf = n as f64;
            let mean = |f: fn(&Observation) -> f64| v.iter().map(|o| f(o)).sum::<f64>() / nf;
            let mean_score = mean(|o| o.score);
            let confidence = nf / (nf + 3.0);
            Fitness {
                agent_id: agent.to_string(),
                agent_version: version.to_string(),
                agent_content_hash: hash.to_string(),
                sample_count: n,
                mean_score: round(mean_score, 6),
                pass_rate: round(v.iter().filter(|o| o.passed).count() as f64 / nf, 6),
                mean_case_pass_rate: round(mean(|o| o.case_pass_rate), 6),
                mean_latency_ms: round(mean(|o| o.avg_latency_ms), 3),
                mean_cost_usd: round(mean(|o| o.total_cost_usd), 8),
                last_observed_at: v
                    .iter()
                    .map(|o| o.observed_at.as_str())
                    .max()
                    .unwrap_or("")
                    .to_string(),
                sample_confidence: round(confidence, 6),
                improvement_priority: round((1.0 - mean_score) * confidence, 6),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.mean_score
            .total_cmp(&b.mean_score)
            .then_with(|| b.sample_count.cmp(&a.sample_count))
            .then_with(|| b.last_observed_at.cmp(&a.last_observed_at))
    });
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(
        agent: &str,
        version: &str,
        profile: &str,
        score: f64,
        passed: bool,
        at: &str,
    ) -> Observation {
        Observation {
            agent_id: agent.into(),
            agent_version: version.into(),
            agent_content_hash: format!("hash-{agent}-{version}"),
            profile_content_hash: profile.into(),
            score,
            passed,
            case_pass_rate: score,
            avg_latency_ms: 100.0,
            total_cost_usd: 0.5,
            observed_at: at.into(),
        }
    }

    #[test]
    fn aggregates_and_ranks_worst_first() {
        let mut o = vec![];
        for (i, s) in [0.2, 0.4, 0.3].iter().enumerate() {
            o.push(obs(
                "weak",
                "1.0.0",
                "p",
                *s,
                false,
                &format!("2026-01-0{}", i + 1),
            ));
        }
        for (i, s) in [0.9, 0.8, 1.0].iter().enumerate() {
            o.push(obs(
                "strong",
                "1.0.0",
                "p",
                *s,
                true,
                &format!("2026-02-0{}", i + 1),
            ));
        }
        let r = select_fitness(&o, "p", 3, 10).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].agent_id, "weak");
        assert_eq!(r[0].mean_score, 0.3);
        assert_eq!(r[0].pass_rate, 0.0);
        assert_eq!(r[0].sample_confidence, 0.5);
        assert_eq!(r[0].improvement_priority, 0.35);
        assert_eq!(r[0].last_observed_at, "2026-01-03");
        assert_eq!(r[1].agent_id, "strong");
        assert_eq!(r[1].mean_score, 0.9);
        assert_eq!(r[1].pass_rate, 1.0);
    }

    #[test]
    fn needs_min_samples_and_matching_profile() {
        let o = vec![
            obs("a", "1", "p", 0.5, true, "t1"),
            obs("a", "1", "p", 0.5, true, "t2"),
            obs("a", "1", "other", 0.1, false, "t3"),
        ];
        assert!(select_fitness(&o, "p", 3, 10).unwrap().is_empty());
        assert_eq!(select_fitness(&o, "p", 2, 10).unwrap().len(), 1);
        assert_eq!(select_fitness(&o, "other", 1, 10).unwrap().len(), 1);
        assert!(select_fitness(&o, "missing", 1, 10).unwrap().is_empty());
    }

    #[test]
    fn versions_are_separate_groups_and_ties_break_by_samples_then_recency() {
        let o = vec![
            obs("a", "1", "p", 0.5, true, "2026-01-01"),
            obs("a", "1", "p", 0.5, true, "2026-01-02"),
            obs("a", "2", "p", 0.5, true, "2026-03-01"),
            obs("a", "2", "p", 0.5, true, "2026-03-02"),
            obs("a", "2", "p", 0.5, true, "2026-03-03"),
            obs("b", "1", "p", 0.5, true, "2026-05-01"),
            obs("b", "1", "p", 0.5, true, "2026-05-02"),
        ];
        let r = select_fitness(&o, "p", 2, 10).unwrap();
        let order: Vec<String> = r
            .iter()
            .map(|f| format!("{}@{}", f.agent_id, f.agent_version))
            .collect();
        // equal mean: more samples first (a@2), then most recent (b@1 before a@1)
        assert_eq!(order, ["a@2", "b@1", "a@1"]);
    }

    #[test]
    fn limit_and_parameter_bounds() {
        let o: Vec<Observation> = (0..5)
            .map(|i| obs(&format!("a{i}"), "1", "p", 0.1 * i as f64, true, "t"))
            .collect();
        assert_eq!(select_fitness(&o, "p", 1, 2).unwrap().len(), 2);
        assert!(select_fitness(&o, "p", 0, 2).is_err());
        assert!(select_fitness(&o, "p", 1001, 2).is_err());
        assert!(select_fitness(&o, "p", 1, 0).is_err());
        assert!(select_fitness(&o, "p", 1, 101).is_err());
    }
}
