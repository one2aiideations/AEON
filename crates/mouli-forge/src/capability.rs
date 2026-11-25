//! Declarative capability contracts and a tenant-scoped immutable registry.
//!
//! A contract describes *what* a capability takes and returns, who may use it
//! and what must be audited. It holds no code and no credentials, and the
//! registry never resolves `auth_ref` or `implementation_ref`.
//!
//! Embedded JSON Schema bodies are not stored: only the top-level payload type
//! is kept (`PayloadType`). Full schema validation needs a JSON Schema crate.

use crate::manifest::is_semver;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Top-level JSON type of a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadType {
    /// Object.
    Object,
    /// Array.
    Array,
    /// String.
    String,
    /// Number.
    Number,
    /// Integer.
    Integer,
    /// Boolean.
    Boolean,
}

impl PayloadType {
    fn as_str(self) -> &'static str {
        match self {
            PayloadType::Object => "object",
            PayloadType::Array => "array",
            PayloadType::String => "string",
            PayloadType::Number => "number",
            PayloadType::Integer => "integer",
            PayloadType::Boolean => "boolean",
        }
    }
}

/// Who a capability acts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalScope {
    /// One user.
    User,
    /// A whole tenant.
    Tenant,
    /// A service account.
    Service,
    /// The system itself.
    System,
}

impl PrincipalScope {
    fn as_str(self) -> &'static str {
        match self {
            PrincipalScope::User => "user",
            PrincipalScope::Tenant => "tenant",
            PrincipalScope::Service => "service",
            PrincipalScope::System => "system",
        }
    }
}

/// Audit events a contract can require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuditEvent {
    /// Invocation requested.
    Requested,
    /// Authorization granted.
    Authorized,
    /// Authorization refused.
    Denied,
    /// Execution began.
    Started,
    /// Execution finished.
    Completed,
    /// Execution failed.
    Failed,
    /// Execution timed out.
    TimedOut,
}

impl AuditEvent {
    fn as_str(self) -> &'static str {
        match self {
            AuditEvent::Requested => "requested",
            AuditEvent::Authorized => "authorized",
            AuditEvent::Denied => "denied",
            AuditEvent::Started => "started",
            AuditEvent::Completed => "completed",
            AuditEvent::Failed => "failed",
            AuditEvent::TimedOut => "timed_out",
        }
    }
}

/// Audit requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditSpec {
    /// Events to record (1 to 20).
    pub events: BTreeSet<AuditEvent>,
    /// Record inputs (needs a redaction profile).
    pub record_input: bool,
    /// Record outputs.
    pub record_output: bool,
    /// Redaction profile reference.
    pub redaction_profile_ref: Option<String>,
}

/// A capability contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// Id: `^[a-z0-9][a-z0-9._-]{2,127}$`.
    pub capability_id: String,
    /// Semantic version.
    pub version: String,
    /// Display name (1 to 120).
    pub name: String,
    /// Description (1 to 2000).
    pub description: String,
    /// Input payload type.
    pub input: PayloadType,
    /// Output payload type.
    pub output: PayloadType,
    /// Reference to the credential source (never the credential).
    pub auth_ref: String,
    /// Principal scope.
    pub principal_scope: PrincipalScope,
    /// Permitted actions (1 to 100).
    pub actions: BTreeSet<String>,
    /// Resource scope, if limited.
    pub resource_scope: Option<String>,
    /// Timeout in seconds, 1 to 3600.
    pub timeout_seconds: u32,
    /// Audit requirements.
    pub audit: AuditSpec,
    /// Opaque implementation reference; never loaded here.
    pub implementation_ref: Option<String>,
    /// Tags (up to 50, each 1 to 64).
    pub tags: BTreeSet<String>,
    /// Creation time (RFC 3339), supplied by the caller.
    pub created_at: String,
}

/// Contract or registry failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    /// Contract rule broken: `(path, message)` pairs, sorted.
    Invalid(Vec<(String, String)>),
    /// Version exists with different content.
    Conflict(String),
    /// No such version for this tenant.
    NotFound(String),
    /// Stored content no longer matches its hash.
    Corrupt(String),
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CapabilityError::Invalid(e) => {
                let parts: Vec<String> = e.iter().map(|(p, m)| format!("{p}: {m}")).collect();
                write!(f, "invalid capability contract: {}", parts.join("; "))
            }
            CapabilityError::Conflict(s) => write!(f, "{s} is immutable and already exists"),
            CapabilityError::NotFound(s) => write!(f, "{s} was not found"),
            CapabilityError::Corrupt(s) => {
                write!(f, "stored capability content hash mismatch: {s}")
            }
        }
    }
}

