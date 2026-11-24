//! Actuator gate: commands pass only when motors are enabled, no emergency
//! stop is active and the magnitude is within limits.

/// Actuator policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActionGate {
    /// Motors may move.
    pub motors_enabled: bool,
    /// Hard stop; overrides everything.
    pub emergency_stop: bool,
    /// Largest allowed `|command|`.
    pub max_command: f32,
}

/// Gate output.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Command {
    /// Command to send (0 when refused).
    pub value: f32,
    /// Whether it was approved.
    pub approved: bool,
}

impl ActionGate {
    /// Limits are finite and non-negative.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.max_command.is_finite() && self.max_command >= 0.0
    }

    /// Decide `requested`. Refusals return a zero, unapproved command.
    #[must_use]
    pub fn check(&self, requested: f32) -> Command {
        if self.emergency_stop
            || !self.motors_enabled
            || !self.is_valid()
            || !requested.is_finite()
            || requested.abs() > self.max_command
        {
            return Command::default();
        }
        Command {
            value: requested,
            approved: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_rules() {
        let mut g = ActionGate {
            motors_enabled: true,
            emergency_stop: false,
            max_command: 1.0,
        };
        assert!(g.check(-1.0).approved);
        assert!(!g.check(1.5).approved);
        assert!(!g.check(f32::NAN).approved);
        g.emergency_stop = true;
        assert_eq!(g.check(0.5), Command::default());
    }
}
