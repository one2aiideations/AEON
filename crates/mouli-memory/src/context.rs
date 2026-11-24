//! Evidence-aware context store and trusted snapshots.
//!
//! What an agent is told about the world should be traceable and should say how much to trust
//! it. This store keeps *entities* and *relationships* as immutable, numbered versions, each
//! tagged with an assertion class (observed fact, human validated, inferred, AI hypothesis),
//! a confidence and an optional validity window. *Evidence* ties a claim to a source and must
//! be reviewed by someone other than its submitter before it counts. An append-only event log
//! records every change.
//!
//! [`ContextStore::snapshot`] assembles a bounded, deterministic packet for an agent from the
//! trusted part of the store: only entities that are currently valid and whose assertion class
//! passes the policy (inferences only on request, AI hypotheses never), relationships whose both
//! ends are in the packet at exactly the packet's versions, and **validated** evidence only.
//! Raw attributes never enter a snapshot.
//!
//! One store belongs to one tenant; separate tenants use separate stores.
//! Times are unix seconds supplied by the caller, so everything is deterministic.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// Most entities in one snapshot.
pub const MAX_ENTITIES: usize = 12;
/// Most relationships in one snapshot.
pub const MAX_RELATIONSHIPS: usize = 24;
/// Most evidence items in one snapshot.
pub const MAX_EVIDENCE: usize = 36;
/// Largest canonical snapshot, in bytes.
pub const MAX_SNAPSHOT_BYTES: usize = 48 * 1024;

/// How a claim came to be believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Assertion {
    /// Directly observed.
    ObservedFact,
    /// Confirmed by a person.
    HumanValidated,
    /// Derived by reasoning.
    Inferred,
    /// Proposed by a model, unconfirmed.
    AiHypothesis,
}

impl Assertion {
    fn as_str(self) -> &'static str {
        match self {
            Assertion::ObservedFact => "observed_fact",
            Assertion::HumanValidated => "human_validated",
            Assertion::Inferred => "inferred",
            Assertion::AiHypothesis => "ai_hypothesis",
        }
    }
}

/// Where a piece of evidence comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Database metadata.
    DatabaseMetadata,
    /// API metadata.
    ApiMetadata,
    /// Repository metadata.
    RepositoryMetadata,
    /// Configuration metadata.
    ConfigurationMetadata,
    /// An event log.
    EventLog,
    /// A person vouches for it.
    HumanAttestation,
    /// An agent's analysis.
    AgentAnalysis,
    /// Anything else.
    Other,
}

/// Review state of evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Review {
    /// Not yet reviewed (does not count).
    Unverified,
    /// Accepted by a reviewer.
    Validated,
    /// Refused by a reviewer.
    Rejected,
}

/// Failure of a store operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    /// A field is empty, too long or out of range (the text says which).
    Invalid(&'static str),
    /// An object with this id already exists.
    Exists(String),
    /// The named object or version does not exist.
    NotFound(String),
    /// The reviewer submitted the evidence themselves.
    SelfReview,
    /// Validation needs a recorded source hash and a matching confirmation.
    HashMismatch,
    /// The evidence was already reviewed with a different outcome.
    AlreadyReviewed,
    /// Nothing in the store passes the snapshot policy.
    NothingTrusted,
    /// The snapshot would exceed [`MAX_SNAPSHOT_BYTES`].
    TooLarge,
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContextError::Invalid(w) => write!(f, "invalid {w}"),
            ContextError::Exists(i) => write!(f, "{i} already exists"),
            ContextError::NotFound(i) => write!(f, "{i} was not found"),
            ContextError::SelfReview => {
                f.write_str("evidence submitters cannot review their own evidence")
            }
            ContextError::HashMismatch => f.write_str("source hash missing or not confirmed"),
            ContextError::AlreadyReviewed => {
                f.write_str("evidence was already reviewed with a different decision")
            }
            ContextError::NothingTrusted => {
                f.write_str("no currently valid entity meets the assertion policy")
            }
            ContextError::TooLarge => f.write_str("assembled context exceeds the size limit"),
        }
    }
}

impl std::error::Error for ContextError {}

/// What a caller supplies for an entity version.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySpec {
    /// Stable id (1 to 128 characters).
    pub id: String,
    /// Kind of entity ("table", "service", ...).
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Longer text.
    pub description: String,
    /// Raw attributes (never copied into snapshots).
    pub attributes: BTreeMap<String, String>,
    /// How it is believed.
    pub assertion: Assertion,
    /// Confidence in `[0, 1]`.
    pub confidence: f64,
    /// Valid from this time on (inclusive), if limited.
    pub valid_from: Option<i64>,
    /// Valid until this time (inclusive), if limited.
    pub valid_to: Option<i64>,
}

/// A stored entity version.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    /// The content, as supplied.
    pub spec: EntitySpec,
    /// Version number, starting at 1.
    pub version: u32,
    /// SHA-256 of the content.
    pub content_hash: String,
}

/// What a caller supplies for a relationship.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationshipSpec {
    /// Stable id.
    pub id: String,
    /// Relationship type ("depends-on", ...).
    pub kind: String,
    /// Source entity and the version this claim is about.
    pub source: (String, u32),
    /// Target entity and the version this claim is about.
    pub target: (String, u32),
    /// How it is believed.
    pub assertion: Assertion,
    /// Confidence in `[0, 1]`.
    pub confidence: f64,
    /// Valid from, inclusive.
    pub valid_from: Option<i64>,
    /// Valid until, inclusive.
    pub valid_to: Option<i64>,
}

/// A stored relationship version.
#[derive(Debug, Clone, PartialEq)]
pub struct Relationship {
    /// The content.
    pub spec: RelationshipSpec,
    /// Version number, starting at 1.
    pub version: u32,
    /// SHA-256 of the content.
    pub content_hash: String,
}

