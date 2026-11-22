//! Capability registry: the tools agents and engines may call.
//!
//! A capability declares what it accepts and returns, which permissions a
//! caller needs, and its expected cost and latency so planners can choose
//! between alternatives.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

/// Description of one callable capability.
#[derive(Debug, Clone, PartialEq)]
pub struct Capability {
    /// Stable name, e.g. "files.search".
    pub name: String,
    /// Version (higher replaces lower for the same name).
    pub version: u32,
    /// Input schema (JSON Schema text or a type name).
    pub input_schema: String,
    /// Output schema.
    pub output_schema: String,
    /// Permissions a caller must hold.
    pub permissions: BTreeSet<String>,
    /// Relative cost per call.
    pub cost: f32,
    /// Expected latency in milliseconds.
    pub latency_ms: u32,
    /// Provider (component or user id).
    pub owner: String,
}

/// Registry errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapError {
    /// Name empty.
    BadName,
    /// Same or newer version already registered.
    Stale,
    /// Not registered.
    Unknown,
    /// Caller lacks these permissions.
    Missing(Vec<String>),
}

impl fmt::Display for CapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadName => f.write_str("capability name empty"),
            Self::Stale => f.write_str("same or newer version registered"),
            Self::Unknown => f.write_str("unknown capability"),
            Self::Missing(p) => write!(f, "missing permissions: {}", p.join(", ")),
        }
    }
}

impl std::error::Error for CapError {}

/// Registered capabilities by name.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    caps: BTreeMap<String, Capability>,
}

impl CapabilityRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or upgrade a capability.
    pub fn register(&mut self, c: Capability) -> Result<(), CapError> {
        if c.name.trim().is_empty() {
            return Err(CapError::BadName);
        }
        if self
            .caps
            .get(&c.name)
            .is_some_and(|old| old.version >= c.version)
        {
            return Err(CapError::Stale);
        }
        self.caps.insert(c.name.clone(), c);
        Ok(())
    }

    /// Look up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Capability> {
        self.caps.get(name)
    }

    /// Check that `held` covers `name`'s permissions.
    pub fn authorize(&self, name: &str, held: &BTreeSet<String>) -> Result<&Capability, CapError> {
        let c = self.caps.get(name).ok_or(CapError::Unknown)?;
        let missing: Vec<String> = c.permissions.difference(held).cloned().collect();
        if missing.is_empty() {
            Ok(c)
        } else {
            Err(CapError::Missing(missing))
        }
    }

    /// Capabilities whose name starts with `prefix`, cheapest first, that
    /// `held` may call and that meet `max_latency_ms`.
    #[must_use]
    pub fn candidates(
        &self,
        prefix: &str,
        held: &BTreeSet<String>,
        max_latency_ms: u32,
    ) -> Vec<&Capability> {
        let mut v: Vec<&Capability> = self
            .caps
            .values()
            .filter(|c| {
                c.name.starts_with(prefix)
                    && c.latency_ms <= max_latency_ms
                    && c.permissions.is_subset(held)
            })
            .collect();
        v.sort_by(|a, b| a.cost.total_cmp(&b.cost));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(name: &str, version: u32, perms: &[&str], cost: f32, latency_ms: u32) -> Capability {
        Capability {
            name: name.into(),
            version,
            input_schema: "Query".into(),
            output_schema: "Hits".into(),
            permissions: perms.iter().map(|p| p.to_string()).collect(),
            cost,
            latency_ms,
            owner: "system".into(),
        }
    }

    fn held(p: &[&str]) -> BTreeSet<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn versions_and_permissions() {
        let mut r = CapabilityRegistry::new();
        r.register(cap("files.search", 1, &["files.read"], 1.0, 50))
            .unwrap();
        assert_eq!(
            r.register(cap("files.search", 1, &[], 1.0, 50)),
            Err(CapError::Stale)
        );
        r.register(cap("files.search", 2, &["files.read"], 0.5, 20))
            .unwrap();
        assert_eq!(r.get("files.search").unwrap().version, 2);
        assert!(r.authorize("files.search", &held(&["files.read"])).is_ok());
        assert_eq!(
            r.authorize("files.search", &held(&[])),
            Err(CapError::Missing(vec!["files.read".into()]))
        );
        assert_eq!(r.authorize("nope", &held(&[])), Err(CapError::Unknown));
    }

    #[test]
    fn candidate_ranking() {
        let mut r = CapabilityRegistry::new();
        r.register(cap("web.fetch.fast", 1, &["net"], 3.0, 100))
            .unwrap();
        r.register(cap("web.fetch.cheap", 1, &["net"], 1.0, 900))
            .unwrap();
        r.register(cap("web.fetch.admin", 1, &["net", "admin"], 0.1, 10))
            .unwrap();
        let h = held(&["net"]);
        let all: Vec<&str> = r
            .candidates("web.fetch", &h, 1000)
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(all, vec!["web.fetch.cheap", "web.fetch.fast"]);
        assert_eq!(r.candidates("web.fetch", &h, 200).len(), 1);
    }
}
