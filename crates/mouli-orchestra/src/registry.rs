//! Agent registry with learned trust.

use std::collections::{BTreeMap, BTreeSet};

/// Agent id.
pub type AgentId = u32;

/// A registered agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentInfo {
    /// Id.
    pub id: AgentId,
    /// Name ("planner", "coder", "researcher", ...).
    pub name: String,
    /// Capabilities it offers ("plan", "code.write", "web.read", ...).
    pub capabilities: BTreeSet<String>,
    /// Trust in [0, 1], an exponential average of outcomes.
    pub trust: f32,
    /// Tasks in flight.
    pub load: u32,
    /// Maximum concurrent tasks.
    pub max_load: u32,
    /// Disabled agents receive no work.
    pub enabled: bool,
    /// Completed tasks.
    pub successes: u64,
    /// Failed tasks.
    pub failures: u64,
}

/// Registry of agents.
#[derive(Debug, Clone)]
pub struct Registry {
    agents: BTreeMap<AgentId, AgentInfo>,
    next: AgentId,
    /// Weight of the newest outcome in the trust average.
    pub learning_rate: f32,
    /// Agents whose trust falls below this are disabled automatically.
    pub min_trust: f32,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            agents: BTreeMap::new(),
            next: 1,
            learning_rate: 0.2,
            min_trust: 0.2,
        }
    }
}

impl Registry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent with neutral trust 0.5.
    pub fn register(&mut self, name: &str, capabilities: &[&str], max_load: u32) -> AgentId {
        let id = self.next;
        self.next += 1;
        self.agents.insert(
            id,
            AgentInfo {
                id,
                name: name.to_owned(),
                capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
                trust: 0.5,
                load: 0,
                max_load: max_load.max(1),
                enabled: true,
                successes: 0,
                failures: 0,
            },
        );
        id
    }

    /// Look up.
    #[must_use]
    pub fn get(&self, id: AgentId) -> Option<&AgentInfo> {
        self.agents.get(&id)
    }

    /// Enable or disable.
    pub fn set_enabled(&mut self, id: AgentId, enabled: bool) -> bool {
        self.agents
            .get_mut(&id)
            .map(|a| a.enabled = enabled)
            .is_some()
    }

    /// Best available agent for `capability`: highest trust, then lowest
    /// relative load, then lowest id.
    #[must_use]
    pub fn assign(&self, capability: &str) -> Option<AgentId> {
        self.agents
            .values()
            .filter(|a| a.enabled && a.load < a.max_load && a.capabilities.contains(capability))
            .max_by(|a, b| {
                a.trust
                    .total_cmp(&b.trust)
                    .then(
                        (u64::from(b.load) * u64::from(a.max_load))
                            .cmp(&(u64::from(a.load) * u64::from(b.max_load))),
                    )
                    .then(b.id.cmp(&a.id))
            })
            .map(|a| a.id)
    }

    /// Mark a task started on `id`.
    pub fn start(&mut self, id: AgentId) {
        if let Some(a) = self.agents.get_mut(&id) {
            a.load = a.load.saturating_add(1);
        }
    }

    /// Record a finished task; updates trust and may disable the agent.
    pub fn finish(&mut self, id: AgentId, success: bool) {
        let (lr, min) = (self.learning_rate, self.min_trust);
        if let Some(a) = self.agents.get_mut(&id) {
            a.load = a.load.saturating_sub(1);
            let target = if success { 1.0 } else { 0.0 };
            a.trust += lr * (target - a.trust);
            if success {
                a.successes += 1;
            } else {
                a.failures += 1;
            }
            if a.trust < min {
                a.enabled = false;
            }
        }
    }

    /// Agents offering `capability`.
    pub fn with_capability<'a>(
        &'a self,
        capability: &'a str,
    ) -> impl Iterator<Item = &'a AgentInfo> + 'a {
        self.agents
            .values()
            .filter(move |a| a.capabilities.contains(capability))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_drives_assignment() {
        let mut r = Registry::new();
        let a = r.register("coder-a", &["code.write"], 2);
        let b = r.register("coder-b", &["code.write", "code.review"], 2);
        assert_eq!(r.assign("code.write"), Some(a), "tie -> lower id");
        r.start(b);
        r.finish(b, true);
        assert_eq!(r.assign("code.write"), Some(b));
        assert_eq!(r.assign("code.review"), Some(b));
        assert_eq!(r.assign("web.read"), None);
        r.start(b);
        r.start(b);
        assert_eq!(r.assign("code.write"), Some(a), "b is full");
    }

    #[test]
    fn repeated_failure_disables() {
        let mut r = Registry::new();
        let a = r.register("flaky", &["x"], 1);
        for _ in 0..5 {
            r.start(a);
            r.finish(a, false);
        }
        let info = r.get(a).unwrap();
        assert!(!info.enabled);
        assert_eq!(info.failures, 5);
        assert_eq!(r.assign("x"), None);
        assert_eq!(r.with_capability("x").count(), 1);
    }
}