/// Evidence that supports an entity version (and optionally one relationship).
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// Unique id.
    pub id: String,
    /// Entity the evidence is about.
    pub entity: String,
    /// Entity version the evidence is about.
    pub entity_version: u32,
    /// Relationship it also supports, if any.
    pub relationship: Option<String>,
    /// Kind of source.
    pub source_kind: SourceKind,
    /// Reference to the source (path, URL, ticket).
    pub source_ref: String,
    /// SHA-256 of the source content, lowercase hex, if recorded.
    pub source_hash: Option<String>,
    /// When it was observed.
    pub observed_at: i64,
    /// Confidence in `[0, 1]`.
    pub confidence: f64,
    /// Who submitted it.
    pub recorded_by: String,
    /// Review state.
    pub status: Review,
    /// Who reviewed it, once reviewed.
    pub reviewed_by: Option<String>,
}

/// One entry of the append-only log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Position in the log, from 0.
    pub seq: usize,
    /// What happened (`entity.created`, `entity.versioned`, `relationship.recorded`,
    /// `evidence.recorded`, `evidence.reviewed`).
    pub kind: &'static str,
    /// Id of the object concerned.
    pub object_id: String,
    /// Its version, where it has one.
    pub object_version: Option<u32>,
    /// Time supplied by the caller.
    pub at: i64,
}

/// What a snapshot may contain.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Also include `Inferred` claims (`AiHypothesis` is never included).
    pub include_inferences: bool,
    /// Current time, for validity windows.
    pub now: i64,
    /// Most entities to include, `1..=`[`MAX_ENTITIES`].
    pub limit: usize,
}

/// Which entities a snapshot starts from.
#[derive(Debug, Clone, PartialEq)]
pub enum Selector {
    /// Exactly these entities (1 to `limit`, no duplicates).
    Ids(Vec<String>),
    /// Entities whose name contains this text, case-insensitively.
    Search(String),
}

/// An entity as shown in a snapshot (no raw attributes, description cut to 1000 characters).
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotEntity {
    /// Entity id.
    pub id: String,
    /// Version.
    pub version: u32,
    /// Kind.
    pub kind: String,
    /// Name.
    pub name: String,
    /// Description (at most 1000 characters).
    pub description: String,
    /// Assertion class.
    pub assertion: Assertion,
    /// Confidence.
    pub confidence: f64,
    /// Content hash of this version.
    pub content_hash: String,
}

/// A relationship as shown in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRelationship {
    /// Relationship id.
    pub id: String,
    /// Version.
    pub version: u32,
    /// Type.
    pub kind: String,
    /// Source entity id and version.
    pub source: (String, u32),
    /// Target entity id and version.
    pub target: (String, u32),
    /// Assertion class.
    pub assertion: Assertion,
    /// Confidence.
    pub confidence: f64,
}

/// A validated piece of evidence as shown in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotEvidence {
    /// Evidence id.
    pub id: String,
    /// Entity id.
    pub entity: String,
    /// Entity version.
    pub entity_version: u32,
    /// Source reference.
    pub source_ref: String,
    /// Source hash.
    pub source_hash: Option<String>,
    /// Observation time.
    pub observed_at: i64,
    /// Confidence.
    pub confidence: f64,
}

/// The assembled, trusted packet.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// `context-snapshot:<hash>`.
    pub reference: String,
    /// SHA-256 over tenant and content.
    pub content_hash: String,
    /// Size of the canonical text.
    pub byte_length: usize,
    /// Entities, ordered by (kind, id).
    pub entities: Vec<SnapshotEntity>,
    /// Relationships, ordered by id.
    pub relationships: Vec<SnapshotRelationship>,
    /// Validated evidence, ordered by id.
    pub evidence: Vec<SnapshotEvidence>,
}

/// Versioned context for one tenant.
#[derive(Debug, Clone)]
pub struct ContextStore {
    tenant: String,
    entities: BTreeMap<String, Vec<Entity>>,
    relationships: BTreeMap<String, Vec<Relationship>>,
    evidence: BTreeMap<String, Evidence>,
    events: Vec<Event>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// Constant-time equality for equal-length strings (different lengths are unequal at once).
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn text(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max
}

fn valid_window(from: Option<i64>, to: Option<i64>) -> bool {
    match (from, to) {
        (Some(a), Some(b)) => a <= b,
        _ => true,
    }
}

fn currently_valid(from: Option<i64>, to: Option<i64>, now: i64) -> bool {
    from.is_none_or(|f| f <= now) && to.is_none_or(|t| t >= now)
}

fn esc(s: &str) -> String {
    format!("{s:?}")
}

fn opt<T: fmt::Display>(v: &Option<T>) -> String {
    v.as_ref().map_or("-".to_string(), |x| x.to_string())
}

impl EntitySpec {
    fn canonical(&self) -> String {
        let attrs: Vec<String> = self
            .attributes
            .iter()
            .map(|(k, v)| format!("{}={}", esc(k), esc(v)))
            .collect();
        format!(
            "entity|{}|{}|{}|{}|{}|{}|{:?}|{}|{}|{}",
            esc(&self.id),
            esc(&self.kind),
            esc(&self.name),
            esc(&self.description),
            attrs.join(","),
            self.assertion.as_str(),
            self.confidence,
            opt(&self.valid_from),
            opt(&self.valid_to),
            ""
        )
    }
}

impl RelationshipSpec {
    fn canonical(&self) -> String {
        format!(
            "relationship|{}|{}|{}@{}|{}@{}|{}|{:?}|{}|{}",
            esc(&self.id),
            esc(&self.kind),
            esc(&self.source.0),
            self.source.1,
            esc(&self.target.0),
            self.target.1,
            self.assertion.as_str(),
            self.confidence,
            opt(&self.valid_from),
            opt(&self.valid_to)
        )
    }
}

impl ContextStore {
    /// An empty store for `tenant`.
    pub fn new(tenant: &str) -> Result<Self, ContextError> {
        if !text(tenant, 128) {
            return Err(ContextError::Invalid("tenant"));
        }
        Ok(ContextStore {
            tenant: tenant.to_string(),
            entities: BTreeMap::new(),
            relationships: BTreeMap::new(),
            evidence: BTreeMap::new(),
            events: Vec::new(),
        })
    }

