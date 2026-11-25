//! MOULI agent runtime.
//!
//! One [`Runtime::tick`] runs the phases
//! sensors -> cognition -> model -> action -> telemetry.
//! Tools are invoked through a capability-checked [`tools::ToolBus`] and every
//! actuator command passes the [`action::ActionGate`].
//!
//! Ported from the retired C core (`aeon_runtime`, `aeon_system_embodiment`).
#![forbid(unsafe_code)]

pub mod action;
pub mod sensors;
pub mod tools;

use action::{ActionGate, Command};
use core::fmt;
use mouli_cognition::inference::{select_action, InferenceState};
use mouli_cognition::memory::EpisodicMemory;
use mouli_cognition::workspace::Workspace;
use sensors::SensorFrame;
use tools::{ToolBus, ToolError};

/// Runtime phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Between ticks.
    Idle,
    /// Reading sensors.
    Sensors,
    /// Updating beliefs.
    Cognition,
    /// Model step.
    Model,
    /// Tool call.
    Tools,
    /// Actuator command.
    Action,
    /// Recording.
    Telemetry,
}

impl Phase {
    const ALL: [Self; 7] = [
        Self::Idle,
        Self::Sensors,
        Self::Cognition,
        Self::Model,
        Self::Tools,
        Self::Action,
        Self::Telemetry,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }
}

/// Runtime errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    /// Bad configuration.
    Config,
    /// A cognition step rejected its input.
    Cognition,
    /// A kernel failed.
    Model,
    /// Tool bus error.
    Tool(ToolError),
    /// Command not finite.
    BadCommand,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config => f.write_str("bad runtime configuration"),
            Self::Cognition => f.write_str("cognition step failed"),
            Self::Model => f.write_str("model step failed"),
            Self::Tool(e) => write!(f, "tool error: {e}"),
            Self::BadCommand => f.write_str("non-finite command"),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Runtime configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Number of discrete actions to choose between (1..=1024).
    pub actions: usize,
    /// RMS-norm epsilon for the model step.
    pub epsilon: f32,
    /// Actuator limits.
    pub gate: ActionGate,
    /// Episodic memory size.
    pub memory: usize,
    /// Workspace size.
    pub workspace: usize,
}

/// The agent runtime.
#[derive(Debug)]
pub struct Runtime {
    cfg: Config,
    phase: Phase,
    ticks: u64,
    phase_count: [u64; 7],
    /// Latest sensor samples.
    pub sensors: SensorFrame,
    /// Registered tools.
    pub tools: ToolBus,
    gate: ActionGate,
    memory: EpisodicMemory,
    workspace: Workspace,
    inference: InferenceState,
    model_output: [f32; 4],
    selected: usize,
    last_command: Command,
}

impl Runtime {
    /// Build a runtime.
    pub fn new(cfg: Config) -> Result<Self, RuntimeError> {
        if cfg.actions == 0
            || cfg.actions > 1024
            || !cfg.epsilon.is_finite()
            || cfg.epsilon <= 0.0
            || !cfg.gate.is_valid()
        {
            return Err(RuntimeError::Config);
        }
        Ok(Self {
            memory: EpisodicMemory::new(cfg.memory).map_err(|_| RuntimeError::Config)?,
            workspace: Workspace::new(cfg.workspace).map_err(|_| RuntimeError::Config)?,
            gate: cfg.gate,
            cfg,
            phase: Phase::Idle,
            ticks: 0,
            phase_count: [0; 7],
            sensors: SensorFrame::new(16),
            tools: ToolBus::new(16),
            inference: InferenceState::default(),
            model_output: [0.0; 4],
            selected: 0,
            last_command: Command::default(),
        })
    }

    fn enter(&mut self, p: Phase) {
        self.phase = p;
        self.phase_count[p.index()] += 1;
    }

