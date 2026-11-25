//! Declarative, deterministic evaluation.
//!
//! A [`Profile`] holds cases, scoring weights and promotion gates as data.
//! [`score`] compares *submitted* outputs against the cases. It never runs an
//! agent, so scoring is repeatable and has no side effects.
//!
//! Not ported: the `json_schema_valid` assertion, which needs a JSON Schema
//! validator. Add it once the workspace takes such a dependency.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A JSON-like value for case inputs and agent outputs.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Number.
    Num(f64),
    /// Text.
    Str(String),
    /// List.
    List(Vec<Value>),
    /// Object.
    Map(BTreeMap<String, Value>),
}

impl Value {
    fn contains_text(&self, needle: &str) -> bool {
        match self {
            Value::Str(s) => s.contains(needle),
            Value::List(items) => items.iter().any(|v| v.contains_text(needle)),
            Value::Map(m) => m.values().any(|v| v.contains_text(needle)),
            _ => false,
        }
    }
}

/// One check on an output.
#[derive(Debug, Clone, PartialEq)]
pub enum Assertion {
    /// Output equals the value.
    ExactMatch(Value),
    /// Some string anywhere in the output contains the text.
    Contains(String),
    /// Output is an object with all these keys.
    RequiredFields(Vec<String>),
}

impl Assertion {
    fn name(&self) -> &'static str {
        match self {
            Assertion::ExactMatch(_) => "exact_match",
            Assertion::Contains(_) => "contains",
            Assertion::RequiredFields(_) => "required_fields",
        }
    }

    fn check(&self, actual: Option<&Value>) -> bool {
        match (self, actual) {
            (Assertion::ExactMatch(expected), Some(v)) => v == expected,
            (Assertion::ExactMatch(expected), None) => *expected == Value::Null,
            (Assertion::Contains(text), Some(v)) => v.contains_text(text),
            (Assertion::RequiredFields(fields), Some(Value::Map(m))) => {
                fields.iter().all(|f| m.contains_key(f))
            }
            _ => false,
        }
    }
}

/// One evaluation case.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    /// Unique id within the profile.
    pub case_id: String,
    /// All assertions must pass.
    pub assertions: Vec<Assertion>,
}

/// Score weights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scoring {
    /// Weight of the case pass rate.
    pub correctness_weight: f64,
    /// Weight of the latency score.
    pub latency_weight: f64,
    /// Weight of the cost score.
    pub cost_weight: f64,
}

/// Promotion gates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gates {
    /// Minimum fraction of cases that pass.
    pub min_case_pass_rate: f64,
    /// Maximum average latency, ms (> 0).
    pub max_avg_latency_ms: f64,
    /// Maximum total cost, USD.
    pub max_total_cost_usd: f64,
}

/// A versioned evaluation profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// Cases, 1 to 500.
    pub cases: Vec<Case>,
    /// Weights.
    pub scoring: Scoring,
    /// Gates.
    pub gates: Gates,
}

/// Output submitted for one case.
#[derive(Debug, Clone, PartialEq)]
pub struct Submitted {
    /// Which case.
    pub case_id: String,
    /// What the agent produced, if anything.
    pub output: Option<Value>,
    /// Latency, ms.
    pub latency_ms: f64,
    /// Cost, USD.
    pub cost_usd: f64,
}

/// Scoring input error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// Two results for one case.
    DuplicateResult(String),
    /// A result names a case the profile does not have.
    UnknownCase(String),
    /// The profile is malformed.
    BadProfile(&'static str),
    /// A latency or cost is negative or not finite.
    BadMeasurement(String),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::DuplicateResult(c) => write!(f, "duplicate result for case_id: {c}"),
            EvalError::UnknownCase(c) => write!(f, "unknown case_id: {c}"),
            EvalError::BadProfile(s) => write!(f, "bad profile: {s}"),
            EvalError::BadMeasurement(c) => write!(f, "bad latency or cost for case: {c}"),
        }
    }
}

impl std::error::Error for EvalError {}