    fn log(&mut self, kind: &'static str, id: &str, version: Option<u32>, at: i64) {
        let seq = self.events.len();
        self.events.push(Event {
            seq,
            kind,
            object_id: id.to_string(),
            object_version: version,
            at,
        });
    }

    fn check_entity(spec: &EntitySpec) -> Result<(), ContextError> {
        if !text(&spec.id, 128) {
            return Err(ContextError::Invalid("entity id"));
        }
        if !text(&spec.kind, 128) || !text(&spec.name, 256) {
            return Err(ContextError::Invalid("entity kind or name"));
        }
        if spec.description.chars().count() > 4000 {
            return Err(ContextError::Invalid("entity description"));
        }
        if !(0.0..=1.0).contains(&spec.confidence) || spec.confidence.is_nan() {
            return Err(ContextError::Invalid("confidence"));
        }
        if !valid_window(spec.valid_from, spec.valid_to) {
            return Err(ContextError::Invalid("validity window"));
        }
        Ok(())
    }

    /// Record a new entity as version 1.
    pub fn create_entity(&mut self, spec: EntitySpec, at: i64) -> Result<u32, ContextError> {
        Self::check_entity(&spec)?;
        if self.entities.contains_key(&spec.id) {
            return Err(ContextError::Exists(spec.id));
        }
        let id = spec.id.clone();
        let hash = sha(&spec.canonical());
        self.entities.insert(
            id.clone(),
            vec![Entity {
                spec,
                version: 1,
                content_hash: hash,
            }],
        );
        self.log("entity.created", &id, Some(1), at);
        Ok(1)
    }

    /// Record a changed entity as the next version. Identical content creates nothing and
    /// returns the current version, so retries are harmless.
    pub fn version_entity(&mut self, spec: EntitySpec, at: i64) -> Result<u32, ContextError> {
        Self::check_entity(&spec)?;
        let versions = self
            .entities
            .get_mut(&spec.id)
            .ok_or_else(|| ContextError::NotFound(spec.id.clone()))?;
        let hash = sha(&spec.canonical());
        let last = versions.last().map_or(0, |v| v.version);
        if versions.last().is_some_and(|v| v.content_hash == hash) {
            return Ok(last);
        }
        let version = last + 1;
        let id = spec.id.clone();
        versions.push(Entity {
            spec,
            version,
            content_hash: hash,
        });
        self.log("entity.versioned", &id, Some(version), at);
        Ok(version)
    }

    /// A specific entity version.
    pub fn entity(&self, id: &str, version: u32) -> Option<&Entity> {
        self.entities.get(id)?.iter().find(|e| e.version == version)
    }

    /// The newest version of an entity.
    pub fn latest_entity(&self, id: &str) -> Option<&Entity> {
        self.entities.get(id)?.last()
    }

    /// Record a relationship. Both referenced entity versions must exist. An id that already
    /// exists gets a new version (identical content creates nothing).
    pub fn record_relationship(
        &mut self,
        spec: RelationshipSpec,
        at: i64,
    ) -> Result<u32, ContextError> {
        if !text(&spec.id, 128) || !text(&spec.kind, 128) {
            return Err(ContextError::Invalid("relationship id or kind"));
        }
        if !(0.0..=1.0).contains(&spec.confidence) || spec.confidence.is_nan() {
            return Err(ContextError::Invalid("confidence"));
        }
        if !valid_window(spec.valid_from, spec.valid_to) {
            return Err(ContextError::Invalid("validity window"));
        }
        for (id, v) in [&spec.source, &spec.target] {
            if self.entity(id, *v).is_none() {
                return Err(ContextError::NotFound(format!("{id}@{v}")));
            }
        }
        let hash = sha(&spec.canonical());
        let versions = self.relationships.entry(spec.id.clone()).or_default();
        if versions.last().is_some_and(|v| v.content_hash == hash) {
            return Ok(versions.len() as u32);
        }
        let version = versions.len() as u32 + 1;
        let id = spec.id.clone();
        versions.push(Relationship {
            spec,
            version,
            content_hash: hash,
        });
        self.log("relationship.recorded", &id, Some(version), at);
        Ok(version)
    }

    /// Record evidence (starts `Unverified`). The entity version, and the relationship if named,
    /// must exist.
    #[allow(clippy::too_many_arguments)]
    pub fn add_evidence(
        &mut self,
        id: &str,
        entity: &str,
        entity_version: u32,
        relationship: Option<&str>,
        source_kind: SourceKind,
        source_ref: &str,
        source_hash: Option<&str>,
        observed_at: i64,
        confidence: f64,
        recorded_by: &str,
    ) -> Result<(), ContextError> {
        if !text(id, 128) || !text(source_ref, 1024) || !text(recorded_by, 256) {
            return Err(ContextError::Invalid(
                "evidence id, source reference or submitter",
            ));
        }
        if !(0.0..=1.0).contains(&confidence) || confidence.is_nan() {
            return Err(ContextError::Invalid("confidence"));
        }
        if source_hash.is_some_and(|h| !is_sha256_hex(h)) {
            return Err(ContextError::Invalid("source hash"));
        }
        if self.evidence.contains_key(id) {
            return Err(ContextError::Exists(id.to_string()));
        }
        if self.entity(entity, entity_version).is_none() {
            return Err(ContextError::NotFound(format!("{entity}@{entity_version}")));
        }
        if let Some(r) = relationship {
            if !self.relationships.contains_key(r) {
                return Err(ContextError::NotFound(r.to_string()));
            }
        }
        self.evidence.insert(
            id.to_string(),
            Evidence {
                id: id.to_string(),
                entity: entity.to_string(),
                entity_version,
                relationship: relationship.map(str::to_string),
                source_kind,
                source_ref: source_ref.to_string(),
                source_hash: source_hash.map(str::to_string),
                observed_at,
                confidence,
                recorded_by: recorded_by.to_string(),
                status: Review::Unverified,
                reviewed_by: None,
            },
        );
        self.log("evidence.recorded", id, None, observed_at);
        Ok(())
    }

