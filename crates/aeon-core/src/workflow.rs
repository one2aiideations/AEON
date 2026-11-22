//! Workflows as task DAGs.
//!
//! A planner emits tasks with dependencies; the runtime asks for ready
//! tasks, reports outcomes, and failures cancel everything downstream.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

/// Task id inside a workflow.
pub type StepId = u32;

/// Step state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepState {
    /// Waiting for dependencies.
    Blocked,
    /// Dependencies met; can run.
    Ready,
    /// Handed to the runtime.
    Running,
    /// Finished with output.
    Done(String),
    /// Failed with a reason.
    Failed(String),
    /// Skipped because a dependency failed.
    Cancelled,
}

/// Workflow errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowError {
    /// Step id already used.
    Duplicate(StepId),
    /// Dependency names an unknown step.
    MissingDep(StepId),
    /// Dependencies form a cycle.
    Cycle,
    /// Step not in the expected state.
    State(StepId),
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(s) => write!(f, "duplicate step {s}"),
            Self::MissingDep(s) => write!(f, "unknown dependency {s}"),
            Self::Cycle => f.write_str("dependency cycle"),
            Self::State(s) => write!(f, "step {s} is not in the right state"),
        }
    }
}

impl std::error::Error for FlowError {}

#[derive(Debug, Clone)]
struct Node {
    action: String,
    deps: BTreeSet<StepId>,
    state: StepState,
}

/// A validated DAG of steps.
#[derive(Debug, Clone)]
pub struct Workflow {
    steps: BTreeMap<StepId, Node>,
}

impl Workflow {
    /// Build from `(id, action, deps)`; rejects duplicates, unknown deps and cycles.
    pub fn new(spec: &[(StepId, &str, &[StepId])]) -> Result<Self, FlowError> {
        let mut steps = BTreeMap::new();
        for &(id, action, deps) in spec {
            let node = Node {
                action: action.to_owned(),
                deps: deps.iter().copied().collect(),
                state: StepState::Blocked,
            };
            if steps.insert(id, node).is_some() {
                return Err(FlowError::Duplicate(id));
            }
        }
        for n in steps.values() {
            if let Some(&d) = n.deps.iter().find(|d| !steps.contains_key(d)) {
                return Err(FlowError::MissingDep(d));
            }
        }
        let mut wf = Self { steps };
        wf.topo_order()?;
        wf.refresh();
        Ok(wf)
    }

    /// Steps in dependency order (Kahn's algorithm).
    pub fn topo_order(&self) -> Result<Vec<StepId>, FlowError> {
        let mut indeg: BTreeMap<StepId, usize> = self
            .steps
            .iter()
            .map(|(&id, n)| (id, n.deps.len()))
            .collect();
        let mut ready: Vec<StepId> = indeg
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut order = Vec::with_capacity(self.steps.len());
        while let Some(id) = ready.pop() {
            order.push(id);
            for (&other, n) in &self.steps {
                if n.deps.contains(&id) {
                    let d = indeg.get_mut(&other).ok_or(FlowError::Cycle)?;
                    *d -= 1;
                    if *d == 0 {
                        ready.push(other);
                    }
                }
            }
        }
        if order.len() == self.steps.len() {
            Ok(order)
        } else {
            Err(FlowError::Cycle)
        }
    }

    fn refresh(&mut self) {
        loop {
            let mut changed = false;
            let snapshot: BTreeMap<StepId, StepState> = self
                .steps
                .iter()
                .map(|(&id, n)| (id, n.state.clone()))
                .collect();
            for n in self.steps.values_mut() {
                if n.state != StepState::Blocked {
                    continue;
                }
                let dep_states: Vec<&StepState> = n.deps.iter().map(|d| &snapshot[d]).collect();
                if dep_states
                    .iter()
                    .any(|s| matches!(s, StepState::Failed(_) | StepState::Cancelled))
                {
                    n.state = StepState::Cancelled;
                    changed = true;
                } else if dep_states.iter().all(|s| matches!(s, StepState::Done(_))) {
                    n.state = StepState::Ready;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Take all ready steps (marks them running).
    pub fn take_ready(&mut self) -> Vec<(StepId, String)> {
        let mut out = Vec::new();
        for (&id, n) in &mut self.steps {
            if n.state == StepState::Ready {
                n.state = StepState::Running;
                out.push((id, n.action.clone()));
            }
        }
        out
    }

    /// Report a running step's outcome.
    pub fn finish(&mut self, id: StepId, result: Result<String, String>) -> Result<(), FlowError> {
        let n = self.steps.get_mut(&id).ok_or(FlowError::State(id))?;
        if n.state != StepState::Running {
            return Err(FlowError::State(id));
        }
        n.state = match result {
            Ok(o) => StepState::Done(o),
            Err(e) => StepState::Failed(e),
        };
        self.refresh();
        Ok(())
    }

    /// No step is ready, running or blocked.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.steps.values().all(|n| {
            matches!(
                n.state,
                StepState::Done(_) | StepState::Failed(_) | StepState::Cancelled
            )
        })
    }

    /// State of a step.
    #[must_use]
    pub fn state(&self, id: StepId) -> Option<&StepState> {
        self.steps.get(&id).map(|n| &n.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_in_dependency_order() {
        let mut wf = Workflow::new(&[
            (1, "fetch mail", &[]),
            (2, "fetch calendar", &[]),
            (3, "summarise", &[1, 2]),
            (4, "notify", &[3]),
        ])
        .unwrap();
        let first: Vec<StepId> = wf.take_ready().into_iter().map(|s| s.0).collect();
        assert_eq!(first, vec![1, 2]);
        wf.finish(1, Ok("12 mails".into())).unwrap();
        assert!(wf.take_ready().is_empty());
        wf.finish(2, Ok("3 events".into())).unwrap();
        assert_eq!(wf.take_ready(), vec![(3, "summarise".to_string())]);
        wf.finish(3, Ok("digest".into())).unwrap();
        assert_eq!(wf.take_ready()[0].0, 4);
        wf.finish(4, Ok(String::new())).unwrap();
        assert!(wf.is_finished());
    }

    #[test]
    fn failure_cancels_downstream() {
        let mut wf =
            Workflow::new(&[(1, "a", &[]), (2, "b", &[1]), (3, "c", &[2]), (4, "d", &[])]).unwrap();
        wf.take_ready();
        wf.finish(1, Err("offline".into())).unwrap();
        assert_eq!(wf.state(3), Some(&StepState::Cancelled));
        assert!(!wf.is_finished());
        wf.finish(4, Ok(String::new())).unwrap();
        assert!(wf.is_finished());
        assert_eq!(wf.finish(4, Ok(String::new())), Err(FlowError::State(4)));
    }

    #[test]
    fn rejects_bad_graphs() {
        assert_eq!(
            Workflow::new(&[(1, "a", &[2]), (2, "b", &[1])]).unwrap_err(),
            FlowError::Cycle
        );
        assert_eq!(
            Workflow::new(&[(1, "a", &[9])]).unwrap_err(),
            FlowError::MissingDep(9)
        );
        assert_eq!(
            Workflow::new(&[(1, "a", &[]), (1, "b", &[])]).unwrap_err(),
            FlowError::Duplicate(1)
        );
    }
}
