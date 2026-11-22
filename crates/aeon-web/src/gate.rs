//! Execution gate for page intents.
//!
//! Default is a dry run (fill and preview, never submit). Executing a
//! sensitive intent needs a consent bound to that exact intent and payload.
//! Secret fields are never filled by the agent; the user types them.

use crate::intents::Intent;
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

/// How to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Fill and preview only.
    DryRun,
    /// Submit.
    Execute,
}

/// User consent for one intent + payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Consent(u64);

/// Gate decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Preview with these values (secrets left blank).
    Preview(BTreeMap<String, String>),
    /// Submit with these values.
    Submit(BTreeMap<String, String>),
    /// Ask the user; carries the text to show.
    NeedsConsent(String),
    /// Refused.
    Refused(String),
}

fn fingerprint(intent: &Intent, payload: &BTreeMap<String, String>) -> u64 {
    let mut h = DefaultHasher::new();
    intent.id.hash(&mut h);
    intent.action.hash(&mut h);
    intent.method.hash(&mut h);
    payload.hash(&mut h);
    h.finish()
}

/// Consent token for showing the user exactly this intent and payload.
#[must_use]
pub fn consent_for(intent: &Intent, payload: &BTreeMap<String, String>) -> Consent {
    Consent(fingerprint(intent, payload))
}

/// Decide what may happen with `payload` for `intent`.
#[must_use]
pub fn decide(intent: &Intent, payload: &BTreeMap<String, String>, mode: Mode, consent: Option<Consent>) -> Decision {
    if let Some(name) = payload.keys().find(|k| !intent.fields.iter().any(|f| &f.name == *k)) {
        return Decision::Refused(format!("unknown field {name}"));
    }
    if let Some(f) = intent
        .fields
        .iter()
        .find(|f| f.secret() && payload.get(&f.name).is_some_and(|v| !v.is_empty()))
    {
        return Decision::Refused(format!("agent may not fill secret field {}", f.name));
    }
    // Keep hidden defaults (e.g. anti-forgery tokens), overlay the payload.
    let mut values: BTreeMap<String, String> = intent
        .fields
        .iter()
        .filter(|f| f.kind == "hidden")
        .map(|f| (f.name.clone(), f.value.clone()))
        .collect();
    values.extend(payload.iter().map(|(k, v)| (k.clone(), v.clone())));
    match mode {
        Mode::DryRun => Decision::Preview(values),
        Mode::Execute if !intent.needs_consent => Decision::Submit(values),
        Mode::Execute => match consent {
            Some(c) if c == consent_for(intent, payload) => Decision::Submit(values),
            Some(_) => Decision::Refused("consent was given for a different action".into()),
            None => Decision::NeedsConsent(format!(
                "{} {} ({:?}) with {} field(s)",
                intent.method,
                intent.action,
                intent.kind,
                payload.len()
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intents::detect;

    fn page() -> Vec<Intent> {
        detect(
            r#"<form action="/s"><input name="q"></form>
               <form action="/login" method="post"><input type="hidden" name="_token" value="t1">
               <input name="user"><input name="pass" type="password"></form>"#,
            "https://x.example/",
        )
    }

    fn p(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn search_runs_login_needs_consent() {
        let v = page();
        let q = p(&[("q", "aeon")]);
        assert_eq!(decide(&v[0], &q, Mode::Execute, None), Decision::Submit(q.clone()));

        let u = p(&[("user", "alice")]);
        assert!(matches!(decide(&v[1], &u, Mode::Execute, None), Decision::NeedsConsent(_)));
        let c = consent_for(&v[1], &u);
        match decide(&v[1], &u, Mode::Execute, Some(c)) {
            Decision::Submit(vals) => assert_eq!(vals.get("_token").map(String::as_str), Some("t1")),
            other => panic!("unexpected {other:?}"),
        }
        let other = p(&[("user", "mallory")]);
        assert!(matches!(decide(&v[1], &other, Mode::Execute, Some(c)), Decision::Refused(_)));
    }

    #[test]
    fn secrets_and_unknown_fields_refused() {
        let v = page();
        assert!(matches!(
            decide(&v[1], &p(&[("pass", "hunter2")]), Mode::DryRun, None),
            Decision::Refused(_)
        ));
        assert!(matches!(
            decide(&v[0], &p(&[("evil", "1")]), Mode::DryRun, None),
            Decision::Refused(_)
        ));
        assert!(matches!(decide(&v[1], &p(&[]), Mode::DryRun, None), Decision::Preview(_)));
    }
}