impl std::error::Error for CapabilityError {}

fn valid_id(s: &str) -> bool {
    let b = s.as_bytes();
    (3..=128).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

fn valid_auth_ref(s: &str) -> bool {
    let b = s.as_bytes();
    (3..=512).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'/' | b'@' | b'-')
        })
}

impl Contract {
    /// All rule violations, sorted by path then message. Empty means valid.
    pub fn violations(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = Vec::new();
        let mut bad = |p: &str, m: &str| v.push((p.to_string(), m.to_string()));
        if !valid_id(&self.capability_id) {
            bad("/capability_id", "must match ^[a-z0-9][a-z0-9._-]{2,127}$");
        }
        if !is_semver(&self.version) {
            bad("/version", "must be a semantic version");
        }
        if self.name.is_empty() || self.name.len() > 120 {
            bad("/name", "must be 1 to 120 characters");
        }
        if self.description.is_empty() || self.description.len() > 2000 {
            bad("/description", "must be 1 to 2000 characters");
        }
        if !valid_auth_ref(&self.auth_ref) {
            bad("/auth_ref", "must be a 3 to 512 character reference");
        }
        if self.actions.is_empty()
            || self.actions.len() > 100
            || self.actions.iter().any(|a| a.is_empty() || a.len() > 128)
        {
            bad(
                "/permission_profile/actions",
                "1 to 100 actions of 1 to 128 characters",
            );
        }
        if self.resource_scope.as_ref().is_some_and(|s| s.len() > 512) {
            bad(
                "/permission_profile/resource_scope",
                "at most 512 characters",
            );
        }
        if !(1..=3600).contains(&self.timeout_seconds) {
            bad("/timeout_seconds", "must be 1 to 3600");
        }
        if self.audit.events.is_empty() || self.audit.events.len() > 20 {
            bad("/audit/events", "1 to 20 events");
        }
        if self.audit.record_input
            && self
                .audit
                .redaction_profile_ref
                .as_deref()
                .is_none_or(str::is_empty)
        {
            bad(
                "/audit/redaction_profile_ref",
                "Recording input requires a redaction profile reference.",
            );
        }
        if self
            .audit
            .redaction_profile_ref
            .as_ref()
            .is_some_and(|s| s.len() < 3 || s.len() > 512)
        {
            bad(
                "/audit/redaction_profile_ref",
                "must be 3 to 512 characters",
            );
        }
        if self
            .implementation_ref
            .as_ref()
            .is_some_and(|s| s.len() < 3 || s.len() > 512)
        {
            bad("/implementation_ref", "must be 3 to 512 characters");
        }
        if self.tags.len() > 50 || self.tags.iter().any(|t| t.is_empty() || t.len() > 64) {
            bad("/tags", "up to 50 tags of 1 to 64 characters");
        }
        if self.created_at.trim().is_empty() {
            bad("/provenance/created_at", "is required");
        }
        v.sort();
        v
    }

    /// Deterministic text form (fixed field order, sorted sets).
    pub fn canonical(&self) -> String {
        let q = |s: &str| format!("{s:?}");
        let set = |it: Vec<String>| format!("[{}]", it.join(","));
        let opt = |o: &Option<String>| o.as_deref().map_or("null".to_string(), q);
        format!(
            "{{id:{},version:{},name:{},description:{},input:{},output:{},auth_ref:{},scope:{},actions:{},resource_scope:{},timeout:{},audit:{{events:{},record_input:{},record_output:{},redaction:{}}},implementation_ref:{},tags:{},created_at:{}}}",
            q(&self.capability_id),
            q(&self.version),
            q(&self.name),
            q(&self.description),
            self.input.as_str(),
            self.output.as_str(),
            q(&self.auth_ref),
            self.principal_scope.as_str(),
            set(self.actions.iter().map(|s| q(s)).collect()),
            opt(&self.resource_scope),
            self.timeout_seconds,
            set(self.audit.events.iter().map(|e| e.as_str().to_string()).collect()),
            self.audit.record_input,
            self.audit.record_output,
            opt(&self.audit.redaction_profile_ref),
            opt(&self.implementation_ref),
            set(self.tags.iter().map(|s| q(s)).collect()),
            q(&self.created_at),
        )
    }