    /// Review evidence. Rules: the reviewer must not be the submitter; the reason needs 10 to
    /// 2000 characters; *validating* needs a recorded source hash and the reviewer's confirmed
    /// hash must match it. Repeating an identical review is a no-op; a different decision after
    /// a review is refused.
    pub fn review_evidence(
        &mut self,
        id: &str,
        reviewer: &str,
        decision: Review,
        reason: &str,
        confirmed_hash: Option<&str>,
        at: i64,
    ) -> Result<(), ContextError> {
        if decision == Review::Unverified {
            return Err(ContextError::Invalid("decision"));
        }
        if !text(reviewer, 256) {
            return Err(ContextError::Invalid("reviewer"));
        }
        let reason_len = reason.trim().chars().count();
        if !(10..=2000).contains(&reason_len) {
            return Err(ContextError::Invalid("reason"));
        }
        if confirmed_hash.is_some_and(|h| !is_sha256_hex(h)) {
            return Err(ContextError::Invalid("confirmed hash"));
        }
        let ev = self
            .evidence
            .get_mut(id)
            .ok_or_else(|| ContextError::NotFound(id.to_string()))?;
        if ev.recorded_by == reviewer {
            return Err(ContextError::SelfReview);
        }
        if decision == Review::Validated {
            match (&ev.source_hash, confirmed_hash) {
                (Some(recorded), Some(confirmed)) if ct_eq(recorded, confirmed) => {}
                _ => return Err(ContextError::HashMismatch),
            }
        }
        if ev.status != Review::Unverified {
            return if ev.status == decision && ev.reviewed_by.as_deref() == Some(reviewer) {
                Ok(())
            } else {
                Err(ContextError::AlreadyReviewed)
            };
        }
        ev.status = decision;
        ev.reviewed_by = Some(reviewer.to_string());
        self.log("evidence.reviewed", id, None, at);
        Ok(())
    }

    /// One evidence record.
    pub fn evidence(&self, id: &str) -> Option<&Evidence> {
        self.evidence.get(id)
    }

    /// The append-only log, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Latest versions of entities, optionally filtered, ordered by (kind, name, id).
    pub fn query_entities(
        &self,
        kind: Option<&str>,
        assertion: Option<Assertion>,
        search: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<&Entity>, ContextError> {
        if !(1..=100).contains(&limit) {
            return Err(ContextError::Invalid("limit"));
        }
        if search.is_some_and(|s| !text(s, 128)) {
            return Err(ContextError::Invalid("search"));
        }
        let needle = search.map(str::to_lowercase);
        let mut out: Vec<&Entity> = self
            .entities
            .values()
            .filter_map(|v| v.last())
            .filter(|e| kind.is_none_or(|k| e.spec.kind == k))
            .filter(|e| assertion.is_none_or(|a| e.spec.assertion == a))
            .filter(|e| {
                needle
                    .as_ref()
                    .is_none_or(|n| e.spec.name.to_lowercase().contains(n.as_str()))
            })
            .collect();
        out.sort_by(|a, b| {
            (&a.spec.kind, &a.spec.name, &a.spec.id).cmp(&(&b.spec.kind, &b.spec.name, &b.spec.id))
        });
        Ok(out.into_iter().skip(offset).take(limit).collect())
    }

    /// Assemble a trusted snapshot. See the module docs for what it may contain.
    pub fn snapshot(&self, selector: &Selector, policy: &Policy) -> Result<Snapshot, ContextError> {
        if !(1..=MAX_ENTITIES).contains(&policy.limit) {
            return Err(ContextError::Invalid("limit"));
        }
        let candidates: Vec<&Entity> = match selector {
            Selector::Ids(ids) => {
                if ids.is_empty() || ids.len() > policy.limit {
                    return Err(ContextError::Invalid("entity ids"));
                }
                let mut seen = std::collections::BTreeSet::new();
                if ids
                    .iter()
                    .any(|i| !text(i, 128) || !seen.insert(i.as_str()))
                {
                    return Err(ContextError::Invalid("entity ids"));
                }
                ids.iter()
                    .map(|i| {
                        self.latest_entity(i)
                            .ok_or_else(|| ContextError::NotFound(i.clone()))
                    })
                    .collect::<Result<_, _>>()?
            }
            Selector::Search(s) => self.query_entities(None, None, Some(s), policy.limit, 0)?,
        };
        let allowed = |a: Assertion| match a {
            Assertion::ObservedFact | Assertion::HumanValidated => true,
            Assertion::Inferred => policy.include_inferences,
            Assertion::AiHypothesis => false,
        };
        let selected: Vec<&Entity> = candidates
            .into_iter()
            .filter(|e| {
                allowed(e.spec.assertion)
                    && currently_valid(e.spec.valid_from, e.spec.valid_to, policy.now)
            })
            .take(policy.limit)
            .collect();
        if selected.is_empty() {
            return Err(ContextError::NothingTrusted);
        }
        let versions: BTreeMap<&str, u32> = selected
            .iter()
            .map(|e| (e.spec.id.as_str(), e.version))
            .collect();

        let mut entities: Vec<SnapshotEntity> = selected
            .iter()
            .map(|e| SnapshotEntity {
                id: e.spec.id.clone(),
                version: e.version,
                kind: e.spec.kind.clone(),
                name: e.spec.name.clone(),
                description: e.spec.description.chars().take(1000).collect(),
                assertion: e.spec.assertion,
                confidence: e.spec.confidence,
                content_hash: e.content_hash.clone(),
            })
            .collect();
        entities.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));

