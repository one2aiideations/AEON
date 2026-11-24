//! Evidence: canonical JSON hashing, integrity checks, bundles, and retention that respects
//! legal holds. Hashes prove a record is unchanged; they are not signatures and do not prove who
//! created it.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

/// SHA-256 of `text` as lower-case hex.
pub fn sha256_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// JSON with object keys sorted and no extra whitespace, so equal data always hashes equally
/// (independent of key order or how the JSON was formatted).
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        Value::String(k.clone()),
                        m.get(k).map_or("null".to_string(), canonical_json)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

/// Why evidence could not be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceError {
    /// A required text field is empty.
    EmptyField(&'static str),
    /// A bundle must contain at least one item.
    EmptyBundle,
    /// The same evidence id was listed twice.
    DuplicateId(String),
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvidenceError::EmptyField(n) => write!(f, "{n} must not be empty"),
            EvidenceError::EmptyBundle => write!(f, "a bundle needs at least one evidence item"),
            EvidenceError::DuplicateId(i) => write!(f, "evidence '{i}' is listed twice"),
        }
    }
}

impl std::error::Error for EvidenceError {}

/// Result of an integrity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    /// The content still matches its recorded hash.
    Valid,
    /// The content no longer matches.
    Mismatch {
        /// Hash recorded when the evidence was collected.
        stored: String,
        /// Hash of the content as it is now.
        computed: String,
    },
}

/// A piece of evidence with a fingerprint of its content.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// Evidence identifier.
    pub id: String,
    /// Kind, for example `log_extract` or `screenshot`.
    pub kind: String,
    /// The entity it concerns, if any.
    pub entity: Option<String>,
    /// When it was collected (epoch seconds).
    pub collected_at: i64,
    content: Value,
    content_hash: String,
}

impl Evidence {
    /// Records new evidence, hashing its canonical content.
    pub fn new(
        id: &str,
        kind: &str,
        content: Value,
        entity: Option<&str>,
        collected_at: i64,
    ) -> Result<Evidence, EvidenceError> {
        if id.trim().is_empty() {
            return Err(EvidenceError::EmptyField("id"));
        }
        if kind.trim().is_empty() {
            return Err(EvidenceError::EmptyField("kind"));
        }
        let content_hash = sha256_hex(&canonical_json(&content));
        Ok(Evidence {
            id: id.to_string(),
            kind: kind.to_string(),
            entity: entity.map(str::to_string),
            collected_at,
            content,
            content_hash,
        })
    }

    /// Rebuilds evidence loaded from storage, keeping the stored hash so [`Evidence::verify`]
    /// can notice if the stored content was altered.
    pub fn restore(
        id: &str,
        kind: &str,
        content: Value,
        stored_hash: &str,
        entity: Option<&str>,
        collected_at: i64,
    ) -> Evidence {
        Evidence {
            id: id.to_string(),
            kind: kind.to_string(),
            entity: entity.map(str::to_string),
            collected_at,
            content,
            content_hash: stored_hash.to_string(),
        }
    }

    /// The evidence content.
    pub fn content(&self) -> &Value {
        &self.content
    }

    /// The recorded content hash.
    pub fn stored_hash(&self) -> &str {
        &self.content_hash
    }

    /// Recomputes the content hash and compares it with the recorded one.
    pub fn verify(&self) -> Integrity {
        let computed = sha256_hex(&canonical_json(&self.content));
        if computed == self.content_hash {
            Integrity::Valid
        } else {
            Integrity::Mismatch {
                stored: self.content_hash.clone(),
                computed,
            }
        }
    }
}

/// A problem found when re-checking a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleProblem {
    /// The bundle's own recorded hash no longer matches its contents list.
    BundleAltered,
    /// A listed item was not supplied.
    Missing(String),
    /// A listed item's content hash differs from the one recorded in the bundle.
    Changed(String),
    /// A listed item's content no longer matches its own recorded hash.
    ContentAltered(String),
}

/// A named, hash-sealed set of evidence for a purpose such as an investigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// Bundle name.
    pub name: String,
    /// What the bundle is for.
    pub purpose: String,
    items: Vec<(String, String)>,
    hash: String,
}

