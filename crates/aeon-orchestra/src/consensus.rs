//! Trust-weighted consensus between agent proposals.
//!
//! Each agent proposes an answer with a confidence. Identical answers pool
//! their weight `trust * confidence`. The winner must hold at least
//! `quorum` of the total weight, otherwise there is no consensus and the
//! decision should go to a stronger model or to the user.

use crate::registry::AgentId;
use std::collections::BTreeMap;

/// One proposal.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// Proposing agent.
    pub agent: AgentId,
    /// Proposed answer (normalised by the caller).
    pub answer: String,
    /// Agent confidence in [0, 1].
    pub confidence: f32,
    /// Agent trust in [0, 1].
    pub trust: f32,
}

/// Consensus outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Agreed answer, its share of the weight, and supporting agents.
    Agreed {
        /// Answer.
        answer: String,
        /// Share of total weight (0..=1).
        share: f32,
        /// Agents that proposed it.
        supporters: Vec<AgentId>,
    },
    /// No answer reached the quorum; the leading answer is given.
    Split {
        /// Leading answer.
        leader: String,
        /// Its share.
        share: f32,
    },
    /// No usable proposals.
    Empty,
}

/// Decide with `quorum` in (0, 1].
#[must_use]
pub fn decide(proposals: &[Proposal], quorum: f32) -> Outcome {
    let mut pools: BTreeMap<&str, (f32, Vec<AgentId>)> = BTreeMap::new();
    let mut total = 0.0f32;
    for p in proposals {
        let w = p.trust.clamp(0.0, 1.0) * p.confidence.clamp(0.0, 1.0);
        if !w.is_finite() || w <= 0.0 {
            continue;
        }
        total += w;
        let e = pools.entry(p.answer.as_str()).or_default();
        e.0 += w;
        e.1.push(p.agent);
    }
    let Some((answer, (weight, supporters))) = pools
        .into_iter()
        .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0).then(b.0.cmp(a.0)))
    else {
        return Outcome::Empty;
    };
    let share = weight / total;
    if share >= quorum {
        Outcome::Agreed {
            answer: answer.to_owned(),
            share,
            supporters,
        }
    } else {
        Outcome::Split {
            leader: answer.to_owned(),
            share,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(agent: AgentId, answer: &str, confidence: f32, trust: f32) -> Proposal {
        Proposal {
            agent,
            answer: answer.into(),
            confidence,
            trust,
        }
    }

    #[test]
    fn trusted_majority_wins() {
        let v = [
            p(1, "A", 0.9, 0.9),
            p(2, "A", 0.6, 0.8),
            p(3, "B", 1.0, 0.3),
        ];
        match decide(&v, 0.6) {
            Outcome::Agreed {
                answer,
                share,
                supporters,
            } => {
                assert_eq!(answer, "A");
                assert!(share > 0.7);
                assert_eq!(supporters, vec![1, 2]);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn split_and_empty() {
        let v = [p(1, "A", 1.0, 0.5), p(2, "B", 1.0, 0.5)];
        assert!(matches!(decide(&v, 0.6), Outcome::Split { .. }));
        assert_eq!(decide(&[], 0.5), Outcome::Empty);
        assert_eq!(decide(&[p(1, "A", 0.0, 1.0)], 0.5), Outcome::Empty);
    }
}