    /// Run one full cycle at `timestamp`.
    pub fn tick(&mut self, timestamp: u64) -> Result<(), RuntimeError> {
        self.enter(Phase::Sensors);
        if self.sensors.is_empty() {
            self.sensors
                .push(0, timestamp, 0.0)
                .map_err(|_| RuntimeError::Cognition)?;
        }
        let observation = self.sensors.latest().map_or(0.0, |s| s.value);

        self.enter(Phase::Cognition);
        let (action, state) = select_action(observation.abs(), 1.0, self.cfg.actions)
            .map_err(|_| RuntimeError::Cognition)?;
        self.selected = action;
        self.inference = state;
        let id = self.ticks + 1;
        self.workspace
            .publish(id, state.confidence)
            .map_err(|_| RuntimeError::Cognition)?;
        self.memory
            .record(id, observation, 1.0)
            .map_err(|_| RuntimeError::Cognition)?;

        self.enter(Phase::Model);
        let x = [observation, 1.0, 0.0, -observation];
        mouli_llm::kernels::ops::rmsnorm(&x, &[], self.cfg.epsilon, &mut self.model_output)
            .map_err(|_| RuntimeError::Model)?;

        self.enter(Phase::Action);
        self.last_command = self.gate.check(action as f32);

        self.enter(Phase::Telemetry);
        self.ticks += 1;
        self.phase = Phase::Idle;
        Ok(())
    }

    /// Invoke a tool with the caller's capability bits.
    pub fn invoke_tool(
        &mut self,
        id: u32,
        caps: u32,
        input: &[u8],
    ) -> Result<Vec<u8>, RuntimeError> {
        self.enter(Phase::Tools);
        self.tools
            .invoke(id, caps, input)
            .map_err(RuntimeError::Tool)
    }

    /// Ask the actuator for `command`; the gate decides.
    pub fn request_action(&mut self, command: f32) -> Result<Command, RuntimeError> {
        if !command.is_finite() {
            return Err(RuntimeError::BadCommand);
        }
        self.enter(Phase::Action);
        self.last_command = self.gate.check(command);
        Ok(self.last_command)
    }

    /// Engage the emergency stop; all later commands are refused.
    pub fn shutdown(&mut self) {
        self.gate.emergency_stop = true;
        self.last_command = Command::default();
        self.phase = Phase::Idle;
    }

    /// Completed ticks.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Current phase.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Times each phase was entered.
    #[must_use]
    pub fn phase_count(&self, p: Phase) -> u64 {
        self.phase_count[p.index()]
    }

    /// Latest belief state.
    #[must_use]
    pub fn inference(&self) -> InferenceState {
        self.inference
    }

    /// Action chosen on the last tick.
    #[must_use]
    pub fn selected_action(&self) -> usize {
        self.selected
    }

    /// Last gated command.
    #[must_use]
    pub fn last_command(&self) -> Command {
        self.last_command
    }

    /// Output of the last model step.
    #[must_use]
    pub fn model_output(&self) -> [f32; 4] {
        self.model_output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            actions: 4,
            epsilon: 1e-5,
            gate: ActionGate {
                motors_enabled: true,
                emergency_stop: false,
                max_command: 4.0,
            },
            memory: 8,
            workspace: 4,
        }
    }

    #[test]
    fn tick_runs_all_phases() {
        let mut r = Runtime::new(cfg()).unwrap();
        r.sensors.push(1, 10, 0.5).unwrap();
        r.tick(10).unwrap();
        r.tick(11).unwrap();
        assert_eq!(r.ticks(), 2);
        assert_eq!(r.phase(), Phase::Idle);
        assert_eq!(r.phase_count(Phase::Model), 2);
        assert!(r.last_command().approved);
        assert!(r.inference().confidence > 0.0);
    }

    #[test]
    fn shutdown_blocks_actions() {
        let mut r = Runtime::new(cfg()).unwrap();
        assert!(r.request_action(1.0).unwrap().approved);
        assert!(!r.request_action(5.0).unwrap().approved);
        r.shutdown();
        assert!(!r.request_action(1.0).unwrap().approved);
        assert_eq!(r.request_action(f32::NAN), Err(RuntimeError::BadCommand));
    }

    #[test]
    fn rejects_bad_config() {
        let mut c = cfg();
        c.actions = 0;
        assert!(Runtime::new(c).is_err());
        let mut c = cfg();
        c.gate.max_command = -1.0;
        assert!(Runtime::new(c).is_err());
    }
}
