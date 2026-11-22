//! Cheapest-first model cascades.
//!
//! A two-model cascade answers with the small model when its confidence is
//! at least a threshold, otherwise it also runs the large model. Every plan
//! is evaluated on items scored by all candidates.

use std::collections::{BTreeMap, BTreeSet};

/// One scored item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scored {
    /// Answer was correct.
    pub correct: bool,
    /// Model confidence in its answer.
    pub confidence: f64,
}

/// A model and its per-item results.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Name.
    pub name: String,
    /// Cost per item (for example parameters invoked).
    pub cost: f64,
    /// Results keyed by item id.
    pub records: BTreeMap<u64, Scored>,
}

/// A routing plan and its measured trade-off.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Models in order.
    pub models: Vec<String>,
    /// Confidence threshold on the first model (cascades only).
    pub threshold: Option<f64>,
    /// Accuracy on shared items.
    pub accuracy: f64,
    /// Mean cost per item.
    pub avg_cost: f64,
}

/// Cascade planning result.
#[derive(Debug, Clone, PartialEq)]
pub struct CascadeReport {
    /// Shared items used.
    pub common_items: usize,
    /// Most accurate single model.
    pub best_single: Plan,
    /// Cheapest plan within `tolerance` of the best single model's accuracy.
    pub recommended: Plan,
    /// Plans not dominated on (cost, accuracy), cheapest first.
    pub frontier: Vec<Plan>,
}

impl CascadeReport {
    /// Fraction of compute saved by the recommendation vs. the best single model.
    #[must_use]
    pub fn saving(&self) -> f64 {
        if self.best_single.avg_cost > 0.0 {
            1.0 - self.recommended.avg_cost / self.best_single.avg_cost
        } else {
            0.0
        }
    }
}

/// Plan cascades over `candidates`, sweeping `grid` thresholds per pair.
///
/// Returns `None` with fewer than two candidates or fewer than `min_common`
/// shared items.
#[must_use]
pub fn plan(
    candidates: &[Candidate],
    grid: usize,
    tolerance: f64,
    min_common: usize,
) -> Option<CascadeReport> {
    if candidates.len() < 2 || grid < 2 {
        return None;
    }
    let mut common: BTreeSet<u64> = candidates[0].records.keys().copied().collect();
    for c in &candidates[1..] {
        common.retain(|k| c.records.contains_key(k));
    }
    if common.len() < min_common.max(1) {
        return None;
    }
    let items: Vec<u64> = common.into_iter().collect();
    let n = items.len() as f64;
    let acc_of = |c: &Candidate| items.iter().filter(|i| c.records[i].correct).count() as f64 / n;

    let singles: Vec<Plan> = candidates
        .iter()
        .map(|c| Plan {
            models: vec![c.name.clone()],
            threshold: None,
            accuracy: acc_of(c),
            avg_cost: c.cost,
        })
        .collect();
    let mut plans = singles.clone();

    let mut by_cost: Vec<&Candidate> = candidates.iter().collect();
    by_cost.sort_by(|a, b| a.cost.total_cmp(&b.cost));
    for (i, small) in by_cost.iter().enumerate() {
        let mut confs: Vec<f64> = items.iter().map(|j| small.records[j].confidence).collect();
        confs.sort_by(f64::total_cmp);
        for large in &by_cost[i + 1..] {
            for g in 0..grid {
                let tau = if g + 1 == grid {
                    f64::INFINITY
                } else {
                    confs[g * (confs.len() - 1) / (grid - 1)]
                };
                let (mut ok, mut cost) = (0.0f64, 0.0f64);
                for j in &items {
                    let rs = small.records[j];
                    cost += small.cost;
                    if rs.confidence >= tau {
                        ok += f64::from(u8::from(rs.correct));
                    } else {
                        ok += f64::from(u8::from(large.records[j].correct));
                        cost += large.cost;
                    }
                }
                plans.push(Plan {
                    models: vec![small.name.clone(), large.name.clone()],
                    threshold: tau.is_finite().then_some(tau),
                    accuracy: ok / n,
                    avg_cost: cost / n,
                });
            }
        }
    }

    let mut sorted = plans.clone();
    sorted.sort_by(|a, b| {
        a.avg_cost
            .total_cmp(&b.avg_cost)
            .then(b.accuracy.total_cmp(&a.accuracy))
    });
    let mut frontier: Vec<Plan> = Vec::new();
    for p in sorted {
        if frontier
            .last()
            .is_none_or(|last| p.accuracy > last.accuracy + 1e-12)
        {
            frontier.push(p);
        }
    }
    let best_single = singles
        .iter()
        .max_by(|a, b| a.accuracy.total_cmp(&b.accuracy))?
        .clone();
    let recommended = plans
        .iter()
        .filter(|p| p.accuracy >= best_single.accuracy - tolerance)
        .min_by(|a, b| a.avg_cost.total_cmp(&b.avg_cost))?
        .clone();
    Some(CascadeReport {
        common_items: items.len(),
        best_single,
        recommended,
        frontier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, cost: f64, rows: &[(bool, f64)]) -> Candidate {
        Candidate {
            name: name.into(),
            cost,
            records: rows
                .iter()
                .enumerate()
                .map(|(i, &(correct, confidence))| {
                    (
                        i as u64,
                        Scored {
                            correct,
                            confidence,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn cascade_saves_compute() {
        // Small model is right exactly when confident; large model is always right.
        let small_rows: Vec<(bool, f64)> = (0..20)
            .map(|i| (i < 15, if i < 15 { 0.9 } else { 0.3 }))
            .collect();
        let large_rows: Vec<(bool, f64)> = (0..20).map(|_| (true, 0.95)).collect();
        let r = plan(
            &[
                cand("small", 1.0, &small_rows),
                cand("large", 10.0, &large_rows),
            ],
            21,
            0.0,
            20,
        )
        .unwrap();
        assert_eq!(r.best_single.models, vec!["large".to_string()]);
        assert!((r.recommended.accuracy - 1.0).abs() < 1e-12);
        assert_eq!(r.recommended.models.len(), 2);
        assert!(r.saving() > 0.5);
        assert!(r.frontier.windows(2).all(|w| w[1].accuracy > w[0].accuracy));
    }

    #[test]
    fn needs_overlap() {
        let a = cand("a", 1.0, &[(true, 0.5)]);
        let b = cand("b", 2.0, &[(true, 0.5)]);
        assert!(plan(&[a.clone(), b], 5, 0.01, 20).is_none());
        assert!(plan(&[a], 5, 0.01, 1).is_none());
    }
}
