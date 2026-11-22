//! Agent lifecycle: observe, plan, act, reflect.
//!
//! [`run_episode`] drives an [`Agent`] for at most `max_steps` actions and
//! stops when the plan is finished, a step fails too often, or the agent
//! decides it is done.

/// One planned step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Capability to call.
    pub capability: String,
    /// Arguments.
    pub args: String,
}

/// Result of one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Step succeeded with output.
    Ok(String),
    /// Step failed with a reason.
    Failed(String),
}

/// What the agent wants after reflecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Continue with the remaining plan.
    Continue,
    /// Replan from the current observation.
    Replan,
    /// Goal reached.
    Done,
}

/// An agent.
pub trait Agent {
    /// Read the current situation into the agent's context.
    fn observe(&mut self) -> String;
    /// Produce a plan for `goal` given the observation.
    fn plan(&mut self, goal: &str, observation: &str) -> Vec<Step>;
    /// Execute one step.
    fn act(&mut self, step: &Step) -> Outcome;
    /// Learn from an outcome and decide what to do next.
    fn reflect(&mut self, step: &Step, outcome: &Outcome) -> Next;
}

/// How an episode ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeEnd {
    /// Agent said done.
    Done,
    /// Plan ran out without a done signal.
    PlanExhausted,
    /// Step budget used up.
    Budget,
    /// Too many failed steps.
    TooManyFailures,
}

/// Episode result: how it ended and the trace of steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    /// End reason.
    pub end: EpisodeEnd,
    /// Steps and their outcomes, in order.
    pub trace: Vec<(Step, Outcome)>,
}

/// Run one goal to completion within budgets.
pub fn run_episode(agent: &mut dyn Agent, goal: &str, max_steps: usize, max_failures: usize) -> Episode {
    let mut trace = Vec::new();
    let mut failures = 0;
    let obs = agent.observe();
    let mut plan: std::collections::VecDeque<Step> = agent.plan(goal, &obs).into();
    while trace.len() < max_steps {
        let Some(step) = plan.pop_front() else {
            return Episode {
                end: EpisodeEnd::PlanExhausted,
                trace,
            };
        };
        let outcome = agent.act(&step);
        if matches!(outcome, Outcome::Failed(_)) {
            failures += 1;
        }
        let next = agent.reflect(&step, &outcome);
        trace.push((step, outcome));
        if failures >= max_failures.max(1) {
            return Episode {
                end: EpisodeEnd::TooManyFailures,
                trace,
            };
        }
        match next {
            Next::Done => {
                return Episode {
                    end: EpisodeEnd::Done,
                    trace,
                }
            }
            Next::Replan => {
                let obs = agent.observe();
                plan = agent.plan(goal, &obs).into();
            }
            Next::Continue => {}
        }
    }
    Episode {
        end: EpisodeEnd::Budget,
        trace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts up to a target; fails once at 2 and replans.
    struct Counter {
        value: u32,
        target: u32,
        failed_once: bool,
    }

    impl Agent for Counter {
        fn observe(&mut self) -> String {
            self.value.to_string()
        }
        fn plan(&mut self, _goal: &str, obs: &str) -> Vec<Step> {
            let v: u32 = obs.parse().unwrap_or(0);
            (v..self.target)
                .map(|_| Step {
                    capability: "inc".into(),
                    args: String::new(),
                })
                .collect()
        }
        fn act(&mut self, _step: &Step) -> Outcome {
            if self.value == 2 && !self.failed_once {
                self.failed_once = true;
                return Outcome::Failed("transient".into());
            }
            self.value += 1;
            Outcome::Ok(self.value.to_string())
        }
        fn reflect(&mut self, _step: &Step, outcome: &Outcome) -> Next {
            match outcome {
                Outcome::Failed(_) => Next::Replan,
                Outcome::Ok(_) if self.value == self.target => Next::Done,
                Outcome::Ok(_) => Next::Continue,
            }
        }
    }

    #[test]
    fn replans_after_failure_and_finishes() {
        let mut a = Counter { value: 0, target: 4, failed_once: false };
        let ep = run_episode(&mut a, "reach 4", 10, 3);
        assert_eq!(ep.end, EpisodeEnd::Done);
        assert_eq!(a.value, 4);
        assert_eq!(ep.trace.len(), 5);
    }

    #[test]
    fn budgets_apply() {
        let mut a = Counter { value: 0, target: 100, failed_once: true };
        assert_eq!(run_episode(&mut a, "far", 3, 3).end, EpisodeEnd::Budget);
        let mut b = Counter { value: 2, target: 5, failed_once: false };
        assert_eq!(run_episode(&mut b, "x", 10, 1).end, EpisodeEnd::TooManyFailures);
        let mut c = Counter { value: 5, target: 5, failed_once: true };
        assert_eq!(run_episode(&mut c, "x", 10, 1).end, EpisodeEnd::PlanExhausted);
    }
}