/// Result for one case.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseResult {
    /// Case id.
    pub case_id: String,
    /// All assertions passed.
    pub passed: bool,
    /// `(assertion type, passed)` per assertion; empty when nothing was submitted.
    pub assertions: Vec<(&'static str, bool)>,
}

/// One gate outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateResult {
    /// Gate satisfied.
    pub passed: bool,
    /// Measured value.
    pub actual: f64,
    /// Threshold.
    pub limit: f64,
}

/// Full evaluation result.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// Weighted score.
    pub score: f64,
    /// Every gate passed.
    pub passed: bool,
    /// Fraction of cases passed.
    pub case_pass_rate: f64,
    /// Mean latency over submitted cases, ms.
    pub avg_latency_ms: f64,
    /// Total cost, USD.
    pub total_cost_usd: f64,
    /// 1 - avg latency / limit, floored at 0.
    pub latency_score: f64,
    /// 1 - cost / limit, floored at 0.
    pub cost_score: f64,
    /// Pass-rate gate.
    pub pass_rate_gate: GateResult,
    /// Latency gate.
    pub latency_gate: GateResult,
    /// Cost gate.
    pub cost_gate: GateResult,
    /// Per case.
    pub case_results: Vec<CaseResult>,
}

fn round(x: f64, places: i32) -> f64 {
    let p = 10f64.powi(places);
    (x * p).round() / p
}

