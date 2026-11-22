//! Assistant call to an OpenAI-compatible chat endpoint.
//!
//! Local endpoints (localhost / 127.0.0.1 / ::1) may be called freely.
//! Remote endpoints require explicit consent, because project data leaves
//! the machine.

use crate::ProjectError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Provider settings.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AiConfig {
    /// Full chat-completions URL, e.g. `http://127.0.0.1:11434/v1/chat/completions`.
    pub endpoint: String,
    /// Bearer key (empty for local servers).
    pub api_key: String,
    /// Model name.
    pub model: String,
}

/// Host part of an http(s) URL, lowercased, without port or brackets.
fn host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether `endpoint` points at this machine.
#[must_use]
pub fn is_local(endpoint: &str) -> bool {
    matches!(host(endpoint).as_deref(), Some("localhost" | "127.0.0.1" | "::1"))
}

/// Check the call may go out: endpoint set and valid, consent for remote hosts.
pub fn check(config: &AiConfig, remote_consent: bool) -> Result<(), ProjectError> {
    if config.endpoint.trim().is_empty() {
        return Err(ProjectError::Ai("endpoint not configured".into()));
    }
    if host(&config.endpoint).is_none() {
        return Err(ProjectError::Ai("endpoint must be an http(s) URL".into()));
    }
    if !is_local(&config.endpoint) && !remote_consent {
        return Err(ProjectError::Ai(
            "remote endpoint: project data would leave this machine; consent required".into(),
        ));
    }
    Ok(())
}

/// Ask the assistant. Returns the reply text.
pub async fn ask(config: &AiConfig, prompt: &str, remote_consent: bool) -> Result<String, ProjectError> {
    check(config, remote_consent)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| ProjectError::Ai(e.to_string()))?;
    let mut req = client.post(&config.endpoint).json(&serde_json::json!({
        "model": config.model,
        "messages": [{"role": "user", "content": prompt}]
    }));
    if !config.api_key.is_empty() {
        req = req.bearer_auth(&config.api_key);
    }
    let resp = req.send().await.map_err(|e| ProjectError::Ai(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(ProjectError::Ai(format!("provider returned {}", resp.status())));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| ProjectError::Ai(e.to_string()))?;
    body["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ProjectError::Ai("malformed response".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(endpoint: &str) -> AiConfig {
        AiConfig {
            endpoint: endpoint.into(),
            api_key: String::new(),
            model: "m".into(),
        }
    }

    #[test]
    fn locality() {
        assert!(is_local("http://127.0.0.1:11434/v1/chat/completions"));
        assert!(is_local("http://LOCALHOST/x"));
        assert!(is_local("http://[::1]:8080/x"));
        assert!(!is_local("https://api.example.com/v1"));
        assert!(!is_local("http://localhost.evil.com/x"));
        assert!(!is_local("http://127.0.0.1@evil.com/x"));
    }

    #[test]
    fn consent_gate() {
        assert!(check(&cfg(""), true).is_err());
        assert!(check(&cfg("ftp://x"), true).is_err());
        assert!(check(&cfg("http://127.0.0.1:1/v1"), false).is_ok());
        assert!(check(&cfg("https://api.example.com/v1"), false).is_err());
        assert!(check(&cfg("https://api.example.com/v1"), true).is_ok());
    }
}
