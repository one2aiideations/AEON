//! Agent manifest: typed, canonically serialised and content-hashed.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// How a manifest version came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Written by a person or imported.
    Authored,
    /// Cloned from another version.
    Cloned,
    /// Produced by candidate generation.
    Derived,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Origin::Authored => "authored",
            Origin::Cloned => "cloned",
            Origin::Derived => "derived",
        }
    }
}

/// Manifest validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// A field is empty or too long.
    BadField(&'static str),
    /// The version is not semantic-version text.
    BadVersion(String),
    /// A numeric field is out of range.
    OutOfRange(&'static str),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::BadField(n) => write!(f, "invalid field: {n}"),
            ManifestError::BadVersion(v) => write!(f, "not a semantic version: {v}"),
            ManifestError::OutOfRange(n) => write!(f, "out of range: {n}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// An agent version.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Stable agent identity.
    pub agent_id: String,
    /// Semantic version of this manifest.
    pub version: String,
    /// Maximum reasoning/tool steps per run, 1..=100.
    pub max_steps: u32,
    /// Wall-clock limit per run in seconds, 1..=3600.
    pub timeout_seconds: u32,
    /// Numeric model parameters (`temperature`, `top_p`, ...).
    pub model_parameters: BTreeMap<String, f64>,
    /// Granted permissions (protected: never mutated).
    pub permissions: BTreeSet<String>,
    /// Allowed tools (protected: never mutated).
    pub tools: BTreeSet<String>,
    /// Where this version came from.
    pub origin: Origin,
    /// Creation time as RFC 3339 text, supplied by the caller.
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// Provenance references (parent version, mutation operators, ...).
    pub source_refs: Vec<String>,
}

/// `true` if `v` is `MAJOR.MINOR.PATCH` with optional pre-release and build parts.
pub fn is_semver(v: &str) -> bool {
    let (core, rest) = match v.find(['-', '+']) {
        Some(i) => (&v[..i], &v[i..]),
        None => (v, ""),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let numeric = |p: &str| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && (p == "0" || !p.starts_with('0'))
    };
    if parts.len() != 3 || !parts.iter().all(|p| numeric(p)) {
        return false;
    }
    let ident = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    };
    let (pre, build) = match rest.find('+') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let pre_ok = pre.is_empty() || (pre.starts_with('-') && ident(&pre[1..]));
    let build_ok = (rest.is_empty() || !rest.contains('+')) || ident(build);
    pre_ok && build_ok
}

impl Manifest {
    /// Check field bounds. Called before a manifest enters a registry.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let text = |s: &str, max: usize| !s.trim().is_empty() && s.len() <= max;
        if !text(&self.agent_id, 128) {
            return Err(ManifestError::BadField("agent_id"));
        }
        if !is_semver(&self.version) {
            return Err(ManifestError::BadVersion(self.version.clone()));
        }
        if !text(&self.created_by, 256) {
            return Err(ManifestError::BadField("created_by"));
        }
        if !text(&self.created_at, 64) {
            return Err(ManifestError::BadField("created_at"));
        }
        if !(1..=100).contains(&self.max_steps) {
            return Err(ManifestError::OutOfRange("max_steps"));
        }
        if !(1..=3600).contains(&self.timeout_seconds) {
            return Err(ManifestError::OutOfRange("timeout_seconds"));
        }
        if self.model_parameters.values().any(|v| !v.is_finite()) {
            return Err(ManifestError::OutOfRange("model_parameters"));
        }
        Ok(())
    }

    /// Deterministic text form: fixed field order, sorted collections, escaped strings.
    /// Equal manifests always produce equal text.
    pub fn canonical(&self) -> String {
        fn esc(s: &str) -> String {
            let mut o = String::with_capacity(s.len() + 2);
            o.push('"');
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                    c => o.push(c),
                }
            }
            o.push('"');
            o
        }
        let list = |it: Vec<String>| format!("[{}]", it.join(","));
        let params = self
            .model_parameters
            .iter()
            .map(|(k, v)| format!("{}:{:?}", esc(k), v))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{agent_id:{},version:{},max_steps:{},timeout_seconds:{},model_parameters:{{{}}},permissions:{},tools:{},provenance:{{origin:{},created_at:{},created_by:{},source_refs:{}}}}}",
            esc(&self.agent_id),
            esc(&self.version),
            self.max_steps,
            self.timeout_seconds,
            params,
            list(self.permissions.iter().map(|s| esc(s)).collect()),
            list(self.tools.iter().map(|s| esc(s)).collect()),
            esc(self.origin.as_str()),
            esc(&self.created_at),
            esc(&self.created_by),
            list(self.source_refs.iter().map(|s| esc(s)).collect()),
        )
    }

    /// SHA-256 of the canonical form, lowercase hex.
    pub fn content_hash(&self) -> String {
        let digest = Sha256::digest(self.canonical().as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Everything except version and provenance: what "the same agent behaviour" means.
    pub fn behaviour_eq(&self, other: &Manifest) -> bool {
        self.agent_id == other.agent_id
            && self.max_steps == other.max_steps
            && self.timeout_seconds == other.timeout_seconds
            && self.model_parameters == other.model_parameters
            && self.permissions == other.permissions
            && self.tools == other.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Manifest {
        Manifest {
            agent_id: "support-agent".into(),
            version: "1.0.0".into(),
            max_steps: 10,
            timeout_seconds: 60,
            model_parameters: BTreeMap::from([("temperature".to_string(), 0.7)]),
            permissions: BTreeSet::from(["read:tickets".to_string()]),
            tools: BTreeSet::from(["search".to_string()]),
            origin: Origin::Authored,
            created_at: "2026-11-23T00:00:00Z".into(),
            created_by: "tester".into(),
            source_refs: vec![],
        }
    }

    #[test]
    fn semver_accepts_and_rejects() {
        for ok in [
            "0.0.0",
            "1.2.3",
            "10.20.30-rc.1",
            "1.0.0+build.5",
            "1.0.0-a+b",
        ] {
            assert!(is_semver(ok), "{ok}");
        }
        for bad in [
            "1.2", "01.2.3", "1.2.3.4", "v1.2.3", "1.2.x", "", "1.0.0-", "1.0.0+",
        ] {
            assert!(!is_semver(bad), "{bad}");
        }
    }

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        let a = sample();
        assert_eq!(a.content_hash(), sample().content_hash());
        assert_eq!(a.content_hash().len(), 64);
        let mut b = sample();
        b.max_steps = 11;
        assert_ne!(a.content_hash(), b.content_hash());
        let mut c = sample();
        c.permissions.insert("write:tickets".into());
        assert_ne!(a.content_hash(), c.content_hash());
    }

    #[test]
    fn collection_order_does_not_change_hash() {
        let mut a = sample();
        a.tools = BTreeSet::from(["b".to_string(), "a".to_string()]);
        let mut b = sample();
        b.tools = BTreeSet::from(["a".to_string(), "b".to_string()]);
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn validation_bounds() {
        assert!(sample().validate().is_ok());
        let mut m = sample();
        m.max_steps = 0;
        assert_eq!(m.validate(), Err(ManifestError::OutOfRange("max_steps")));
        let mut m = sample();
        m.timeout_seconds = 3601;
        assert_eq!(
            m.validate(),
            Err(ManifestError::OutOfRange("timeout_seconds"))
        );
        let mut m = sample();
        m.version = "x".into();
        assert!(matches!(m.validate(), Err(ManifestError::BadVersion(_))));
        let mut m = sample();
        m.model_parameters.insert("top_p".into(), f64::NAN);
        assert!(m.validate().is_err());
    }

    #[test]
    fn strings_are_escaped_in_canonical_form() {
        let mut m = sample();
        m.created_by = "a\"b\\c\n".into();
        assert!(m.canonical().contains("a\\\"b\\\\c\\n"));
    }
}
