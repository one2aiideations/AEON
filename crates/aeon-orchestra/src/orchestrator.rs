//! Orchestrating one objective.
//!
//! A planner decomposes the objective into steps (capability + action +
//! dependencies). Each ready step is checked against the constitution,
//! budget and quota, assigned to the most trusted capable agent, and run
//! through a circuit breaker with retries. Outcomes update agent trust.

use crate::governance::{Budget, Constitution, Quota, Spend, Verdict};
use crate::registry::{AgentId, Registry};
use crate::resilience::CircuitBreaker;
use std::collections::BTreeMap;

/// One planned step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    /// Step id.
    pub id: u32,
    /// Capability needed ("web.read", "code.write").
    pub capability: String,
    /// Governed action name ("net.get", "files.write").
    pub action: String,
    /// Instruction for the agent.
    pub instruction: String,
    /// Steps that must finish first.
    pub deps: Vec<u32>,
    /// Estimated cost charged to the budget.
    pub cost: u32,
}

/// Decomposes objectives.
pub trait Planner {
    /// Steps for `objective`.
    fn decompose(&mut self, objective: &str) -> Vec<PlanStep>;
}

/// Runs a step on an agent.
pub trait Worker {
    /// Execute `step` on `agent`; returns output or error.
    fn run(&mut self, agent: AgentId, step: &PlanStep) -> Result<String, String>;
}

/// Final state of a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Finished.
    Done {
        /// Agent that did it.
        agent: AgentId,
        /// Output.
        output: String,
    },
    /// Failed after retries.
    Failed(String),
    /// Blocked by governance (deny, missing approval, budget, quota).
    Blocked(String),
    /// Skipped because a dependency did not finish.
    Skipped,
    /// No capable agent.
    Unassigned,
}

/// Orchestrator state.
#[derive(Debug)]
pub struct Orchestrator {
    /// Agents.
    pub registry: Registry,
    /// Rules.
    pub constitution: Constitution,
    /// Spending budget (cost units per period).
    pub budget: Budget,
    /// Per-agent call quota.
    pub quota: Quota,
    breakers: BTreeMap<String, CircuitBreaker>,
    /// Attempts per step.
    pub max_attempts: u32,
}

impl Orchestrator {
    /// New orchestrator.
    #[must_use]
    pub fn new(registry: Registry, constitution: Constitution, budget: Budget, quota: Quota) -> Self {
        Self {
            registry,
            constitution,
            budget,
            quota,
            breakers: BTreeMap::new(),
            max_attempts: 2,
        }
    }

    fn govern(&mut self, step: &PlanStep, approved: &dyn Fn(&PlanStep) -> bool, period: u64) -> Option<String> {
        let (verdict, reason) = self.constitution.judge(&step.action);
        let reason = reason.to_owned();
        match verdict {
            Verdict::Deny => return Some(format!("denied: {reason}")),
            Verdict::RequireApproval if !approved(step) => return Some(format!("needs approval: {reason}")),
            _ => {}
        }
        if self.budget.spend(period, f64::from(step.cost)) == Spend::Refused {
            return Some("budget exhausted".into());
        }
        None
    }

    /// Run `objective` to completion at time `now` (ms) in budget `period`.
    /// `approved` answers whether the user approved a step needing approval.
    pub fn run(
        &mut self,
        objective: &str,
        planner: &mut dyn Planner,
        worker: &mut dyn Worker,
        approved: &dyn Fn(&PlanStep) -> bool,
        now: u64,
        period: u64,
    ) -> BTreeMap<u32, StepOutcome> {
        let steps = planner.decompose(objective);
        let mut out: BTreeMap<u32, StepOutcome> = BTreeMap::new();
        // Process in rounds: each round runs every step whose deps are resolved.
        loop {
            let pending: Vec<&PlanStep> = steps.iter().filter(|s| !out.contains_key(&s.id)).collect();
            if pending.is_empty() {
                break;
            }
            let mut progressed = false;
            for step in pending {
                let unresolved = step.deps.iter().any(|d| !out.contains_key(d));
                let all_exist = step.deps.iter().all(|d| steps.iter().any(|s| s.id == *d));
                if unresolved && all_exist {
                    continue;
                }
                progressed = true;
                let deps_done = step
                    .deps
                    .iter()
                    .all(|d| matches!(out.get(d), Some(StepOutcome::Done { .. })));
                if !deps_done {
                    out.insert(step.id, StepOutcome::Skipped);
                    continue;
                }
                if let Some(why) = self.govern(step, approved, period) {
                    out.insert(step.id, StepOutcome::Blocked(why));
                    continue;
                }
                out.insert(step.id, self.execute(step, worker, now));
            }
            if !progressed {
                // Cycle: skip the rest.
                for s in steps.iter().filter(|s| !out.contains_key(&s.id)) {
                    out.insert(s.id, StepOutcome::Skipped);
                }
                break;
            }
        }
        out
    }

