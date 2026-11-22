//! Immutable, tenant-scoped agent version registry (in memory).

use crate::manifest::{Manifest, ManifestError};
use std::collections::BTreeMap;
use std::fmt;

/// Registry failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// The version exists with different content or a different parent.
    Conflict(String),
    /// No such version.
    NotFound(String),
    /// The manifest is invalid.
    Invalid(ManifestError),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::Conflict(s) => write!(f, "{s} is immutable and already exists"),
            RegistryError::NotFound(s) => write!(f, "{s} was not found"),
            RegistryError::Invalid(e) => write!(f, "invalid manifest: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// A stored version.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// Owning tenant.
    pub tenant_id: String,
    /// The manifest.
    pub manifest: Manifest,
    /// SHA-256 of the canonical manifest.
    pub content_hash: String,
    /// `agent@version` this one was cloned or derived from.
    pub parent_artifact_id: Option<String>,
}

/// Tenant-scoped version store. Versions are never overwritten or deleted.
#[derive(Debug, Default)]
pub struct Registry {
    records: BTreeMap<(String, String, String), Record>,
}

impl Registry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a version. Returns the record and whether it was newly created.
    /// Re-submitting identical content and parent is idempotent; anything else
    /// under an existing version is a [`RegistryError::Conflict`].
    pub fn create(
        &mut self,
        tenant_id: &str,
        manifest: Manifest,
        parent_artifact_id: Option<String>,
    ) -> Result<(Record, bool), RegistryError> {
        manifest.validate().map_err(RegistryError::Invalid)?;
        let key = (
            tenant_id.to_string(),
            manifest.agent_id.clone(),
            manifest.version.clone(),
        );
        let hash = manifest.content_hash();
        if let Some(existing) = self.records.get(&key) {
            if existing.content_hash != hash || existing.parent_artifact_id != parent_artifact_id {
                return Err(RegistryError::Conflict(format!("{}@{}", key.1, key.2)));
            }
            return Ok((existing.clone(), false));
        }
        let record = Record {
            tenant_id: tenant_id.to_string(),
            manifest,
            content_hash: hash,
            parent_artifact_id,
        };
        self.records.insert(key, record.clone());
        Ok((record, true))
    }

    /// Fetch one version.
    pub fn get(
        &self,
        tenant_id: &str,
        agent_id: &str,
        version: &str,
    ) -> Result<&Record, RegistryError> {
        self.records
            .get(&(
                tenant_id.to_string(),
                agent_id.to_string(),
                version.to_string(),
            ))
            .ok_or_else(|| RegistryError::NotFound(format!("{agent_id}@{version}")))
    }

    /// Versions of an agent, newest first (`created_at`, then `version`, descending).
    pub fn list(
        &self,
        tenant_id: &str,
        agent_id: &str,
        limit: usize,
        offset: usize,
    ) -> Vec<&Record> {
        let mut rows: Vec<&Record> = self
            .records
            .values()
            .filter(|r| r.tenant_id == tenant_id && r.manifest.agent_id == agent_id)
            .collect();
        rows.sort_by(|a, b| {
            (&b.manifest.created_at, &b.manifest.version)
                .cmp(&(&a.manifest.created_at, &a.manifest.version))
        });
        rows.into_iter().skip(offset).take(limit).collect()
    }

    /// Walk `parent_artifact_id` links from a version back to its root.
    /// The first element is the starting version. Stops at a missing parent.
    pub fn lineage(&self, tenant_id: &str, agent_id: &str, version: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Some(format!("{agent_id}@{version}"));
        while let Some(id) = cur {
            if out.contains(&id) {
                break; // defensive: never loop on corrupt data
            }
            let Some((a, v)) = id.split_once('@') else {
                break;
            };
            let next = self
                .records
                .get(&(tenant_id.to_string(), a.to_string(), v.to_string()));
            out.push(id.clone());
            cur = next.and_then(|r| r.parent_artifact_id.clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, Origin};
    use std::collections::{BTreeMap, BTreeSet};

    fn m(version: &str, created_at: &str) -> Manifest {
        Manifest {
            agent_id: "a".into(),
            version: version.into(),
            max_steps: 5,
            timeout_seconds: 30,
            model_parameters: BTreeMap::new(),
            permissions: BTreeSet::new(),
            tools: BTreeSet::new(),
            origin: Origin::Authored,
            created_at: created_at.into(),
            created_by: "t".into(),
            source_refs: vec![],
        }
    }

    #[test]
    fn create_is_idempotent_for_identical_content() {
        let mut r = Registry::new();
        let (_, created) = r.create("t1", m("1.0.0", "2026-01-01"), None).unwrap();
        assert!(created);
        let (_, created) = r.create("t1", m("1.0.0", "2026-01-01"), None).unwrap();
        assert!(!created);
    }

    #[test]
    fn different_content_or_parent_conflicts() {
        let mut r = Registry::new();
        r.create("t1", m("1.0.0", "2026-01-01"), None).unwrap();
        let mut changed = m("1.0.0", "2026-01-01");
        changed.max_steps = 6;
        assert!(matches!(
            r.create("t1", changed, None),
            Err(RegistryError::Conflict(_))
        ));
        assert!(matches!(
            r.create("t1", m("1.0.0", "2026-01-01"), Some("a@0.9.0".into())),
            Err(RegistryError::Conflict(_))
        ));
    }

    #[test]
    fn tenants_are_isolated() {
        let mut r = Registry::new();
        r.create("t1", m("1.0.0", "2026-01-01"), None).unwrap();
        assert!(r.get("t1", "a", "1.0.0").is_ok());
        assert!(matches!(
            r.get("t2", "a", "1.0.0"),
            Err(RegistryError::NotFound(_))
        ));
        // The same version under another tenant is a separate record.
        let mut other = m("1.0.0", "2026-01-01");
        other.max_steps = 9;
        assert!(r.create("t2", other, None).is_ok());
    }

    #[test]
    fn invalid_manifest_is_rejected() {
        let mut r = Registry::new();
        let mut bad = m("1.0.0", "2026-01-01");
        bad.max_steps = 0;
        assert!(matches!(
            r.create("t", bad, None),
            Err(RegistryError::Invalid(_))
        ));
    }

    #[test]
    fn list_orders_newest_first_with_paging() {
        let mut r = Registry::new();
        r.create("t", m("1.0.0", "2026-01-01"), None).unwrap();
        r.create("t", m("1.1.0", "2026-02-01"), None).unwrap();
        r.create("t", m("1.2.0", "2026-03-01"), None).unwrap();
        let all: Vec<_> = r
            .list("t", "a", 10, 0)
            .iter()
            .map(|x| x.manifest.version.clone())
            .collect();
        assert_eq!(all, ["1.2.0", "1.1.0", "1.0.0"]);
        let page: Vec<_> = r
            .list("t", "a", 1, 1)
            .iter()
            .map(|x| x.manifest.version.clone())
            .collect();
        assert_eq!(page, ["1.1.0"]);
    }

    #[test]
    fn lineage_follows_parents() {
        let mut r = Registry::new();
        r.create("t", m("1.0.0", "2026-01-01"), None).unwrap();
        r.create("t", m("1.1.0", "2026-02-01"), Some("a@1.0.0".into()))
            .unwrap();
        r.create("t", m("1.2.0", "2026-03-01"), Some("a@1.1.0".into()))
            .unwrap();
        assert_eq!(
            r.lineage("t", "a", "1.2.0"),
            ["a@1.2.0", "a@1.1.0", "a@1.0.0"]
        );
        assert_eq!(r.lineage("t", "a", "1.0.0"), ["a@1.0.0"]);
    }
}