    /// SHA-256 of the canonical form, lowercase hex.
    pub fn content_hash(&self) -> String {
        Sha256::digest(self.canonical().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// A stored contract version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRecord {
    /// Owning tenant.
    pub tenant_id: String,
    /// The contract.
    pub contract: Contract,
    /// Hash recorded at registration.
    pub content_hash: String,
}

/// Tenant-isolated, immutable capability contract versions.
#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    records: BTreeMap<(String, String, String), CapabilityRecord>,
}

impl CapabilityRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a contract. Returns the record and whether it was newly stored.
    pub fn register(
        &mut self,
        tenant_id: &str,
        contract: Contract,
    ) -> Result<(CapabilityRecord, bool), CapabilityError> {
        if tenant_id.trim().is_empty() || tenant_id.len() > 128 {
            return Err(CapabilityError::Invalid(vec![(
                "/tenant".into(),
                "A valid tenant identity is required".into(),
            )]));
        }
        let violations = contract.violations();
        if !violations.is_empty() {
            return Err(CapabilityError::Invalid(violations));
        }
        let key = (
            tenant_id.to_string(),
            contract.capability_id.clone(),
            contract.version.clone(),
        );
        let hash = contract.content_hash();
        if let Some(existing) = self.records.get(&key) {
            if existing.content_hash != hash {
                return Err(CapabilityError::Conflict(format!("{}@{}", key.1, key.2)));
            }
            return Ok((existing.clone(), false));
        }
        let record = CapabilityRecord {
            tenant_id: tenant_id.to_string(),
            contract,
            content_hash: hash,
        };
        self.records.insert(key, record.clone());
        Ok((record, true))
    }

    /// Fetch a version, verifying its stored hash.
    pub fn get(
        &self,
        tenant_id: &str,
        capability_id: &str,
        version: &str,
    ) -> Result<&CapabilityRecord, CapabilityError> {
        let rec = self
            .records
            .get(&(
                tenant_id.to_string(),
                capability_id.to_string(),
                version.to_string(),
            ))
            .ok_or_else(|| CapabilityError::NotFound(format!("{capability_id}@{version}")))?;
        if rec.contract.content_hash() != rec.content_hash {
            return Err(CapabilityError::Corrupt(format!(
                "{capability_id}@{version}"
            )));
        }
        Ok(rec)
    }