/// Score submitted results against a profile.
pub fn score(profile: &Profile, submitted: &[Submitted]) -> Result<Score, EvalError> {
    if profile.cases.is_empty() || profile.cases.len() > 500 {
        return Err(EvalError::BadProfile("cases must number 1 to 500"));
    }
    if profile.gates.max_avg_latency_ms <= 0.0 || !profile.gates.max_avg_latency_ms.is_finite() {
        return Err(EvalError::BadProfile("max_avg_latency_ms must be positive"));
    }
    let ids: BTreeSet<&str> = profile.cases.iter().map(|c| c.case_id.as_str()).collect();
    if ids.len() != profile.cases.len() {
        return Err(EvalError::BadProfile("case ids must be unique"));
    }

    let mut by_id: BTreeMap<&str, &Submitted> = BTreeMap::new();
    for s in submitted {
        if by_id.insert(s.case_id.as_str(), s).is_some() {
            return Err(EvalError::DuplicateResult(s.case_id.clone()));
        }
    }
    if let Some(unknown) = by_id.keys().find(|k| !ids.contains(**k)) {
        return Err(EvalError::UnknownCase((*unknown).to_string()));
    }

    let mut case_results = Vec::with_capacity(profile.cases.len());
    let (mut latency_sum, mut latency_n, mut total_cost) = (0.0, 0usize, 0.0);
    for case in &profile.cases {
        let Some(sub) = by_id.get(case.case_id.as_str()) else {
            case_results.push(CaseResult {
                case_id: case.case_id.clone(),
                passed: false,
                assertions: vec![],
            });
            continue;
        };
        if !(sub.latency_ms.is_finite()
            && sub.latency_ms >= 0.0
            && sub.cost_usd.is_finite()
            && sub.cost_usd >= 0.0)
        {
            return Err(EvalError::BadMeasurement(case.case_id.clone()));
        }
        latency_sum += sub.latency_ms;
        latency_n += 1;
        total_cost += sub.cost_usd;
        let results: Vec<(&'static str, bool)> = case
            .assertions
            .iter()
            .map(|a| (a.name(), a.check(sub.output.as_ref())))
            .collect();
        let passed = results.iter().all(|(_, ok)| *ok);
        case_results.push(CaseResult {
            case_id: case.case_id.clone(),
            passed,
            assertions: results,
        });
    }

    let pass_rate =
        case_results.iter().filter(|c| c.passed).count() as f64 / profile.cases.len() as f64;
    let avg_latency = if latency_n > 0 {
        latency_sum / latency_n as f64
    } else {
        0.0
    };
    let g = profile.gates;
    let w = profile.scoring;
    let latency_score = if latency_n > 0 {
        (1.0 - avg_latency / g.max_avg_latency_ms).max(0.0)
    } else {
        0.0
    };
    let cost_score = if g.max_total_cost_usd > 0.0 {
        (1.0 - total_cost / g.max_total_cost_usd).max(0.0)
    } else if total_cost == 0.0 {
        1.0
    } else {
        0.0
    };
    let total = pass_rate * w.correctness_weight
        + latency_score * w.latency_weight
        + cost_score * w.cost_weight;

    let pass_rate_gate = GateResult {
        passed: pass_rate >= g.min_case_pass_rate,
        actual: pass_rate,
        limit: g.min_case_pass_rate,
    };
    let latency_gate = GateResult {
        passed: avg_latency <= g.max_avg_latency_ms,
        actual: avg_latency,
        limit: g.max_avg_latency_ms,
    };
    let cost_gate = GateResult {
        passed: total_cost <= g.max_total_cost_usd,
        actual: total_cost,
        limit: g.max_total_cost_usd,
    };

    Ok(Score {
        score: round(total, 6),
        passed: pass_rate_gate.passed && latency_gate.passed && cost_gate.passed,
        case_pass_rate: round(pass_rate, 6),
        avg_latency_ms: round(avg_latency, 3),
        total_cost_usd: round(total_cost, 8),
        latency_score: round(latency_score, 6),
        cost_score: round(cost_score, 6),
        pass_rate_gate,
        latency_gate,
        cost_gate,
        case_results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    fn profile() -> Profile {
        Profile {
            cases: vec![
                Case {
                    case_id: "c1".into(),
                    assertions: vec![Assertion::ExactMatch(s("42"))],
                },
                Case {
                    case_id: "c2".into(),
                    assertions: vec![Assertion::Contains("refund".into())],
                },
                Case {
                    case_id: "c3".into(),
                    assertions: vec![Assertion::RequiredFields(vec!["a".into(), "b".into()])],
                },
            ],
            scoring: Scoring {
                correctness_weight: 0.6,
                latency_weight: 0.2,
                cost_weight: 0.2,
            },
            gates: Gates {
                min_case_pass_rate: 0.6,
                max_avg_latency_ms: 1000.0,
                max_total_cost_usd: 1.0,
            },
        }
    }

    fn sub(id: &str, out: Option<Value>, ms: f64, cost: f64) -> Submitted {
        Submitted {
            case_id: id.into(),
            output: out,
            latency_ms: ms,
            cost_usd: cost,
        }
    }

    fn obj(keys: &[&str]) -> Value {
        Value::Map(
            keys.iter()
                .map(|k| ((*k).to_string(), Value::Null))
                .collect(),
        )
    }

    #[test]
    fn perfect_run_scores_and_passes() {
        let r = score(
            &profile(),
            &[
                sub("c1", Some(s("42")), 100.0, 0.1),
                sub(
                    "c2",
                    Some(Value::List(vec![s("x"), s("we will refund you")])),
                    100.0,
                    0.1,
                ),
                sub("c3", Some(obj(&["a", "b", "c"])), 100.0, 0.1),
            ],
        )
        .unwrap();
        assert_eq!(r.case_pass_rate, 1.0);
        assert_eq!(r.avg_latency_ms, 100.0);
        assert_eq!(r.total_cost_usd, 0.3);
        // 1.0*0.6 + (1-0.1)*0.2 + (1-0.3)*0.2 = 0.6 + 0.18 + 0.14
        assert_eq!(r.score, 0.92);
        assert!(r.passed);
    }

    #[test]
    fn missing_case_counts_as_failed_and_excluded_from_latency() {
        let r = score(&profile(), &[sub("c1", Some(s("42")), 200.0, 0.0)]).unwrap();
        assert_eq!(r.case_pass_rate, round(1.0 / 3.0, 6));
        assert_eq!(r.avg_latency_ms, 200.0);
        assert!(!r.passed); // pass rate 0.33 < 0.6
        assert!(r.case_results[1].assertions.is_empty());
    }

    #[test]
    fn nothing_submitted_gives_zero_latency_score() {
        let r = score(&profile(), &[]).unwrap();
        assert_eq!(r.latency_score, 0.0);
        assert_eq!(r.avg_latency_ms, 0.0);
        assert_eq!(r.case_pass_rate, 0.0);
    }

    #[test]
    fn gates_fail_on_latency_and_cost() {
        let slow = score(
            &profile(),
            &[
                sub("c1", Some(s("42")), 2000.0, 0.0),
                sub("c2", Some(s("refund")), 2000.0, 0.0),
                sub("c3", Some(obj(&["a", "b"])), 2000.0, 0.0),
            ],
        )
        .unwrap();
        assert!(!slow.latency_gate.passed);
        assert!(!slow.passed);
        assert_eq!(slow.latency_score, 0.0);
        let dear = score(
            &profile(),
            &[
                sub("c1", Some(s("42")), 1.0, 5.0),
                sub("c2", Some(s("refund")), 1.0, 0.0),
                sub("c3", Some(obj(&["a", "b"])), 1.0, 0.0),
            ],
        )
        .unwrap();
        assert!(!dear.cost_gate.passed);
        assert_eq!(dear.cost_score, 0.0);
    }

    #[test]
    fn zero_cost_budget() {
        let mut p = profile();
        p.gates.max_total_cost_usd = 0.0;
        let free = score(&p, &[sub("c1", Some(s("42")), 1.0, 0.0)]).unwrap();
        assert_eq!(free.cost_score, 1.0);
        let paid = score(&p, &[sub("c1", Some(s("42")), 1.0, 0.01)]).unwrap();
        assert_eq!(paid.cost_score, 0.0);
        assert!(!paid.cost_gate.passed);
    }

    #[test]
    fn assertion_semantics() {
        assert!(Assertion::ExactMatch(Value::Num(1.0)).check(Some(&Value::Num(1.0))));
        assert!(!Assertion::ExactMatch(s("a")).check(Some(&s("b"))));
        assert!(Assertion::ExactMatch(Value::Null).check(None));
        assert!(!Assertion::Contains("x".into()).check(None));
        assert!(!Assertion::Contains("x".into()).check(Some(&Value::Num(1.0))));
        let nested = Value::Map(BTreeMap::from([(
            "k".to_string(),
            Value::List(vec![s("deep needle here")]),
        )]));
        assert!(Assertion::Contains("needle".into()).check(Some(&nested)));
        assert!(!Assertion::RequiredFields(vec!["a".into()]).check(Some(&s("a"))));
        assert!(!Assertion::RequiredFields(vec!["a".into()]).check(None));
    }

    #[test]
    fn input_errors() {
        let p = profile();
        assert_eq!(
            score(&p, &[sub("c1", None, 1.0, 0.0), sub("c1", None, 1.0, 0.0)]),
            Err(EvalError::DuplicateResult("c1".into()))
        );
        assert_eq!(
            score(&p, &[sub("zz", None, 1.0, 0.0)]),
            Err(EvalError::UnknownCase("zz".into()))
        );
        assert!(matches!(
            score(&p, &[sub("c1", None, -1.0, 0.0)]),
            Err(EvalError::BadMeasurement(_))
        ));
        assert!(matches!(
            score(&p, &[sub("c1", None, f64::NAN, 0.0)]),
            Err(EvalError::BadMeasurement(_))
        ));
        let mut bad = profile();
        bad.cases.clear();
        assert!(matches!(score(&bad, &[]), Err(EvalError::BadProfile(_))));
        let mut bad = profile();
        bad.gates.max_avg_latency_ms = 0.0;
        assert!(matches!(score(&bad, &[]), Err(EvalError::BadProfile(_))));
        let mut bad = profile();
        bad.cases.push(bad.cases[0].clone());
        assert!(matches!(score(&bad, &[]), Err(EvalError::BadProfile(_))));
    }
}
