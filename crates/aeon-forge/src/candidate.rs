//! Bounded, deterministic candidate generation.
//!
//! Only registered behavioural operators exist. Identity, permissions and tools
//! are never mutated, and the result is always an unverified candidate that is
//! not eligible for promotion until it has been evaluated.

use crate::manifest::{is_semver, Manifest, Origin};
use crate::registry::{Registry, RegistryError};
use std::fmt;

/// Most operators one candidate may combine.
pub const MAX_OPERATORS: usize = 5;
/// Operator set version, recorded in provenance.
pub const OPERATOR_VERSION: &str = "1.0.0";

/// A registered mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `max_steps` minus one (floor 1).
    MaxStepsDecrement,
    /// `max_steps` plus one (cap 100).
    MaxStepsIncrement,
    /// `timeout_seconds` minus five (floor 1).
    TimeoutDecrement,
    /// `timeout_seconds` plus five (cap 3600).
    TimeoutIncrement,
    /// `temperature` minus 0.1 (floor 0).
    TemperatureLower,
    /// `temperature` plus 0.1 (cap 2).
    TemperatureHigher,
    /// `top_p` minus 0.05 (floor 0).
    TopPLower,
    /// `top_p` plus 0.05 (cap 1).
    TopPHigher,
}

impl Operator {
    /// Every operator.
    pub const ALL: [Operator; 8] = [
        Operator::MaxStepsDecrement,
        Operator::MaxStepsIncrement,
        Operator::TimeoutDecrement,
        Operator::TimeoutIncrement,
        Operator::TemperatureLower,
        Operator::TemperatureHigher,
        Operator::TopPLower,
        Operator::TopPHigher,
    ];

    /// Stable id used in provenance and requests.
    pub fn id(self) -> &'static str {
        match self {
            Operator::MaxStepsDecrement => "behavior.max_steps.decrement",
            Operator::MaxStepsIncrement => "behavior.max_steps.increment",
            Operator::TimeoutDecrement => "behavior.timeout_seconds.decrement",
            Operator::TimeoutIncrement => "behavior.timeout_seconds.increment",
            Operator::TemperatureLower => "model.parameters.temperature.lower",
            Operator::TemperatureHigher => "model.parameters.temperature.higher",
            Operator::TopPLower => "model.parameters.top_p.lower",
            Operator::TopPHigher => "model.parameters.top_p.higher",
        }
    }

    /// Path of the manifest field this operator changes.
    pub fn path(self) -> &'static str {
        match self {
            Operator::MaxStepsDecrement | Operator::MaxStepsIncrement => "/behavior/max_steps",
            Operator::TimeoutDecrement | Operator::TimeoutIncrement => "/behavior/timeout_seconds",
            Operator::TemperatureLower | Operator::TemperatureHigher => {
                "/model_binding/parameters/temperature"
            }
            Operator::TopPLower | Operator::TopPHigher => "/model_binding/parameters/top_p",
        }
    }

    /// Look an operator up by id.
    pub fn from_id(id: &str) -> Option<Operator> {
        Operator::ALL.into_iter().find(|o| o.id() == id)
    }

    fn apply(self, m: &mut Manifest) {
        fn param(m: &mut Manifest, name: &str, default: f64, delta: f64, lo: f64, hi: f64) {
            let cur = m.model_parameters.get(name).copied().unwrap_or(default);
            let next = (cur + delta).clamp(lo, hi);
            m.model_parameters
                .insert(name.to_string(), (next * 10_000.0).round() / 10_000.0);
        }
        match self {
            Operator::MaxStepsDecrement => {
                m.max_steps = m.max_steps.saturating_sub(1).clamp(1, 100)
            }
            Operator::MaxStepsIncrement => m.max_steps = (m.max_steps + 1).clamp(1, 100),
            Operator::TimeoutDecrement => {
                m.timeout_seconds = m.timeout_seconds.saturating_sub(5).clamp(1, 3600)
            }
            Operator::TimeoutIncrement => {
                m.timeout_seconds = (m.timeout_seconds + 5).clamp(1, 3600)
            }
            Operator::TemperatureLower => param(m, "temperature", 0.7, -0.1, 0.0, 2.0),
            Operator::TemperatureHigher => param(m, "temperature", 0.7, 0.1, 0.0, 2.0),
            Operator::TopPLower => param(m, "top_p", 1.0, -0.05, 0.0, 1.0),
            Operator::TopPHigher => param(m, "top_p", 1.0, 0.05, 0.0, 1.0),
        }
    }
}