    fn execute(&mut self, step: &PlanStep, worker: &mut dyn Worker, now: u64) -> StepOutcome {
        let mut last_err = String::from("no attempt made");
        let mut tried: Vec<AgentId> = Vec::new();
        for attempt in 0..self.max_attempts {
            let t = now + u64::from(attempt);
            let breaker = self
                .breakers
                .entry(step.capability.clone())
                .or_insert_with(|| CircuitBreaker::new(3, 30_000));
            if !breaker.allow(t) {
                return StepOutcome::Failed(format!("circuit open for {}", step.capability));
            }
            // Prefer an agent not tried yet for this step.
            let agent = self
                .registry
                .with_capability(&step.capability)
                .filter(|a| a.enabled && a.load < a.max_load && !tried.contains(&a.id))
                .max_by(|a, b| a.trust.total_cmp(&b.trust).then(b.id.cmp(&a.id)))
                .map(|a| a.id)
                .or_else(|| self.registry.assign(&step.capability));
            let Some(agent) = agent else {
                return if tried.is_empty() {
                    StepOutcome::Unassigned
                } else {
                    StepOutcome::Failed(last_err)
                };
            };
            if !self.quota.allow(&format!("agent:{agent}"), t) {
                return StepOutcome::Blocked(format!("quota reached for agent {agent}"));
            }
            tried.push(agent);
            self.registry.start(agent);
            let r = worker.run(agent, step);
            let ok = r.is_ok();
            self.registry.finish(agent, ok);
            if let Some(b) = self.breakers.get_mut(&step.capability) {
                b.record(t, ok);
            }
            match r {
                Ok(output) => return StepOutcome::Done { agent, output },
                Err(e) => last_err = e,
            }
        }
        StepOutcome::Failed(last_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<PlanStep>);
    impl Planner for Fixed {
        fn decompose(&mut self, _objective: &str) -> Vec<PlanStep> {
            self.0.clone()
        }
    }

    fn step(id: u32, cap: &str, action: &str, deps: &[u32]) -> PlanStep {
        PlanStep {
            id,
            capability: cap.into(),
            action: action.into(),
            instruction: format!("step {id}"),
            deps: deps.to_vec(),
            cost: 1,
        }
    }

    fn orch() -> (Orchestrator, AgentId, AgentId) {
        let mut reg = Registry::new();
        let researcher = reg.register("researcher", &["web.read"], 2);
        let writer = reg.register("writer", &["doc.write"], 2);
        let mut c = Constitution::new();
        c.rule("net.get", Verdict::Allow, "reading the web is fine")
            .rule("files.write", Verdict::RequireApproval, "writes need the user")
            .rule("payments.*", Verdict::Deny, "never");
        (Orchestrator::new(reg, c, Budget::new(10.0, true), Quota::new(100, 60_000)), researcher, writer)
    }

    #[test]
    fn runs_plan_with_governance() {
        let (mut o, researcher, writer) = orch();
        let mut plan = Fixed(vec![
            step(1, "web.read", "net.get", &[]),
            step(2, "doc.write", "files.write", &[1]),
            step(3, "doc.write", "payments.send", &[]),
            step(4, "video.edit", "net.get", &[]),
        ]);
        let mut worker = |agent: AgentId, s: &PlanStep| -> Result<String, String> { Ok(format!("{agent}:{}", s.id)) };
        struct W<F>(F);
        impl<F: FnMut(AgentId, &PlanStep) -> Result<String, String>> Worker for W<F> {
            fn run(&mut self, agent: AgentId, step: &PlanStep) -> Result<String, String> {
                (self.0)(agent, step)
            }
        }
        let res = o.run("brief me", &mut plan, &mut W(&mut worker), &|_| true, 0, 1);
        assert_eq!(
            res[&1],
            StepOutcome::Done {
                agent: researcher,
                output: format!("{researcher}:1")
            }
        );
        assert!(matches!(res[&2], StepOutcome::Done { agent, .. } if agent == writer));
        assert!(matches!(&res[&3], StepOutcome::Blocked(w) if w.starts_with("denied")));
        assert_eq!(res[&4], StepOutcome::Unassigned);

        let res = o.run("again", &mut plan, &mut W(&mut worker), &|_| false, 0, 1);
        assert!(matches!(&res[&2], StepOutcome::Blocked(w) if w.starts_with("needs approval")));
    }

    #[test]
    fn retries_on_other_agent_and_skips_dependents() {
        let (mut o, _r, _w) = orch();
        let backup = o.registry.register("researcher-2", &["web.read"], 1);
        let mut plan = Fixed(vec![step(1, "web.read", "net.get", &[]), step(2, "doc.write", "net.get", &[1])]);
        struct FailFirst(Vec<AgentId>);
        impl Worker for FailFirst {
            fn run(&mut self, agent: AgentId, _s: &PlanStep) -> Result<String, String> {
                self.0.push(agent);
                if self.0.len() == 1 {
                    Err("timeout".into())
                } else {
                    Ok("found".into())
                }
            }
        }
        let mut w = FailFirst(Vec::new());
        let res = o.run("x", &mut plan, &mut w, &|_| true, 0, 1);
        assert!(matches!(res[&1], StepOutcome::Done { agent, .. } if agent == backup));
        assert!(matches!(res[&2], StepOutcome::Done { .. }));

        struct AlwaysFail;
        impl Worker for AlwaysFail {
            fn run(&mut self, _a: AgentId, _s: &PlanStep) -> Result<String, String> {
                Err("down".into())
            }
        }
        let res = o.run("y", &mut plan, &mut AlwaysFail, &|_| true, 100, 2);
        assert_eq!(res[&1], StepOutcome::Failed("down".into()));
        assert_eq!(res[&2], StepOutcome::Skipped);
    }
}
