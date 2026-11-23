//! Workflow states, resource limits and retry backoff.

use crate::error::{Error, Result};

/// Lifecycle of a unit of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Created, not started.
    Pending,
    /// Running.
    Running,
    /// Waiting for a person (approval) or an event.
    Waiting,
    /// Finished successfully.
    Succeeded,
    /// Finished with an error.
    Failed,
    /// Stopped on request.
    Cancelled,
}

impl State {
    /// No further transitions are possible.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    fn allowed(self, to: Self) -> bool {
        use State::{Cancelled, Failed, Pending, Running, Succeeded, Waiting};
        matches!(
            (self, to),
            (Pending, Running | Cancelled)
                | (Running, Waiting | Succeeded | Failed | Cancelled)
                | (Waiting, Running | Failed | Cancelled)
        )
    }
}

/// A named piece of work with a checked state machine.
#[derive(Debug, Clone)]
pub struct Workflow {
    /// Name.
    pub name: String,
    state: State,
}

impl Workflow {
    /// New pending workflow.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            state: State::Pending,
        }
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// Move to `to`.
    ///
    /// # Errors
    /// [`Error::State`] when the transition is not allowed.
    pub fn transition(&mut self, to: State) -> Result<()> {
        if !self.state.allowed(to) {
            return Err(Error::State(format!(
                "{:?} -> {:?} is not allowed",
                self.state, to
            )));
        }
        self.state = to;
        Ok(())
    }
}

/// Resource limits for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted input in bytes.
    pub max_input_bytes: usize,
    /// Largest accepted output in bytes.
    pub max_output_bytes: usize,
    /// Most steps in one plan.
    pub max_steps: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1 << 20,
            max_output_bytes: 1 << 20,
            max_steps: 100,
        }
    }
}

impl Limits {
    /// Validate that all limits are positive.
    ///
    /// # Errors
    /// [`Error::Invalid`] when any limit is zero.
    pub fn validate(&self) -> Result<()> {
        if self.max_input_bytes == 0 || self.max_output_bytes == 0 || self.max_steps == 0 {
            return Err(Error::Invalid("limits must be positive".into()));
        }
        Ok(())
    }

    /// Check an input size.
    ///
    /// # Errors
    /// [`Error::Limit`] when too large.
    pub fn check_input(&self, len: usize) -> Result<()> {
        if len > self.max_input_bytes {
            return Err(Error::Limit(format!(
                "input of {len} bytes exceeds {}",
                self.max_input_bytes
            )));
        }
        Ok(())
    }

    /// Check an output size.
    ///
    /// # Errors
    /// [`Error::Limit`] when too large.
    pub fn check_output(&self, len: usize) -> Result<()> {
        if len > self.max_output_bytes {
            return Err(Error::Limit(format!(
                "output of {len} bytes exceeds {}",
                self.max_output_bytes
            )));
        }
        Ok(())
    }

    /// Check a plan length.
    ///
    /// # Errors
    /// [`Error::Limit`] when too many steps.
    pub fn check_steps(&self, steps: usize) -> Result<()> {
        if steps > self.max_steps {
            return Err(Error::Limit(format!(
                "{steps} steps exceed {}",
                self.max_steps
            )));
        }
        Ok(())
    }
}

/// Delay in milliseconds before retry number `attempt` (from zero): `base * 2^attempt`, capped.
#[must_use]
pub fn backoff_ms(attempt: u32, base_ms: u64, cap_ms: u64) -> u64 {
    base_ms
        .saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX))
        .min(cap_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_and_illegal_transitions() {
        let mut w = Workflow::new("report");
        assert!(w.transition(State::Succeeded).is_err());
        w.transition(State::Running).unwrap();
        w.transition(State::Waiting).unwrap();
        w.transition(State::Running).unwrap();
        w.transition(State::Succeeded).unwrap();
        assert!(w.state().is_terminal());
        for to in [State::Running, State::Failed, State::Pending] {
            assert!(w.transition(to).is_err());
        }
    }

    #[test]
    fn limits() {
        let l = Limits {
            max_input_bytes: 10,
            max_output_bytes: 5,
            max_steps: 2,
        };
        assert!(l.check_input(10).is_ok() && l.check_input(11).is_err());
        assert!(
            l.check_output(6).is_err() && l.check_steps(3).is_err() && l.check_steps(2).is_ok()
        );
        assert!(Limits { max_steps: 0, ..l }.validate().is_err());
        assert!(Limits::default().validate().is_ok());
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_ms(0, 250, 10_000), 250);
        assert_eq!(backoff_ms(3, 250, 10_000), 2000);
        assert_eq!(backoff_ms(10, 250, 10_000), 10_000);
        assert_eq!(backoff_ms(200, 250, 10_000), 10_000);
    }
}
