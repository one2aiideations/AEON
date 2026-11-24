//! Model selection by context, with fallbacks.

use std::collections::BTreeMap;

/// How fast an answer is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Interactive.
    High,
    /// Background.
    Normal,
}

/// Where the work runs and under which rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// On this device; cost (battery, heat) matters.
    Local,
    /// Remote service allowed.
    Remote,
    /// Sensitive data: only low-risk models.
    Regulated,
}

/// Compute available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// CPU only.
    Cpu,
    /// GPU available.
    Gpu,
}

/// Selection context.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    /// Urgency.
    pub urgency: Urgency,
    /// Environment.
    pub environment: Environment,
    /// Device.
    pub device: Device,
    /// Input language code (for `*.AUTO` capabilities).
    pub language: String,
    /// Target language code (for translation).
    pub target_language: String,
}

/// A model offering one capability.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    /// Name.
    pub name: String,
    /// Measured accuracy in [0, 1] (from `mouli-theory` robust score where available).
    pub accuracy: f32,
    /// Typical latency in milliseconds.
    pub latency_ms: u32,
    /// Relative running cost.
    pub cost: f32,
    /// Data-handling risk is low (safe for regulated data).
    pub low_risk: bool,
    /// Needs a GPU.
    pub gpu_required: bool,
}

/// Score a model for a context; `None` means it may not be used at all.
#[must_use]
pub fn score(m: &ModelEntry, ctx: &Context) -> Option<f32> {
    if m.gpu_required && ctx.device == Device::Cpu {
        return None;
    }
    if ctx.environment == Environment::Regulated && !m.low_risk {
        return None;
    }
    let latency_scale = match ctx.urgency {
        Urgency::High => 500.0,
        Urgency::Normal => 1500.0,
    };
    let mut s = m.accuracy * 5.0 - m.latency_ms as f32 / latency_scale;
    if ctx.environment == Environment::Local {
        s -= m.cost;
    }
    Some(s)
}

/// Resolve `ocr.AUTO`, `asr.AUTO`, `tts.AUTO`, `translate.AUTO` from the context.
#[must_use]
pub fn resolve_capability(capability: &str, ctx: &Context) -> Option<String> {
    let Some(base) = capability.strip_suffix(".AUTO") else {
        return Some(capability.to_owned());
    };
    if ctx.language.is_empty() {
        return None;
    }
    match base {
        "ocr" | "asr" | "tts" => Some(format!("{base}.{}", ctx.language)),
        "translate" if !ctx.target_language.is_empty() => Some(format!(
            "translate.{}_to_{}",
            ctx.language, ctx.target_language
        )),
        _ => None,
    }
}

/// Models per capability.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    models: BTreeMap<String, Vec<ModelEntry>>,
}

impl ModelCatalog {
    /// Empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a model for `capability`.
    pub fn add(&mut self, capability: &str, m: ModelEntry) {
        self.models
            .entry(capability.to_owned())
            .or_default()
            .push(m);
    }

    /// Usable models for `capability`, best first: the first is the choice,
    /// the rest are the fallback chain.
    #[must_use]
    pub fn ranked(&self, capability: &str, ctx: &Context) -> Vec<&ModelEntry> {
        let Some(cap) = resolve_capability(capability, ctx) else {
            return Vec::new();
        };
        let mut v: Vec<(f32, &ModelEntry)> = self
            .models
            .get(&cap)
            .into_iter()
            .flatten()
            .filter_map(|m| score(m, ctx).map(|s| (s, m)))
            .collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        v.into_iter().map(|(_, m)| m).collect()
    }

    /// Run `call` down the fallback chain until one succeeds. Returns the
    /// model used and its output, or every error.
    pub fn run_with_fallback<T>(
        &self,
        capability: &str,
        ctx: &Context,
        mut call: impl FnMut(&ModelEntry) -> Result<T, String>,
    ) -> Result<(String, T), Vec<String>> {
        let mut errors = Vec::new();
        for m in self.ranked(capability, ctx) {
            match call(m) {
                Ok(v) => return Ok((m.name.clone(), v)),
                Err(e) => errors.push(format!("{}: {e}", m.name)),
            }
        }
        if errors.is_empty() {
            errors.push(format!("no usable model for {capability}"));
        }
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(
        name: &str,
        accuracy: f32,
        latency_ms: u32,
        cost: f32,
        low_risk: bool,
        gpu: bool,
    ) -> ModelEntry {
        ModelEntry {
            name: name.into(),
            accuracy,
            latency_ms,
            cost,
            low_risk,
            gpu_required: gpu,
        }
    }

    fn ctx(urgency: Urgency, environment: Environment, device: Device) -> Context {
        Context {
            urgency,
            environment,
            device,
            language: "ta".into(),
            target_language: "en".into(),
        }
    }

    fn catalog() -> ModelCatalog {
        let mut c = ModelCatalog::new();
        c.add("chat", m("big-gpu", 0.9, 800, 2.0, true, true));
        c.add("chat", m("small-cpu", 0.7, 200, 0.2, true, false));
        c.add("chat", m("cloud", 0.99, 1200, 0.0, false, false));
        c.add("asr.ta", m("whisper-ta", 0.8, 300, 0.5, true, false));
        c
    }

    #[test]
    fn context_changes_choice() {
        let c = catalog();
        let cpu_regulated = c.ranked(
            "chat",
            &ctx(Urgency::High, Environment::Regulated, Device::Cpu),
        );
        assert_eq!(cpu_regulated.len(), 1);
        assert_eq!(cpu_regulated[0].name, "small-cpu");
        let remote = c.ranked(
            "chat",
            &ctx(Urgency::Normal, Environment::Remote, Device::Gpu),
        );
        assert_eq!(remote[0].name, "cloud");
        assert_eq!(
            c.ranked(
                "asr.AUTO",
                &ctx(Urgency::High, Environment::Local, Device::Cpu)
            )[0]
            .name,
            "whisper-ta"
        );
    }

    #[test]
    fn fallback_chain() {
        let c = catalog();
        let x = ctx(Urgency::Normal, Environment::Remote, Device::Gpu);
        let (used, out) = c
            .run_with_fallback("chat", &x, |m| {
                if m.name == "cloud" {
                    Err("offline".into())
                } else {
                    Ok(1)
                }
            })
            .unwrap();
        assert_ne!(used, "cloud");
        assert_eq!(out, 1);
        let errs = c
            .run_with_fallback::<()>("chat", &x, |_| Err("down".into()))
            .unwrap_err();
        assert_eq!(errs.len(), 3);
        assert!(c.run_with_fallback::<()>("nope", &x, |_| Ok(())).is_err());
    }

    #[test]
    fn auto_resolution() {
        let x = ctx(Urgency::High, Environment::Local, Device::Cpu);
        assert_eq!(
            resolve_capability("translate.AUTO", &x).unwrap(),
            "translate.ta_to_en"
        );
        assert_eq!(resolve_capability("chat", &x).unwrap(), "chat");
        assert!(resolve_capability("video.AUTO", &x).is_none());
    }
}