/// Candidate generation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateError {
    /// The request breaks the contract (version, operators, actor, no-op).
    Invalid(String),
    /// The candidate version already exists.
    Conflict(String),
    /// The parent version is missing.
    ParentNotFound(String),
}

impl fmt::Display for CandidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandidateError::Invalid(s) => write!(f, "invalid candidate request: {s}"),
            CandidateError::Conflict(s) => write!(
                f,
                "{s} already exists; immutable versions are never overwritten"
            ),
            CandidateError::ParentNotFound(s) => write!(f, "parent {s} was not found"),
        }
    }
}

impl std::error::Error for CandidateError {}

/// Result of a generation.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// `agent@version` of the parent.
    pub parent_artifact_id: String,
    /// Hash of the parent content.
    pub parent_content_hash: String,
    /// Hash of the new candidate.
    pub candidate_content_hash: String,
    /// Seed used for operator ordering.
    pub seed: u32,
    /// Operators in the order they were applied.
    pub operator_ids: Vec<&'static str>,
    /// Field paths whose value actually changed.
    pub changed_paths: Vec<&'static str>,
    /// Always `"unverified_candidate"`.
    pub trust_status: &'static str,
    /// Always `false`: promotion needs evaluation.
    pub promotion_eligible: bool,
}

/// SplitMix64: a tiny deterministic generator, enough for ordering.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn shuffle<T>(items: &mut [T], seed: u32) {
    let mut rng = SplitMix(u64::from(seed));
    for i in (1..items.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

/// Derive a new immutable version from `parent_version` and store it.
///
/// `created_at` is supplied by the caller. The same inputs always produce the
/// same candidate (operator order depends only on `seed`).
#[allow(clippy::too_many_arguments)]
pub fn generate(
    registry: &mut Registry,
    tenant_id: &str,
    agent_id: &str,
    parent_version: &str,
    candidate_version: &str,
    operator_ids: &[&str],
    seed: u32,
    actor_id: &str,
    created_at: &str,
) -> Result<Candidate, CandidateError> {
    let bad = |s: &str| CandidateError::Invalid(s.to_string());
    if !is_semver(candidate_version) {
        return Err(bad("candidate_version must be valid semantic version text"));
    }
    if operator_ids.is_empty() || operator_ids.len() > MAX_OPERATORS {
        return Err(bad("operator_ids must contain 1 to 5 operators"));
    }
    let mut seen = std::collections::BTreeSet::new();
    if !operator_ids.iter().all(|id| seen.insert(*id)) {
        return Err(bad("operator_ids must be unique"));
    }
    let mut ops = Vec::with_capacity(operator_ids.len());
    let mut unknown = Vec::new();
    for id in operator_ids {
        match Operator::from_id(id) {
            Some(o) => ops.push(o),
            None => unknown.push(*id),
        }
    }
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(CandidateError::Invalid(format!(
            "unsupported mutation operators: {unknown:?}"
        )));
    }
    if actor_id.trim().is_empty() || actor_id.len() > 256 {
        return Err(bad("actor_id must contain 1 to 256 characters"));
    }

    let parent = registry
        .get(tenant_id, agent_id, parent_version)
        .map_err(|_| CandidateError::ParentNotFound(format!("{agent_id}@{parent_version}")))?
        .clone();
    if registry.get(tenant_id, agent_id, candidate_version).is_ok() {
        return Err(CandidateError::Conflict(format!(
            "{agent_id}@{candidate_version}"
        )));
    }

    let mut cand = parent.manifest.clone();
    cand.version = candidate_version.to_string();
    shuffle(&mut ops, seed);
    for op in &ops {
        op.apply(&mut cand);
    }
    if cand.behaviour_eq(&parent.manifest) {
        return Err(bad("the requested operators produced no candidate change"));
    }

    let parent_ref = format!("{agent_id}@{parent_version}");
    cand.origin = Origin::Derived;
    cand.created_at = created_at.to_string();
    cand.created_by = actor_id.to_string();
    if !cand.source_refs.contains(&parent_ref) {
        cand.source_refs.push(parent_ref.clone());
    }
    for op in &ops {
        cand.source_refs.push(format!(
            "aeon-forge:mutation/{}@{OPERATOR_VERSION}?seed={seed}",
            op.id()
        ));
    }

    // Defence in depth: protected fields must match the immutable parent.
    if cand.agent_id != parent.manifest.agent_id
        || cand.permissions != parent.manifest.permissions
        || cand.tools != parent.manifest.tools
    {
        return Err(bad("candidate mutation changed a protected field"));
    }

    let (record, _) = registry
        .create(tenant_id, cand, Some(parent_ref.clone()))
        .map_err(|e| match e {
            RegistryError::Conflict(s) => CandidateError::Conflict(s),
            other => CandidateError::Invalid(other.to_string()),
        })?;

    let mut changed: Vec<&'static str> = Vec::new();
    for op in &ops {
        let differs = match op.path() {
            "/behavior/max_steps" => parent.manifest.max_steps != record.manifest.max_steps,
            "/behavior/timeout_seconds" => {
                parent.manifest.timeout_seconds != record.manifest.timeout_seconds
            }
            "/model_binding/parameters/temperature" => {
                parent.manifest.model_parameters.get("temperature")
                    != record.manifest.model_parameters.get("temperature")
            }
            _ => {
                parent.manifest.model_parameters.get("top_p")
                    != record.manifest.model_parameters.get("top_p")
            }
        };
        if differs && !changed.contains(&op.path()) {
            changed.push(op.path());
        }
    }
    changed.sort_unstable();

    Ok(Candidate {
        parent_artifact_id: parent_ref,
        parent_content_hash: parent.content_hash,
        candidate_content_hash: record.content_hash,
        seed,
        operator_ids: ops.iter().map(|o| o.id()).collect(),
        changed_paths: changed,
        trust_status: "unverified_candidate",
        promotion_eligible: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn base() -> Manifest {
        Manifest {
            agent_id: "a".into(),
            version: "1.0.0".into(),
            max_steps: 10,
            timeout_seconds: 60,
            model_parameters: BTreeMap::from([("temperature".to_string(), 0.7)]),
            permissions: BTreeSet::from(["p".to_string()]),
            tools: BTreeSet::from(["t".to_string()]),
            origin: Origin::Authored,
            created_at: "2026-01-01T00:00:00Z".into(),
            created_by: "me".into(),
            source_refs: vec![],
        }
    }

    fn registry() -> Registry {
        let mut r = Registry::new();
        r.create("t", base(), None).unwrap();
        r
    }

    fn gen(
        r: &mut Registry,
        version: &str,
        ops: &[&str],
        seed: u32,
    ) -> Result<Candidate, CandidateError> {
        generate(
            r,
            "t",
            "a",
            "1.0.0",
            version,
            ops,
            seed,
            "actor",
            "2026-02-01T00:00:00Z",
        )
    }

    #[test]
    fn derives_a_new_version_and_protects_fields() {
        let mut r = registry();
        let c = gen(
            &mut r,
            "1.0.1",
            &[
                "behavior.max_steps.increment",
                "model.parameters.temperature.lower",
            ],
            7,
        )
        .unwrap();
        let rec = r.get("t", "a", "1.0.1").unwrap();
        assert_eq!(rec.manifest.max_steps, 11);
        assert_eq!(rec.manifest.model_parameters["temperature"], 0.6);
        assert_eq!(rec.manifest.permissions, base().permissions);
        assert_eq!(rec.manifest.tools, base().tools);
        assert_eq!(rec.manifest.origin, Origin::Derived);
        assert_eq!(rec.parent_artifact_id.as_deref(), Some("a@1.0.0"));
        assert_eq!(c.trust_status, "unverified_candidate");
        assert!(!c.promotion_eligible);
        assert_eq!(
            c.changed_paths,
            [
                "/behavior/max_steps",
                "/model_binding/parameters/temperature"
            ]
        );
        assert!(rec
            .manifest
            .source_refs
            .iter()
            .any(|s| s.contains("max_steps.increment@1.0.0?seed=7")));
        assert_ne!(c.parent_content_hash, c.candidate_content_hash);
    }

    #[test]
    fn same_inputs_give_the_same_candidate() {
        let ops = [
            "behavior.max_steps.increment",
            "behavior.timeout_seconds.decrement",
            "model.parameters.top_p.lower",
        ];
        let mut r1 = registry();
        let mut r2 = registry();
        let a = gen(&mut r1, "2.0.0", &ops, 42).unwrap();
        let b = gen(&mut r2, "2.0.0", &ops, 42).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn seed_controls_operator_order() {
        let ops = [
            "behavior.max_steps.increment",
            "behavior.timeout_seconds.increment",
            "model.parameters.top_p.lower",
            "model.parameters.temperature.higher",
        ];
        let mut orders = std::collections::BTreeSet::new();
        for seed in 0..40 {
            let mut r = registry();
            let v = format!("3.0.{seed}");
            orders.insert(gen(&mut r, &v, &ops, seed).unwrap().operator_ids);
        }
        assert!(orders.len() > 1, "shuffle never changed the order");
    }

    #[test]
    fn rejects_bad_requests() {
        let mut r = registry();
        let inc = "behavior.max_steps.increment";
        assert!(matches!(
            gen(&mut r, "bad", &[inc], 1),
            Err(CandidateError::Invalid(_))
        ));
        assert!(matches!(
            gen(&mut r, "1.0.1", &[], 1),
            Err(CandidateError::Invalid(_))
        ));
        assert!(matches!(
            gen(&mut r, "1.0.1", &[inc, inc], 1),
            Err(CandidateError::Invalid(_))
        ));
        assert!(matches!(
            gen(&mut r, "1.0.1", &["nope"], 1),
            Err(CandidateError::Invalid(_))
        ));
        let six: Vec<&str> = Operator::ALL.iter().take(6).map(|o| o.id()).collect();
        assert!(matches!(
            gen(&mut r, "1.0.1", &six, 1),
            Err(CandidateError::Invalid(_))
        ));
        assert!(matches!(
            generate(&mut r, "t", "a", "1.0.0", "1.0.1", &[inc], 1, "  ", "x"),
            Err(CandidateError::Invalid(_))
        ));
        assert!(matches!(
            generate(&mut r, "t", "a", "9.9.9", "1.0.1", &[inc], 1, "me", "x"),
            Err(CandidateError::ParentNotFound(_))
        ));
        assert!(matches!(
            gen(&mut r, "1.0.0", &[inc], 1),
            Err(CandidateError::Conflict(_))
        ));
    }

    #[test]
    fn no_op_candidates_are_rejected() {
        let mut r = Registry::new();
        let mut m = base();
        m.max_steps = 100;
        r.create("t", m, None).unwrap();
        let res = generate(
            &mut r,
            "t",
            "a",
            "1.0.0",
            "1.0.1",
            &["behavior.max_steps.increment"],
            1,
            "me",
            "x",
        );
        assert!(
            matches!(res, Err(CandidateError::Invalid(ref s)) if s.contains("no candidate change"))
        );
    }

    #[test]
    fn bounds_are_respected() {
        let mut r = Registry::new();
        let mut m = base();
        m.max_steps = 1;
        m.model_parameters.insert("top_p".into(), 0.02);
        r.create("t", m, None).unwrap();
        generate(
            &mut r,
            "t",
            "a",
            "1.0.0",
            "1.0.1",
            &[
                "behavior.max_steps.decrement",
                "model.parameters.top_p.lower",
            ],
            1,
            "me",
            "x",
        )
        .unwrap();
        let rec = r.get("t", "a", "1.0.1").unwrap();
        assert_eq!(rec.manifest.max_steps, 1);
        assert_eq!(rec.manifest.model_parameters["top_p"], 0.0);
    }

    #[test]
    fn lineage_records_the_parent() {
        let mut r = registry();
        gen(&mut r, "1.1.0", &["behavior.max_steps.increment"], 1).unwrap();
        assert_eq!(r.lineage("t", "a", "1.1.0"), ["a@1.1.0", "a@1.0.0"]);
    }
}
