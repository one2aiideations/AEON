//! Parent/candidate comparison.
//!
//! Puts the evaluation of a derived candidate next to the evaluation of its parent: score,
//! latency and cost deltas and a plain outcome label. Before comparing it checks that the pair
//! is comparable: same agent, the candidate really descends from the stated parent, both runs
//! used the identical evaluation profile, and the numbers are in range.
//!
//! A comparison is evidence for a human or a policy, **never** a promotion: every result carries
//! `promotion_eligible = false`.

use crate::evaluation::Score;
use crate::registry::Record;
use std::fmt;

/// The evaluation facts of one run that a comparison needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Content hash of the evaluation profile that produced it.
    pub profile_content_hash: String,
    /// Weighted score, in `[0, 1]`.
    pub score: f64,
    /// Whether every promotion gate passed.
    pub passed: bool,
    /// Mean latency, ms.
    pub avg_latency_ms: f64,
    /// Total cost, USD.
    pub total_cost_usd: f64,
}

impl Run {
    /// Build from a scoring result and the profile's content hash.
    pub fn from_score(score: &Score, profile_content_hash: &str) -> Run {
        Run {
            profile_content_hash: profile_content_hash.to_string(),
            score: score.score,
            passed: score.passed,
            avg_latency_ms: score.avg_latency_ms,
            total_cost_usd: score.total_cost_usd,
        }
    }
}

/// Why two runs cannot be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareError {
    /// The two records belong to different agents.
    DifferentAgents,
    /// Parent and candidate are the same version.
    SameVersion,
    /// The candidate's recorded parent is not the supplied parent.
    NotDescended {
        /// What the candidate says its parent is.
        recorded: Option<String>,
        /// What the caller supplied.
        expected: String,
    },
    /// The runs used different evaluation profiles.
    ProfileMismatch,
    /// A number is not finite, a score is outside `[0, 1]`, or a latency/cost is negative.
    BadNumber(&'static str),
    /// The workload hash is not a SHA-256 hex digest.
    BadWorkloadHash,
}

impl fmt::Display for CompareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompareError::DifferentAgents => {
                f.write_str("parent and candidate belong to different agents")
            }
            CompareError::SameVersion => f.write_str("parent and candidate versions must differ"),
            CompareError::NotDescended { recorded, expected } => {
                write!(f, "candidate lineage {recorded:?} does not reference the supplied parent {expected:?}")
            }
            CompareError::ProfileMismatch => {
                f.write_str("parent and candidate evaluations must use the same profile")
            }
            CompareError::BadNumber(n) => write!(f, "invalid number: {n}"),
            CompareError::BadWorkloadHash => {
                f.write_str("workload_hash must be a SHA-256 hex digest")
            }
        }
    }
}

impl std::error::Error for CompareError {}

/// A before/after pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Delta {
    /// Parent value.
    pub parent: f64,
    /// Candidate value.
    pub candidate: f64,
    /// `candidate - parent`, rounded.
    pub delta: f64,
}

/// Which score is higher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The candidate scored higher.
    CandidateScoreHigher,
    /// The candidate scored lower.
    CandidateScoreLower,
    /// Equal scores.
    ScoreEqual,
}

/// Result of comparing a candidate with its parent.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// Score pair.
    pub score: Delta,
    /// Gate outcome of each run: `(parent, candidate)`.
    pub passed: (bool, bool),
    /// Mean latency pair, ms.
    pub avg_latency_ms: Delta,
    /// Total cost pair, USD.
    pub total_cost_usd: Delta,
    /// Score ordering.
    pub outcome: Outcome,
    /// Hash of the workload both runs claim to have used.
    pub workload_hash: String,
    /// `"execution_records"` when the caller verified the evidence against stored execution
    /// records, otherwise `"caller_asserted"`.
    pub workload_provenance: &'static str,
    /// Always `false`: a comparison never authorises promotion.
    pub promotion_eligible: bool,
}

fn round(x: f64, places: i32) -> f64 {
    let p = 10f64.powi(places);
    (x * p).round() / p
}

