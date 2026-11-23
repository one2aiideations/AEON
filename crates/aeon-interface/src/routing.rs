//! Model routing: choose among local and hosted models by capability, privacy, context size
//! and cost. The choice and its reason are returned so the interface can show them.

use crate::error::{Error, Result};
use std::collections::BTreeSet;

type Filter<'a> = Box<dyn Fn(&ModelEntry) -> bool + 'a>;

/// A model the workspace can use.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    /// Model id.
    pub id: String,
    /// Provider name (`local` for the on-device engine).
    pub provider: String,
    /// Runs on this machine.
    pub local: bool,
    /// Capabilities: `text`, `vision`, `audio`, `code`, ...
    pub caps: BTreeSet<String>,
    /// Context window in tokens.
    pub context: u32,
    /// Cost per 1000 tokens in micro-units; zero for local models.
    pub cost_per_1k: u32,
    /// Quality score 0 to 100 (for AEON, the robust score S).
    pub quality: u32,
}

impl ModelEntry {
    /// Build an entry.
    #[must_use]
    pub fn new(
        id: &str,
        provider: &str,
        local: bool,
        caps: &[&str],
        context: u32,
        cost_per_1k: u32,
        quality: u32,
    ) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            local,
            caps: caps.iter().map(|c| (*c).to_string()).collect(),
            context,
            cost_per_1k,
            quality,
        }
    }
}

/// What the request needs.
#[derive(Debug, Clone, Default)]
pub struct Needs {
    /// Required capabilities.
    pub caps: BTreeSet<String>,
    /// Must run locally (for example because the text is sensitive).
    pub local_only: bool,
    /// Largest acceptable cost per 1000 tokens, if any.
    pub max_cost_per_1k: Option<u32>,
    /// Smallest acceptable context window.
    pub min_context: u32,
}

/// The decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Chosen model id.
    pub model: String,
    /// Its provider.
    pub provider: String,
    /// Whether it is local.
    pub local: bool,
    /// Why it won.
    pub reason: String,
}

/// Pick the best model. Highest quality wins; ties go to the cheaper, then the lower id.
/// External models are skipped when `allow_external` is false or the request is local-only.
///
/// # Errors
/// [`Error::NotFound`] naming the first requirement nobody met.
pub fn choose(models: &[ModelEntry], needs: &Needs, allow_external: bool) -> Result<Route> {
    let local_only = needs.local_only || !allow_external;
    let mut pool: Vec<&ModelEntry> = models.iter().collect();
    let stages: [(&str, Filter<'_>); 4] = [
        (
            "no model has the required capabilities",
            Box::new(|m| needs.caps.is_subset(&m.caps)),
        ),
        (
            "no model has a large enough context window",
            Box::new(|m| m.context >= needs.min_context),
        ),
        (
            if needs.local_only {
                "no local model (request is local-only)"
            } else {
                "no local model and external models are not allowed"
            },
            Box::new(move |m| m.local || !local_only),
        ),
        (
            "no model within the cost limit",
            Box::new(|m| needs.max_cost_per_1k.is_none_or(|c| m.cost_per_1k <= c)),
        ),
    ];
    for (why, keep) in &stages {
        pool.retain(|m| keep(m));
        if pool.is_empty() {
            return Err(Error::NotFound((*why).to_string()));
        }
    }
    pool.sort_by(|a, b| {
        b.quality
            .cmp(&a.quality)
            .then(a.cost_per_1k.cmp(&b.cost_per_1k))
            .then(a.id.cmp(&b.id))
    });
    let m = pool[0];
    Ok(Route {
        model: m.id.clone(),
        provider: m.provider.clone(),
        local: m.local,
        reason: format!(
            "best quality ({}) among {} eligible; {}",
            m.quality,
            pool.len(),
            if m.local { "runs locally" } else { "hosted" }
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fleet() -> Vec<ModelEntry> {
        vec![
            ModelEntry::new("tiny", "local", true, &["text"], 2048, 0, 40),
            ModelEntry::new("coder", "local", true, &["text", "code"], 8192, 0, 60),
            ModelEntry::new(
                "big",
                "cloud",
                false,
                &["text", "code", "vision"],
                128_000,
                500,
                95,
            ),
            ModelEntry::new(
                "big2",
                "cloud",
                false,
                &["text", "code", "vision"],
                128_000,
                300,
                95,
            ),
        ]
    }

    fn needs(caps: &[&str]) -> Needs {
        Needs {
            caps: caps.iter().map(|c| (*c).to_string()).collect(),
            ..Needs::default()
        }
    }

    #[test]
    fn local_first_when_external_not_allowed() {
        let r = choose(&fleet(), &needs(&["text"]), false).unwrap();
        assert_eq!(r.model, "coder");
        assert!(r.local);
    }

    #[test]
    fn external_allowed_prefers_quality_then_cost() {
        let r = choose(&fleet(), &needs(&["text"]), true).unwrap();
        assert_eq!(r.model, "big2");
        assert!(!r.local);
    }

    #[test]
    fn local_only_overrides_permission() {
        let mut n = needs(&["text"]);
        n.local_only = true;
        assert!(choose(&fleet(), &n, true).unwrap().local);
    }

    #[test]
    fn explains_why_nothing_fits() {
        let e = choose(&fleet(), &needs(&["audio"]), true).unwrap_err();
        assert!(e.to_string().contains("capabilities"));
        let e = choose(&fleet(), &needs(&["vision"]), false).unwrap_err();
        assert!(e.to_string().contains("local"));
        let mut n = needs(&["text"]);
        n.min_context = 1_000_000;
        assert!(choose(&fleet(), &n, true)
            .unwrap_err()
            .to_string()
            .contains("context"));
        let mut n = needs(&["vision"]);
        n.max_cost_per_1k = Some(100);
        assert!(choose(&fleet(), &n, true)
            .unwrap_err()
            .to_string()
            .contains("cost"));
        assert!(choose(&[], &needs(&[]), true).is_err());
    }
}