fn bundle_hash(name: &str, purpose: &str, items: &[(String, String)]) -> String {
    let list: Vec<Value> = items
        .iter()
        .map(|(id, h)| serde_json::json!({"id": id, "hash": h}))
        .collect();
    sha256_hex(&canonical_json(
        &serde_json::json!({"name": name, "purpose": purpose, "items": list}),
    ))
}

impl Bundle {
    /// Seals `evidence` (any order) into a bundle. Ids must be unique and the list non-empty.
    pub fn new(name: &str, purpose: &str, evidence: &[&Evidence]) -> Result<Bundle, EvidenceError> {
        if name.trim().is_empty() {
            return Err(EvidenceError::EmptyField("name"));
        }
        if purpose.trim().is_empty() {
            return Err(EvidenceError::EmptyField("purpose"));
        }
        if evidence.is_empty() {
            return Err(EvidenceError::EmptyBundle);
        }
        let mut items: Vec<(String, String)> = evidence
            .iter()
            .map(|e| (e.id.clone(), e.content_hash.clone()))
            .collect();
        items.sort();
        if let Some(w) = items.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(EvidenceError::DuplicateId(w[0].0.clone()));
        }
        let hash = bundle_hash(name, purpose, &items);
        Ok(Bundle {
            name: name.to_string(),
            purpose: purpose.to_string(),
            items,
            hash,
        })
    }

    /// The bundle's seal.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Ids of the sealed items, sorted.
    pub fn ids(&self) -> Vec<&str> {
        self.items.iter().map(|(i, _)| i.as_str()).collect()
    }

    /// Re-checks the bundle against `current` evidence. An empty result means everything is
    /// intact. Extra evidence in `current` that the bundle does not list is ignored.
    pub fn verify(&self, current: &[&Evidence]) -> Vec<BundleProblem> {
        let mut out = Vec::new();
        if bundle_hash(&self.name, &self.purpose, &self.items) != self.hash {
            out.push(BundleProblem::BundleAltered);
        }
        for (id, recorded) in &self.items {
            match current.iter().find(|e| &e.id == id) {
                None => out.push(BundleProblem::Missing(id.clone())),
                Some(e) => {
                    if &e.content_hash != recorded {
                        out.push(BundleProblem::Changed(id.clone()));
                    } else if e.verify() != Integrity::Valid {
                        out.push(BundleProblem::ContentAltered(id.clone()));
                    }
                }
            }
        }
        out
    }
}

/// What a legal hold covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldScope {
    /// Everything.
    All,
    /// Every item about this entity.
    Entity(String),
    /// This one item.
    Evidence(String),
}

/// A legal hold that stops evidence from expiring while it is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegalHold {
    /// Hold identifier.
    pub id: String,
    /// What it covers.
    pub scope: HoldScope,
    /// Whether it is in force.
    pub active: bool,
}

impl LegalHold {
    fn covers(&self, e: &Evidence) -> bool {
        self.active
            && match &self.scope {
                HoldScope::All => true,
                HoldScope::Entity(x) => e.entity.as_deref() == Some(x.as_str()),
                HoldScope::Evidence(id) => &e.id == id,
            }
    }
}

/// The retention rule for an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Keep indefinitely.
    Keep,
    /// Move to archive.
    Archive,
    /// A person must review it.
    Review,
    /// May expire this many days after collection.
    ExpireAfterDays(u32),
}

/// The retention verdict. It is advice only: nothing here deletes anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Protected by this legal hold.
    Held(String),
    /// Keep.
    Keep,
    /// Archive.
    Archive,
    /// Needs human review.
    Review,
    /// Past its retention period and unprotected.
    Expired,
}