        let mut relationships: Vec<SnapshotRelationship> = Vec::new();
        for versions_of in self.relationships.values() {
            // Highest version of each relationship that passes every rule.
            let best = versions_of.iter().rev().find(|r| {
                let s = &r.spec;
                versions.get(s.source.0.as_str()) == Some(&s.source.1)
                    && versions.get(s.target.0.as_str()) == Some(&s.target.1)
                    && allowed(s.assertion)
                    && currently_valid(s.valid_from, s.valid_to, policy.now)
            });
            if let Some(r) = best {
                relationships.push(SnapshotRelationship {
                    id: r.spec.id.clone(),
                    version: r.version,
                    kind: r.spec.kind.clone(),
                    source: r.spec.source.clone(),
                    target: r.spec.target.clone(),
                    assertion: r.spec.assertion,
                    confidence: r.spec.confidence,
                });
            }
        }
        relationships.sort_by(|a, b| a.id.cmp(&b.id));
        relationships.truncate(MAX_RELATIONSHIPS);

        let mut evidence: Vec<SnapshotEvidence> = self
            .evidence
            .values()
            .filter(|e| {
                e.status == Review::Validated
                    && versions.get(e.entity.as_str()) == Some(&e.entity_version)
            })
            .map(|e| SnapshotEvidence {
                id: e.id.clone(),
                entity: e.entity.clone(),
                entity_version: e.entity_version,
                source_ref: e.source_ref.clone(),
                source_hash: e.source_hash.clone(),
                observed_at: e.observed_at,
                confidence: e.confidence,
            })
            .collect();
        evidence.sort_by(|a, b| a.id.cmp(&b.id));
        evidence.truncate(MAX_EVIDENCE);

        let mut snap = Snapshot {
            reference: String::new(),
            content_hash: String::new(),
            byte_length: 0,
            entities,
            relationships,
            evidence,
        };
        let canonical = snap.canonical();
        if canonical.len() > MAX_SNAPSHOT_BYTES {
            return Err(ContextError::TooLarge);
        }
        let digest = sha(&format!("{}\n{canonical}", esc(&self.tenant)));
        snap.byte_length = canonical.len();
        snap.reference = format!("context-snapshot:{digest}");
        snap.content_hash = digest;
        Ok(snap)
    }
}