    /// A tenant's versions ordered by id, then newest first. `limit` 1..=100.
    pub fn list(
        &self,
        tenant_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<&CapabilityRecord>, CapabilityError> {
        if !(1..=100).contains(&limit) {
            return Err(CapabilityError::Invalid(vec![(
                "/limit".into(),
                "limit must be 1..100".into(),
            )]));
        }
        let mut rows: Vec<&CapabilityRecord> = self
            .records
            .values()
            .filter(|r| r.tenant_id == tenant_id)
            .collect();
        rows.sort_by(|a, b| {
            a.contract
                .capability_id
                .cmp(&b.contract.capability_id)
                .then_with(|| {
                    (&b.contract.created_at, &b.contract.version)
                        .cmp(&(&a.contract.created_at, &a.contract.version))
                })
        });
        let page: Vec<&CapabilityRecord> = rows.into_iter().skip(offset).take(limit).collect();
        for r in &page {
            if r.contract.content_hash() != r.content_hash {
                return Err(CapabilityError::Corrupt(format!(
                    "{}@{}",
                    r.contract.capability_id, r.contract.version
                )));
            }
        }
        Ok(page)
    }

    #[cfg(test)]
    fn tamper(&mut self, tenant: &str, id: &str, version: &str) {
        let key = (tenant.to_string(), id.to_string(), version.to_string());
        if let Some(r) = self.records.get_mut(&key) {
            r.contract.name.push('!');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(id: &str, version: &str, created: &str) -> Contract {
        Contract {
            capability_id: id.into(),
            version: version.into(),
            name: "Search".into(),
            description: "Search tickets".into(),
            input: PayloadType::Object,
            output: PayloadType::Array,
            auth_ref: "vault:tickets/read".into(),
            principal_scope: PrincipalScope::Tenant,
            actions: BTreeSet::from(["tickets.read".to_string()]),
            resource_scope: None,
            timeout_seconds: 30,
            audit: AuditSpec {
                events: BTreeSet::from([AuditEvent::Requested, AuditEvent::Completed]),
                record_input: false,
                record_output: false,
                redaction_profile_ref: None,
            },
            implementation_ref: None,
            tags: BTreeSet::new(),
            created_at: created.into(),
        }
    }

    #[test]
    fn valid_contract_registers_once() {
        let mut r = CapabilityRegistry::new();
        let (rec, created) = r
            .register("t", contract("tickets.search", "1.0.0", "2026-01-01"))
            .unwrap();
        assert!(created);
        assert_eq!(rec.content_hash.len(), 64);
        let (_, again) = r
            .register("t", contract("tickets.search", "1.0.0", "2026-01-01"))
            .unwrap();
        assert!(!again);
    }

    #[test]
    fn changed_content_conflicts_and_tenants_are_isolated() {
        let mut r = CapabilityRegistry::new();
        r.register("t1", contract("tickets.search", "1.0.0", "2026-01-01"))
            .unwrap();
        let mut changed = contract("tickets.search", "1.0.0", "2026-01-01");
        changed.timeout_seconds = 31;
        assert!(matches!(
            r.register("t1", changed.clone()),
            Err(CapabilityError::Conflict(_))
        ));
        assert!(r.register("t2", changed).is_ok());
        assert!(matches!(
            r.get("t3", "tickets.search", "1.0.0"),
            Err(CapabilityError::NotFound(_))
        ));
    }

    #[test]
    fn violations_are_reported_sorted() {
        let mut c = contract("A", "x", "");
        c.timeout_seconds = 0;
        c.actions.clear();
        let v = c.violations();
        let paths: Vec<&str> = v.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/capability_id",
                "/permission_profile/actions",
                "/provenance/created_at",
                "/timeout_seconds",
                "/version"
            ]
        );
        assert!(matches!(
            CapabilityRegistry::new().register("t", c),
            Err(CapabilityError::Invalid(_))
        ));
    }

    #[test]
    fn recording_input_requires_redaction_profile() {
        let mut c = contract("tickets.search", "1.0.0", "2026-01-01");
        c.audit.record_input = true;
        assert!(c
            .violations()
            .iter()
            .any(|(p, _)| p == "/audit/redaction_profile_ref"));
        c.audit.redaction_profile_ref = Some("redact:pii".into());
        assert!(c.violations().is_empty());
    }

    #[test]
    fn id_and_auth_ref_patterns() {
        assert!(valid_id("a.b-c_9"));
        assert!(!valid_id("ab"));
        assert!(!valid_id("-abc"));
        assert!(!valid_id("Abc"));
        assert!(valid_auth_ref("vault:a/b@c"));
        assert!(!valid_auth_ref("a b c"));
        assert!(!valid_auth_ref(":abc"));
    }

    #[test]
    fn list_orders_and_pages() {
        let mut r = CapabilityRegistry::new();
        r.register("t", contract("b.cap", "1.0.0", "2026-01-01"))
            .unwrap();
        r.register("t", contract("a.cap", "1.0.0", "2026-01-01"))
            .unwrap();
        r.register("t", contract("a.cap", "1.1.0", "2026-02-01"))
            .unwrap();
        let all: Vec<String> = r
            .list("t", 10, 0)
            .unwrap()
            .iter()
            .map(|x| format!("{}@{}", x.contract.capability_id, x.contract.version))
            .collect();
        assert_eq!(all, ["a.cap@1.1.0", "a.cap@1.0.0", "b.cap@1.0.0"]);
        assert_eq!(r.list("t", 1, 2).unwrap().len(), 1);
        assert!(r.list("t", 0, 0).is_err());
        assert!(r.list("t", 101, 0).is_err());
    }

    #[test]
    fn tampering_is_detected_on_read() {
        let mut r = CapabilityRegistry::new();
        r.register("t", contract("tickets.search", "1.0.0", "2026-01-01"))
            .unwrap();
        r.tamper("t", "tickets.search", "1.0.0");
        assert!(matches!(
            r.get("t", "tickets.search", "1.0.0"),
            Err(CapabilityError::Corrupt(_))
        ));
        assert!(matches!(
            r.list("t", 10, 0),
            Err(CapabilityError::Corrupt(_))
        ));
    }

    #[test]
    fn hash_ignores_set_insertion_order() {
        let mut a = contract("tickets.search", "1.0.0", "2026-01-01");
        a.actions = BTreeSet::from(["z".to_string(), "a".to_string()]);
        let mut b = contract("tickets.search", "1.0.0", "2026-01-01");
        b.actions = BTreeSet::from(["a".to_string(), "z".to_string()]);
        assert_eq!(a.content_hash(), b.content_hash());
        b.tags.insert("x".into());
        assert_ne!(a.content_hash(), b.content_hash());
    }
}