/// Decides what retention allows for `e` at `now`. An active hold that covers the item always
/// wins, whatever the policy.
pub fn evaluate_retention(e: &Evidence, policy: Policy, holds: &[LegalHold], now: i64) -> Decision {
    if let Some(h) = holds.iter().find(|h| h.covers(e)) {
        return Decision::Held(h.id.clone());
    }
    match policy {
        Policy::Keep => Decision::Keep,
        Policy::Archive => Decision::Archive,
        Policy::Review => Decision::Review,
        Policy::ExpireAfterDays(d) => {
            let due = e
                .collected_at
                .saturating_add(i64::from(d).saturating_mul(86_400));
            if now >= due {
                Decision::Expired
            } else {
                Decision::Keep
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(id: &str, content: Value) -> Evidence {
        Evidence::new(id, "log_extract", content, Some("vendor:7"), 1_000).unwrap()
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hex(&[0, 15, 16, 255]), "000f10ff");
    }

    #[test]
    fn canonical_json_ignores_key_order_and_spacing() {
        let a: Value = serde_json::from_str(r#"{"b": 1, "a": {"y": [1, 2], "x": null}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":null,"y":[1,2]},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), r#"{"a":{"x":null,"y":[1,2]},"b":1}"#);
        assert_eq!(canonical_json(&a), canonical_json(&b));
        // Array order matters.
        assert_ne!(
            canonical_json(&json!([1, 2])),
            canonical_json(&json!([2, 1]))
        );
        assert_eq!(canonical_json(&json!("a\"b")), r#""a\"b""#);
    }

    #[test]
    fn same_content_same_hash_different_content_different_hash() {
        let a = ev("E1", json!({"user": "bob", "n": 1}));
        let b = ev("E2", json!({"n": 1, "user": "bob"}));
        let c = ev("E3", json!({"user": "bob", "n": 2}));
        assert_eq!(a.stored_hash(), b.stored_hash());
        assert_ne!(a.stored_hash(), c.stored_hash());
        assert_eq!(a.verify(), Integrity::Valid);
    }

    #[test]
    fn tampering_with_stored_content_is_detected() {
        let a = ev("E1", json!({"amount": 100}));
        let loaded = Evidence::restore(
            "E1",
            "log_extract",
            json!({"amount": 900}),
            a.stored_hash(),
            Some("vendor:7"),
            1_000,
        );
        match loaded.verify() {
            Integrity::Mismatch { stored, computed } => {
                assert_eq!(stored, a.stored_hash());
                assert_ne!(computed, stored);
            }
            Integrity::Valid => panic!("tampering not detected"),
        }
        let untouched = Evidence::restore(
            "E1",
            "log_extract",
            json!({"amount": 100}),
            a.stored_hash(),
            None,
            1_000,
        );
        assert_eq!(untouched.verify(), Integrity::Valid);
    }

    #[test]
    fn evidence_needs_id_and_kind() {
        assert_eq!(
            Evidence::new(" ", "k", json!(1), None, 0),
            Err(EvidenceError::EmptyField("id"))
        );
        assert_eq!(
            Evidence::new("i", "", json!(1), None, 0),
            Err(EvidenceError::EmptyField("kind"))
        );
        assert!(Evidence::new("i", "k", Value::Null, None, 0).is_ok());
    }

    #[test]
    fn bundle_hash_is_independent_of_input_order() {
        let (a, b) = (ev("E1", json!(1)), ev("E2", json!(2)));
        let x = Bundle::new("case-9", "fraud review", &[&a, &b]).unwrap();
        let y = Bundle::new("case-9", "fraud review", &[&b, &a]).unwrap();
        assert_eq!(x.hash(), y.hash());
        assert_eq!(x.ids(), vec!["E1", "E2"]);
        assert_ne!(
            x.hash(),
            Bundle::new("case-9", "other purpose", &[&a, &b])
                .unwrap()
                .hash()
        );
        assert_ne!(
            x.hash(),
            Bundle::new("case-9", "fraud review", &[&a]).unwrap().hash()
        );
    }

    #[test]
    fn bundle_validation() {
        let a = ev("E1", json!(1));
        assert_eq!(Bundle::new("n", "p", &[]), Err(EvidenceError::EmptyBundle));
        assert_eq!(
            Bundle::new("", "p", &[&a]),
            Err(EvidenceError::EmptyField("name"))
        );
        assert_eq!(
            Bundle::new("n", " ", &[&a]),
            Err(EvidenceError::EmptyField("purpose"))
        );
        let dup = ev("E1", json!(2));
        assert_eq!(
            Bundle::new("n", "p", &[&a, &dup]),
            Err(EvidenceError::DuplicateId("E1".into()))
        );
    }

    #[test]
    fn bundle_verify_finds_each_kind_of_problem() {
        let (a, b) = (ev("E1", json!(1)), ev("E2", json!(2)));
        let bundle = Bundle::new("n", "p", &[&a, &b]).unwrap();
        assert!(bundle.verify(&[&a, &b]).is_empty());
        let extra = ev("E9", json!(9));
        assert!(bundle.verify(&[&b, &extra, &a]).is_empty());
        assert_eq!(
            bundle.verify(&[&a]),
            vec![BundleProblem::Missing("E2".into())]
        );
        let replaced = ev("E2", json!(3));
        assert_eq!(
            bundle.verify(&[&a, &replaced]),
            vec![BundleProblem::Changed("E2".into())]
        );
        let forged = Evidence::restore("E2", "log_extract", json!(3), b.stored_hash(), None, 1_000);
        assert_eq!(
            bundle.verify(&[&a, &forged]),
            vec![BundleProblem::ContentAltered("E2".into())]
        );
        let mut altered = bundle.clone();
        altered.purpose = "something else".into();
        assert_eq!(
            altered.verify(&[&a, &b]),
            vec![BundleProblem::BundleAltered]
        );
        assert_eq!(bundle.verify(&[]).len(), 2);
    }

    fn hold(id: &str, scope: HoldScope, active: bool) -> LegalHold {
        LegalHold {
            id: id.into(),
            scope,
            active,
        }
    }

    #[test]
    fn holds_respect_their_scope() {
        let e = ev("E1", json!(1));
        let day = 86_400;
        let past = 1_000 + 400 * day;
        let p = Policy::ExpireAfterDays(365);
        assert_eq!(evaluate_retention(&e, p, &[], past), Decision::Expired);
        assert_eq!(
            evaluate_retention(&e, p, &[hold("H1", HoldScope::All, true)], past),
            Decision::Held("H1".into())
        );
        assert_eq!(
            evaluate_retention(
                &e,
                p,
                &[hold("H2", HoldScope::Entity("vendor:7".into()), true)],
                past
            ),
            Decision::Held("H2".into())
        );
        assert_eq!(
            evaluate_retention(
                &e,
                p,
                &[hold("H3", HoldScope::Evidence("E1".into()), true)],
                past
            ),
            Decision::Held("H3".into())
        );
        // A hold for something else does NOT protect this item (the prototype protected everything).
        assert_eq!(
            evaluate_retention(
                &e,
                p,
                &[hold("H4", HoldScope::Entity("vendor:8".into()), true)],
                past
            ),
            Decision::Expired
        );
        assert_eq!(
            evaluate_retention(
                &e,
                p,
                &[hold("H5", HoldScope::Evidence("E2".into()), true)],
                past
            ),
            Decision::Expired
        );
        // A released hold protects nothing.
        assert_eq!(
            evaluate_retention(&e, p, &[hold("H6", HoldScope::All, false)], past),
            Decision::Expired
        );
        // The first covering hold is named.
        let holds = [
            hold("A", HoldScope::Entity("x".into()), true),
            hold("B", HoldScope::All, true),
            hold("C", HoldScope::All, true),
        ];
        assert_eq!(
            evaluate_retention(&e, p, &holds, past),
            Decision::Held("B".into())
        );
    }

    #[test]
    fn expiry_boundary_and_other_policies() {
        let e = ev("E1", json!(1));
        let due = 1_000 + 30 * 86_400;
        let p = Policy::ExpireAfterDays(30);
        assert_eq!(evaluate_retention(&e, p, &[], due - 1), Decision::Keep);
        assert_eq!(evaluate_retention(&e, p, &[], due), Decision::Expired);
        assert_eq!(
            evaluate_retention(&e, Policy::Keep, &[], i64::MAX),
            Decision::Keep
        );
        assert_eq!(
            evaluate_retention(&e, Policy::Archive, &[], 0),
            Decision::Archive
        );
        assert_eq!(
            evaluate_retention(&e, Policy::Review, &[], 0),
            Decision::Review
        );
        // Held beats every policy, including Archive.
        assert!(matches!(
            evaluate_retention(&e, Policy::Archive, &[hold("H", HoldScope::All, true)], 0),
            Decision::Held(_)
        ));
        // Overflow-safe.
        let late = Evidence::new("L", "k", json!(1), None, i64::MAX).unwrap();
        assert_eq!(
            evaluate_retention(&late, Policy::ExpireAfterDays(u32::MAX), &[], i64::MAX),
            Decision::Expired
        );
    }
}