impl Snapshot {
    /// Deterministic text form (fixed field order, sorted lists), used for size and hashing.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        for e in &self.entities {
            out.push_str(&format!(
                "E|{}|{}|{}|{}|{}|{}|{:?}|{}\n",
                esc(&e.id),
                e.version,
                esc(&e.kind),
                esc(&e.name),
                esc(&e.description),
                e.assertion.as_str(),
                e.confidence,
                e.content_hash
            ));
        }
        for r in &self.relationships {
            out.push_str(&format!(
                "R|{}|{}|{}|{}@{}|{}@{}|{}|{:?}\n",
                esc(&r.id),
                r.version,
                esc(&r.kind),
                esc(&r.source.0),
                r.source.1,
                esc(&r.target.0),
                r.target.1,
                r.assertion.as_str(),
                r.confidence
            ));
        }
        for v in &self.evidence {
            out.push_str(&format!(
                "V|{}|{}@{}|{}|{}|{}|{:?}\n",
                esc(&v.id),
                esc(&v.entity),
                v.entity_version,
                esc(&v.source_ref),
                opt(&v.source_hash),
                v.observed_at,
                v.confidence
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn spec(id: &str, name: &str, assertion: Assertion) -> EntitySpec {
        EntitySpec {
            id: id.into(),
            kind: "service".into(),
            name: name.into(),
            description: format!("{name} description"),
            attributes: BTreeMap::from([("secret".to_string(), "raw-value".to_string())]),
            assertion,
            confidence: 0.9,
            valid_from: None,
            valid_to: None,
        }
    }

    fn policy(include_inferences: bool, now: i64) -> Policy {
        Policy {
            include_inferences,
            now,
            limit: 12,
        }
    }

    fn store() -> ContextStore {
        ContextStore::new("tenant-1").unwrap()
    }

    #[test]
    fn entities_version_immutably_and_retries_are_harmless() {
        let mut s = store();
        assert_eq!(
            s.create_entity(spec("a", "Billing", Assertion::ObservedFact), 1),
            Ok(1)
        );
        assert_eq!(
            s.create_entity(spec("a", "Billing", Assertion::ObservedFact), 2),
            Err(ContextError::Exists("a".into()))
        );
        assert_eq!(
            s.version_entity(spec("a", "Billing", Assertion::ObservedFact), 3),
            Ok(1),
            "identical content: no new version"
        );
        assert_eq!(
            s.version_entity(spec("a", "Billing v2", Assertion::ObservedFact), 4),
            Ok(2)
        );
        assert_eq!(s.entity("a", 1).unwrap().spec.name, "Billing");
        assert_eq!(s.latest_entity("a").unwrap().spec.name, "Billing v2");
        assert_ne!(
            s.entity("a", 1).unwrap().content_hash,
            s.entity("a", 2).unwrap().content_hash
        );
        assert_eq!(
            s.version_entity(spec("zz", "x", Assertion::ObservedFact), 5),
            Err(ContextError::NotFound("zz".into()))
        );
        assert_eq!(
            s.events().iter().map(|e| e.kind).collect::<Vec<_>>(),
            ["entity.created", "entity.versioned"]
        );
        assert_eq!(s.events()[1].object_version, Some(2));
    }

    #[test]
    fn validation_of_inputs() {
        assert!(ContextStore::new("  ").is_err());
        let mut s = store();
        let mut bad = spec("a", "x", Assertion::ObservedFact);
        bad.confidence = 1.5;
        assert_eq!(
            s.create_entity(bad.clone(), 0),
            Err(ContextError::Invalid("confidence"))
        );
        bad.confidence = f64::NAN;
        assert!(s.create_entity(bad, 0).is_err());
        let mut w = spec("a", "x", Assertion::ObservedFact);
        w.valid_from = Some(10);
        w.valid_to = Some(5);
        assert_eq!(
            s.create_entity(w, 0),
            Err(ContextError::Invalid("validity window"))
        );
        assert!(s
            .create_entity(spec("", "x", Assertion::ObservedFact), 0)
            .is_err());
        assert!(s
            .create_entity(spec("a", " ", Assertion::ObservedFact), 0)
            .is_err());
        assert!(s.events().is_empty(), "failed operations log nothing");
    }

    #[test]
    fn relationships_pin_entity_versions_and_version_themselves() {
        let mut s = store();
        s.create_entity(spec("a", "A", Assertion::ObservedFact), 0)
            .unwrap();
        s.create_entity(spec("b", "B", Assertion::ObservedFact), 0)
            .unwrap();
        let rel = |v: u32, conf: f64| RelationshipSpec {
            id: "r1".into(),
            kind: "depends-on".into(),
            source: ("a".into(), 1),
            target: ("b".into(), v),
            assertion: Assertion::ObservedFact,
            confidence: conf,
            valid_from: None,
            valid_to: None,
        };
        assert_eq!(s.record_relationship(rel(1, 0.8), 1), Ok(1));
        assert_eq!(
            s.record_relationship(rel(1, 0.8), 2),
            Ok(1),
            "identical: no new version"
        );
        assert_eq!(s.record_relationship(rel(1, 0.6), 3), Ok(2));
        assert_eq!(
            s.record_relationship(rel(7, 0.8), 4),
            Err(ContextError::NotFound("b@7".into()))
        );
        let mut ghost = rel(1, 0.8);
        ghost.source = ("nope".into(), 1);
        assert!(matches!(
            s.record_relationship(ghost, 5),
            Err(ContextError::NotFound(_))
        ));
    }

    fn with_evidence() -> ContextStore {
        let mut s = store();
        s.create_entity(spec("a", "A", Assertion::ObservedFact), 0)
            .unwrap();
        s.add_evidence(
            "ev1",
            "a",
            1,
            None,
            SourceKind::ConfigurationMetadata,
            "repo://cfg.yaml",
            Some(H1),
            10,
            0.9,
            "alice",
        )
        .unwrap();
        s
    }

    #[test]
    fn evidence_rules() {
        let mut s = with_evidence();
        assert_eq!(
            s.add_evidence(
                "ev1",
                "a",
                1,
                None,
                SourceKind::Other,
                "x",
                None,
                0,
                0.5,
                "bob"
            ),
            Err(ContextError::Exists("ev1".into()))
        );
        assert!(matches!(
            s.add_evidence(
                "ev2",
                "a",
                9,
                None,
                SourceKind::Other,
                "x",
                None,
                0,
                0.5,
                "bob"
            ),
            Err(ContextError::NotFound(_))
        ));
        assert!(matches!(
            s.add_evidence(
                "ev2",
                "a",
                1,
                Some("rX"),
                SourceKind::Other,
                "x",
                None,
                0,
                0.5,
                "bob"
            ),
            Err(ContextError::NotFound(_))
        ));
        assert!(s
            .add_evidence(
                "ev2",
                "a",
                1,
                None,
                SourceKind::Other,
                "x",
                Some("NOTHEX"),
                0,
                0.5,
                "bob"
            )
            .is_err());
        assert!(s
            .add_evidence(
                "ev2",
                "a",
                1,
                None,
                SourceKind::Other,
                "x",
                None,
                0,
                2.0,
                "bob"
            )
            .is_err());
        assert_eq!(s.evidence("ev1").unwrap().status, Review::Unverified);
    }

    #[test]
    fn review_requires_a_second_person_and_a_matching_hash() {
        let mut s = with_evidence();
        let reason = "checked against the repository at the recorded commit";
        assert_eq!(
            s.review_evidence("ev1", "alice", Review::Validated, reason, Some(H1), 20),
            Err(ContextError::SelfReview)
        );
        assert_eq!(
            s.review_evidence("ev1", "bob", Review::Validated, "short", Some(H1), 20),
            Err(ContextError::Invalid("reason"))
        );
        assert_eq!(
            s.review_evidence("ev1", "bob", Review::Validated, reason, None, 20),
            Err(ContextError::HashMismatch)
        );
        assert_eq!(
            s.review_evidence("ev1", "bob", Review::Validated, reason, Some(H2), 20),
            Err(ContextError::HashMismatch)
        );
        assert_eq!(
            s.review_evidence("ev1", "bob", Review::Unverified, reason, Some(H1), 20),
            Err(ContextError::Invalid("decision"))
        );
        assert_eq!(
            s.review_evidence("nope", "bob", Review::Validated, reason, Some(H1), 20),
            Err(ContextError::NotFound("nope".into()))
        );
        assert_eq!(
            s.evidence("ev1").unwrap().status,
            Review::Unverified,
            "failed reviews change nothing"
        );
        assert_eq!(
            s.review_evidence("ev1", "bob", Review::Validated, reason, Some(H1), 20),
            Ok(())
        );
        let ev = s.evidence("ev1").unwrap();
        assert_eq!(
            (ev.status, ev.reviewed_by.as_deref()),
            (Review::Validated, Some("bob"))
        );
        // repeating the same review is a no-op; a different decision or reviewer is refused
        assert_eq!(
            s.review_evidence("ev1", "bob", Review::Validated, reason, Some(H1), 21),
            Ok(())
        );
        assert_eq!(
            s.review_evidence("ev1", "bob", Review::Rejected, reason, None, 22),
            Err(ContextError::AlreadyReviewed)
        );
        assert_eq!(
            s.review_evidence("ev1", "carol", Review::Validated, reason, Some(H1), 22),
            Err(ContextError::AlreadyReviewed)
        );
        assert_eq!(
            s.events()
                .iter()
                .filter(|e| e.kind == "evidence.reviewed")
                .count(),
            1
        );
    }

    #[test]
    fn rejection_needs_no_hash_and_evidence_without_a_hash_cannot_validate() {
        let mut s = store();
        s.create_entity(spec("a", "A", Assertion::ObservedFact), 0)
            .unwrap();
        s.add_evidence(
            "nohash",
            "a",
            1,
            None,
            SourceKind::AgentAnalysis,
            "log://x",
            None,
            1,
            0.4,
            "agent-1",
        )
        .unwrap();
        let reason = "cannot confirm where this came from";
        assert_eq!(
            s.review_evidence("nohash", "bob", Review::Validated, reason, Some(H1), 2),
            Err(ContextError::HashMismatch)
        );
        assert_eq!(
            s.review_evidence("nohash", "bob", Review::Rejected, reason, None, 2),
            Ok(())
        );
        assert_eq!(s.evidence("nohash").unwrap().status, Review::Rejected);
    }

    fn graph() -> ContextStore {
        let mut s = store();
        s.create_entity(spec("api", "Payments API", Assertion::ObservedFact), 0)
            .unwrap();
        s.create_entity(spec("db", "Payments DB", Assertion::HumanValidated), 0)
            .unwrap();
        s.create_entity(spec("guess", "Payments cache", Assertion::AiHypothesis), 0)
            .unwrap();
        s.create_entity(spec("infer", "Payments queue", Assertion::Inferred), 0)
            .unwrap();
        s.create_entity(spec("other", "Search", Assertion::ObservedFact), 0)
            .unwrap();
        let rel = |id: &str, a: &str, b: &str, assertion| RelationshipSpec {
            id: id.into(),
            kind: "depends-on".into(),
            source: (a.into(), 1),
            target: (b.into(), 1),
            assertion,
            confidence: 0.8,
            valid_from: None,
            valid_to: None,
        };
        s.record_relationship(rel("api-db", "api", "db", Assertion::ObservedFact), 0)
            .unwrap();
        s.record_relationship(rel("api-guess", "api", "guess", Assertion::ObservedFact), 0)
            .unwrap();
        s.record_relationship(rel("api-infer", "api", "infer", Assertion::Inferred), 0)
            .unwrap();
        s.record_relationship(rel("api-other", "api", "other", Assertion::AiHypothesis), 0)
            .unwrap();
        s
    }

    #[test]
    fn snapshot_policy_filters_by_assertion_class() {
        let s = graph();
        let sel = Selector::Search("payments".into());
        let strict = s.snapshot(&sel, &policy(false, 100)).unwrap();
        let ids: Vec<&str> = strict.entities.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["api", "db"], "no inference, no hypothesis");
        assert_eq!(
            strict
                .relationships
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["api-db"]
        );
        let loose = s.snapshot(&sel, &policy(true, 100)).unwrap();
        let ids: Vec<&str> = loose.entities.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            ["api", "db", "infer"],
            "inference only on request, hypothesis never"
        );
        assert_eq!(
            loose
                .relationships
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["api-db", "api-infer"]
        );
    }

    #[test]
    fn snapshot_excludes_raw_attributes_and_cuts_descriptions() {
        let mut s = store();
        let mut e = spec("a", "A", Assertion::ObservedFact);
        e.description = "x".repeat(3000);
        s.create_entity(e, 0).unwrap();
        let snap = s
            .snapshot(&Selector::Ids(vec!["a".into()]), &policy(false, 0))
            .unwrap();
        assert_eq!(snap.entities[0].description.chars().count(), 1000);
        assert!(!snap.canonical().contains("raw-value"));
        assert!(!snap.canonical().contains("secret"));
    }

    #[test]
    fn validity_windows_are_respected() {
        let mut s = store();
        let mut now_only = spec("a", "A", Assertion::ObservedFact);
        now_only.valid_from = Some(100);
        now_only.valid_to = Some(200);
        s.create_entity(now_only, 0).unwrap();
        let sel = Selector::Ids(vec!["a".into()]);
        assert_eq!(
            s.snapshot(&sel, &policy(false, 99)),
            Err(ContextError::NothingTrusted)
        );
        assert!(s.snapshot(&sel, &policy(false, 100)).is_ok());
        assert!(s.snapshot(&sel, &policy(false, 200)).is_ok());
        assert_eq!(
            s.snapshot(&sel, &policy(false, 201)),
            Err(ContextError::NothingTrusted)
        );
    }

    #[test]
    fn only_validated_evidence_for_the_selected_version_is_included() {
        let mut s = with_evidence();
        let reason = "verified against the stored configuration file";
        s.add_evidence(
            "ev-rejected",
            "a",
            1,
            None,
            SourceKind::EventLog,
            "log://r",
            Some(H2),
            11,
            0.5,
            "alice",
        )
        .unwrap();
        s.add_evidence(
            "ev-pending",
            "a",
            1,
            None,
            SourceKind::EventLog,
            "log://p",
            Some(H2),
            12,
            0.5,
            "alice",
        )
        .unwrap();
        s.review_evidence("ev1", "bob", Review::Validated, reason, Some(H1), 20)
            .unwrap();
        s.review_evidence("ev-rejected", "bob", Review::Rejected, reason, None, 21)
            .unwrap();
        let sel = Selector::Ids(vec!["a".into()]);
        let snap = s.snapshot(&sel, &policy(false, 50)).unwrap();
        assert_eq!(
            snap.evidence
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["ev1"]
        );
        // a new entity version no longer carries the old version's evidence
        s.version_entity(spec("a", "A renamed", Assertion::ObservedFact), 60)
            .unwrap();
        assert!(s
            .snapshot(&sel, &policy(false, 61))
            .unwrap()
            .evidence
            .is_empty());
    }

    #[test]
    fn relationships_must_match_the_selected_entity_versions() {
        let mut s = graph();
        s.version_entity(spec("db", "Payments DB (v2)", Assertion::HumanValidated), 5)
            .unwrap();
        let snap = s
            .snapshot(&Selector::Search("payments".into()), &policy(false, 100))
            .unwrap();
        assert!(snap.entities.iter().any(|e| e.id == "db" && e.version == 2));
        assert!(
            snap.relationships.is_empty(),
            "api-db points at db@1, but the snapshot shows db@2"
        );
        // re-assert the relationship against the new version
        s.record_relationship(
            RelationshipSpec {
                id: "api-db".into(),
                kind: "depends-on".into(),
                source: ("api".into(), 1),
                target: ("db".into(), 2),
                assertion: Assertion::ObservedFact,
                confidence: 0.9,
                valid_from: None,
                valid_to: None,
            },
            6,
        )
        .unwrap();
        let snap = s
            .snapshot(&Selector::Search("payments".into()), &policy(false, 100))
            .unwrap();
        assert_eq!(
            (snap.relationships.len(), snap.relationships[0].version),
            (1, 2)
        );
    }

    #[test]
    fn selector_validation_and_limits() {
        let s = graph();
        let p = |limit| Policy {
            include_inferences: false,
            now: 0,
            limit,
        };
        assert!(s.snapshot(&Selector::Ids(vec![]), &p(5)).is_err());
        assert!(s
            .snapshot(&Selector::Ids(vec!["api".into(), "api".into()]), &p(5))
            .is_err());
        assert!(s
            .snapshot(&Selector::Ids(vec!["api".into(), "db".into()]), &p(1))
            .is_err());
        assert!(matches!(
            s.snapshot(&Selector::Ids(vec!["ghost".into()]), &p(5)),
            Err(ContextError::NotFound(_))
        ));
        assert!(s.snapshot(&Selector::Search("  ".into()), &p(5)).is_err());
        assert!(s.snapshot(&Selector::Search("zzz".into()), &p(5)).is_err());
        assert!(s
            .snapshot(&Selector::Ids(vec!["api".into()]), &p(0))
            .is_err());
        assert!(s
            .snapshot(&Selector::Ids(vec!["api".into()]), &p(MAX_ENTITIES + 1))
            .is_err());
        // limit applies to the entity count
        let two = s
            .snapshot(&Selector::Search("payments".into()), &p(1))
            .unwrap();
        assert_eq!(two.entities.len(), 1);
    }

    #[test]
    fn snapshots_are_deterministic_and_bound_to_the_tenant() {
        let s = graph();
        let sel = Selector::Search("payments".into());
        let a = s.snapshot(&sel, &policy(false, 100)).unwrap();
        let b = s.snapshot(&sel, &policy(false, 100)).unwrap();
        assert_eq!(a, b);
        assert!(a.reference.starts_with("context-snapshot:") && a.content_hash.len() == 64);
        assert_eq!(a.byte_length, a.canonical().len());
        // the same content under another tenant hashes differently
        let mut other = ContextStore::new("tenant-2").unwrap();
        for e in ["api", "db"] {
            other
                .create_entity(s.latest_entity(e).unwrap().spec.clone(), 0)
                .unwrap();
        }
        other
            .record_relationship(s.relationships.get("api-db").unwrap()[0].spec.clone(), 0)
            .unwrap();
        let c = other.snapshot(&sel, &policy(false, 100)).unwrap();
        assert_eq!(a.canonical(), c.canonical());
        assert_ne!(a.content_hash, c.content_hash);
    }

    #[test]
    fn oversized_snapshots_are_refused() {
        let mut s = store();
        for i in 0..12 {
            let mut e = spec(
                &format!("e{i}"),
                &format!("Entity {i}"),
                Assertion::ObservedFact,
            );
            e.description = "d".repeat(1000);
            s.create_entity(e, 0).unwrap();
        }
        let sel = Selector::Search("entity".into());
        let reason = "verified against the stored configuration";
        // a few small pieces of evidence still fit
        for n in 0..3 {
            let id = format!("ok{n}");
            s.add_evidence(
                &id,
                "e0",
                1,
                None,
                SourceKind::Other,
                "short",
                Some(H1),
                1,
                0.5,
                "alice",
            )
            .unwrap();
            s.review_evidence(&id, "bob", Review::Validated, reason, Some(H1), 2)
                .unwrap();
        }
        assert!(s.snapshot(&sel, &policy(false, 0)).unwrap().byte_length <= MAX_SNAPSHOT_BYTES);
        // 36 validated items with long source references push the packet past the cap
        for n in 0..MAX_EVIDENCE {
            let id = format!("big{n:02}");
            s.add_evidence(
                &id,
                "e1",
                1,
                None,
                SourceKind::Other,
                &"r".repeat(1024),
                Some(H1),
                1,
                0.5,
                "alice",
            )
            .unwrap();
            s.review_evidence(&id, "bob", Review::Validated, reason, Some(H1), 2)
                .unwrap();
        }
        assert_eq!(
            s.snapshot(&sel, &policy(false, 0)),
            Err(ContextError::TooLarge)
        );
        // a narrower selection that leaves the heavy evidence out still works
        assert!(s
            .snapshot(&Selector::Ids(vec!["e0".into()]), &policy(false, 0))
            .is_ok());
    }

    #[test]
    fn query_filters_orders_and_pages() {
        let s = graph();
        let all = s.query_entities(None, None, None, 100, 0).unwrap();
        assert_eq!(all.len(), 5);
        let names: Vec<&str> = all.iter().map(|e| e.spec.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(
            s.query_entities(None, Some(Assertion::Inferred), None, 10, 0)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            s.query_entities(Some("service"), None, Some("PAYMENTS"), 10, 0)
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            s.query_entities(Some("table"), None, None, 10, 0)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(s.query_entities(None, None, None, 2, 4).unwrap().len(), 1);
        assert!(s.query_entities(None, None, None, 0, 0).is_err());
        assert!(s.query_entities(None, None, None, 101, 0).is_err());
        assert!(s.query_entities(None, None, Some(" "), 10, 0).is_err());
    }

    #[test]
    fn constant_time_compare_and_hash_helpers() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(is_sha256_hex(H1));
        assert!(is_sha256_hex(&"a".repeat(64)));
        assert!(
            !is_sha256_hex(&"A".repeat(64)),
            "only lowercase digests are accepted"
        );
        assert!(!is_sha256_hex("abc"));
        assert_eq!(
            sha("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
