//! Hard limits for an agent's repair loop. Charging past a limit is an error, not a warning.

use std::fmt;

/// The limits for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most outer iterations.
    pub max_iterations: u32,
    /// Most calls to a language model.
    pub max_model_calls: u32,
    /// Most attempts to apply and test a repair.
    pub max_repair_attempts: u32,
    /// Most wall-clock seconds one test run may take.
    pub max_test_runtime_secs: u32,
    /// Whether a patch may modify test files.
    pub allow_test_modification: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_iterations: 12,
            max_model_calls: 40,
            max_repair_attempts: 6,
            max_test_runtime_secs: 240,
            allow_test_modification: false,
        }
    }
}

/// Which limit was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// Outer iterations.
    Iterations,
    /// Model calls.
    ModelCalls,
    /// Repair attempts.
    RepairAttempts,
}

/// A limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    /// What ran out.
    pub resource: Resource,
    /// The limit that was reached.
    pub limit: u32,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} budget of {} exhausted", self.resource, self.limit)
    }
}

impl std::error::Error for BudgetExceeded {}

/// Counts usage against [`Limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    limits: Limits,
    iterations: u32,
    model_calls: u32,
    repair_attempts: u32,
}

impl Budget {
    /// A fresh budget with nothing used.
    pub const fn new(limits: Limits) -> Self {
        Self {
            limits,
            iterations: 0,
            model_calls: 0,
            repair_attempts: 0,
        }
    }

    /// The limits in force.
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    fn charge(used: &mut u32, limit: u32, resource: Resource) -> Result<(), BudgetExceeded> {
        if *used >= limit {
            return Err(BudgetExceeded { resource, limit });
        }
        *used += 1;
        Ok(())
    }

    /// Records one outer iteration.
    pub fn charge_iteration(&mut self) -> Result<(), BudgetExceeded> {
        Self::charge(
            &mut self.iterations,
            self.limits.max_iterations,
            Resource::Iterations,
        )
    }

    /// Records one model call.
    pub fn charge_model_call(&mut self) -> Result<(), BudgetExceeded> {
        Self::charge(
            &mut self.model_calls,
            self.limits.max_model_calls,
            Resource::ModelCalls,
        )
    }

    /// Records one repair attempt.
    pub fn charge_repair_attempt(&mut self) -> Result<(), BudgetExceeded> {
        Self::charge(
            &mut self.repair_attempts,
            self.limits.max_repair_attempts,
            Resource::RepairAttempts,
        )
    }

    /// Model calls still available.
    pub fn model_calls_left(&self) -> u32 {
        self.limits.max_model_calls - self.model_calls
    }
}