fn check(name: &'static str, v: f64, lo: f64, hi: f64) -> Result<f64, CompareError> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(v)
    } else {
        Err(CompareError::BadNumber(name))
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Compare a candidate with its parent.
///
/// `evidence_verified` is the caller's statement that both runs were checked against stored
/// execution records; it only changes the reported provenance, never the numbers.
pub fn compare(
    parent: &Record,
    candidate: &Record,
    parent_run: &Run,
    candidate_run: &Run,
    workload_hash: &str,
    evidence_verified: bool,
) -> Result<Comparison, CompareError> {
    let (pm, cm) = (&parent.manifest, &candidate.manifest);
    if pm.agent_id != cm.agent_id || parent.tenant_id != candidate.tenant_id {
        return Err(CompareError::DifferentAgents);
    }
    if pm.version == cm.version {
        return Err(CompareError::SameVersion);
    }
    let expected = format!("{}@{}", pm.agent_id, pm.version);
    if candidate.parent_artifact_id.as_deref() != Some(expected.as_str()) {
        return Err(CompareError::NotDescended {
            recorded: candidate.parent_artifact_id.clone(),
            expected,
        });
    }
    if parent_run.profile_content_hash != candidate_run.profile_content_hash {
        return Err(CompareError::ProfileMismatch);
    }
    if !is_sha256_hex(workload_hash) {
        return Err(CompareError::BadWorkloadHash);
    }
    let (ps, cs) = (
        check("parent score", parent_run.score, 0.0, 1.0)?,
        check("candidate score", candidate_run.score, 0.0, 1.0)?,
    );
    let (pl, cl) = (
        check("parent latency", parent_run.avg_latency_ms, 0.0, f64::MAX)?,
        check(
            "candidate latency",
            candidate_run.avg_latency_ms,
            0.0,
            f64::MAX,
        )?,
    );
    let (pc, cc) = (
        check("parent cost", parent_run.total_cost_usd, 0.0, f64::MAX)?,
        check(
            "candidate cost",
            candidate_run.total_cost_usd,
            0.0,
            f64::MAX,
        )?,
    );
    let score_delta = round(cs - ps, 6);
    Ok(Comparison {
        score: Delta {
            parent: ps,
            candidate: cs,
            delta: score_delta,
        },
        passed: (parent_run.passed, candidate_run.passed),
        avg_latency_ms: Delta {
            parent: pl,
            candidate: cl,
            delta: round(cl - pl, 3),
        },
        total_cost_usd: Delta {
            parent: pc,
            candidate: cc,
            delta: round(cc - pc, 8),
        },
        outcome: if score_delta > 0.0 {
            Outcome::CandidateScoreHigher
        } else if score_delta < 0.0 {
            Outcome::CandidateScoreLower
        } else {
            Outcome::ScoreEqual
        },
        workload_hash: workload_hash.to_ascii_lowercase(),
        workload_provenance: if evidence_verified {
            "execution_records"
        } else {
            "caller_asserted"
        },
        promotion_eligible: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::generate;
    use crate::manifest::{Manifest, Origin};
    use crate::registry::Registry;
    use std::collections::{BTreeMap, BTreeSet};

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn base() -> Manifest {
        Manifest {
            agent_id: "a".into(),
            version: "1.0.0".into(),
            max_steps: 10,
            timeout_seconds: 60,
            model_parameters: BTreeMap::new(),
            permissions: BTreeSet::new(),
            tools: BTreeSet::new(),
            origin: Origin::Authored,
            created_at: "2026-01-01T00:00:00Z".into(),
            created_by: "me".into(),
            source_refs: vec![],
        }
    }

    fn pair() -> (Record, Record) {
        let mut r = Registry::new();
        r.create("t", base(), None).unwrap();
        generate(
            &mut r,
            "t",
            "a",
            "1.0.0",
            "1.1.0",
            &["behavior.max_steps.increment"],
            1,
            "me",
            "2026-02-01T00:00:00Z",
        )
        .unwrap();
        (
            r.get("t", "a", "1.0.0").unwrap().clone(),
            r.get("t", "a", "1.1.0").unwrap().clone(),
        )
    }

    fn run(score: f64, latency: f64, cost: f64, passed: bool) -> Run {
        Run {
            profile_content_hash: "p".into(),
            score,
            passed,
            avg_latency_ms: latency,
            total_cost_usd: cost,
        }
    }

    #[test]
    fn deltas_and_outcome() {
        let (p, c) = pair();
        let r = compare(
            &p,
            &c,
            &run(0.5, 200.0, 0.30, false),
            &run(0.75, 150.5, 0.25, true),
            HASH,
            false,
        )
        .unwrap();
        assert_eq!(
            r.score,
            Delta {
                parent: 0.5,
                candidate: 0.75,
                delta: 0.25
            }
        );
        assert_eq!(r.avg_latency_ms.delta, -49.5);
        assert_eq!(r.total_cost_usd.delta, -0.05);
        assert_eq!(r.passed, (false, true));
        assert_eq!(r.outcome, Outcome::CandidateScoreHigher);
        assert_eq!(r.workload_provenance, "caller_asserted");
        assert!(!r.promotion_eligible);
        let lower = compare(
            &p,
            &c,
            &run(0.9, 1.0, 0.0, true),
            &run(0.8, 1.0, 0.0, true),
            HASH,
            true,
        )
        .unwrap();
        assert_eq!(lower.outcome, Outcome::CandidateScoreLower);
        assert_eq!(lower.workload_provenance, "execution_records");
        let eq = compare(
            &p,
            &c,
            &run(0.7, 1.0, 0.0, true),
            &run(0.7, 1.0, 0.0, true),
            HASH,
            false,
        )
        .unwrap();
        assert_eq!(eq.outcome, Outcome::ScoreEqual);
        assert!(!eq.promotion_eligible);
    }

    #[test]
    fn rounding_matches_the_reference_precision() {
        let (p, c) = pair();
        let r = compare(
            &p,
            &c,
            &run(0.1, 0.1, 0.1, true),
            &run(0.3, 0.3, 0.3, true),
            HASH,
            false,
        )
        .unwrap();
        assert_eq!(r.score.delta, 0.2);
        assert_eq!(r.avg_latency_ms.delta, 0.2);
        assert_eq!(r.total_cost_usd.delta, 0.2);
    }

    #[test]
    fn lineage_is_enforced() {
        let (p, c) = pair();
        // swapped: the "candidate" (1.0.0) has no parent
        assert!(matches!(
            compare(
                &c,
                &p,
                &run(0.5, 1.0, 0.0, true),
                &run(0.5, 1.0, 0.0, true),
                HASH,
                false
            ),
            Err(CompareError::NotDescended { recorded: None, .. })
        ));
        assert_eq!(
            compare(
                &p,
                &p,
                &run(0.5, 1.0, 0.0, true),
                &run(0.5, 1.0, 0.0, true),
                HASH,
                false
            ),
            Err(CompareError::SameVersion)
        );
        let mut other = c.clone();
        other.manifest.agent_id = "b".into();
        assert_eq!(
            compare(
                &p,
                &other,
                &run(0.5, 1.0, 0.0, true),
                &run(0.5, 1.0, 0.0, true),
                HASH,
                false
            ),
            Err(CompareError::DifferentAgents)
        );
        let mut tenant = c.clone();
        tenant.tenant_id = "other".into();
        assert_eq!(
            compare(
                &p,
                &tenant,
                &run(0.5, 1.0, 0.0, true),
                &run(0.5, 1.0, 0.0, true),
                HASH,
                false
            ),
            Err(CompareError::DifferentAgents)
        );
    }

    #[test]
    fn profile_and_workload_checks() {
        let (p, c) = pair();
        let mut other_profile = run(0.5, 1.0, 0.0, true);
        other_profile.profile_content_hash = "q".into();
        assert_eq!(
            compare(
                &p,
                &c,
                &run(0.5, 1.0, 0.0, true),
                &other_profile,
                HASH,
                false
            ),
            Err(CompareError::ProfileMismatch)
        );
        for bad in ["", "abc", &"g".repeat(64), &HASH[..63]] {
            assert_eq!(
                compare(
                    &p,
                    &c,
                    &run(0.5, 1.0, 0.0, true),
                    &run(0.5, 1.0, 0.0, true),
                    bad,
                    false
                ),
                Err(CompareError::BadWorkloadHash),
                "{bad:?}"
            );
        }
        // upper-case digests are accepted and normalised
        let r = compare(
            &p,
            &c,
            &run(0.5, 1.0, 0.0, true),
            &run(0.5, 1.0, 0.0, true),
            &HASH.to_uppercase(),
            false,
        )
        .unwrap();
        assert_eq!(r.workload_hash, HASH);
    }

    #[test]
    fn numbers_are_validated() {
        let (p, c) = pair();
        let good = run(0.5, 1.0, 0.0, true);
        for (bad, name) in [
            (run(1.5, 1.0, 0.0, true), "candidate score"),
            (run(-0.1, 1.0, 0.0, true), "candidate score"),
            (run(f64::NAN, 1.0, 0.0, true), "candidate score"),
            (run(0.5, -1.0, 0.0, true), "candidate latency"),
            (run(0.5, f64::INFINITY, 0.0, true), "candidate latency"),
            (run(0.5, 1.0, -0.01, true), "candidate cost"),
        ] {
            assert_eq!(
                compare(&p, &c, &good, &bad, HASH, false),
                Err(CompareError::BadNumber(name))
            );
        }
        assert_eq!(
            compare(&p, &c, &run(2.0, 1.0, 0.0, true), &good, HASH, false),
            Err(CompareError::BadNumber("parent score"))
        );
    }

    #[test]
    fn run_from_score_copies_the_facts() {
        use crate::evaluation::{
            score, Assertion, Case, Gates, Profile, Scoring, Submitted, Value,
        };
        let profile = Profile {
            cases: vec![Case {
                case_id: "c".into(),
                assertions: vec![Assertion::ExactMatch(Value::Str("x".into()))],
            }],
            scoring: Scoring {
                correctness_weight: 1.0,
                latency_weight: 0.0,
                cost_weight: 0.0,
            },
            gates: Gates {
                min_case_pass_rate: 1.0,
                max_avg_latency_ms: 100.0,
                max_total_cost_usd: 1.0,
            },
        };
        let s = score(
            &profile,
            &[Submitted {
                case_id: "c".into(),
                output: Some(Value::Str("x".into())),
                latency_ms: 10.0,
                cost_usd: 0.25,
            }],
        )
        .unwrap();
        let r = Run::from_score(&s, "ph");
        assert_eq!(
            (r.score, r.passed, r.avg_latency_ms, r.total_cost_usd),
            (1.0, true, 10.0, 0.25)
        );
        assert_eq!(r.profile_content_hash, "ph");
    }

    #[test]
    fn errors_display() {
        assert!(CompareError::SameVersion.to_string().contains("differ"));
        assert!(CompareError::BadNumber("x").to_string().contains('x'));
    }
}
