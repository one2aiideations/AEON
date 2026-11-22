//! Eligibility traces and reward-modulated plasticity (three-factor learning).
//!
//! A synapse that was recently active keeps a decaying *eligibility trace*. When a reward
//! (or punishment) signal arrives later, every synapse changes in proportion to its trace,
//! so credit reaches the connections that actually took part, even when the outcome comes
//! after the activity. This is the standard remedy for delayed reward in spiking networks.

/// One decaying trace per synapse.
#[derive(Debug, Clone, PartialEq)]
pub struct Eligibility {
    traces: Vec<f32>,
    decay: f32,
}

/// Invalid parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EligibilityError {
    /// `decay` must be in `[0, 1]`.
    BadDecay,
    /// Slice lengths differ.
    LengthMismatch,
    /// Index out of range.
    OutOfRange,
}

impl std::fmt::Display for EligibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EligibilityError::BadDecay => "decay must be in [0, 1]",
            EligibilityError::LengthMismatch => "slice length does not match the trace count",
            EligibilityError::OutOfRange => "synapse index out of range",
        })
    }
}

impl std::error::Error for EligibilityError {}

impl Eligibility {
    /// `n` traces, each multiplied by `decay` per [`Eligibility::step`]
    /// (1 never forgets, 0 forgets immediately).
    pub fn new(n: usize, decay: f32) -> Result<Self, EligibilityError> {
        if !(0.0..=1.0).contains(&decay) || decay.is_nan() {
            return Err(EligibilityError::BadDecay);
        }
        Ok(Eligibility {
            traces: vec![0.0; n],
            decay,
        })
    }

    /// Number of traces.
    pub fn len(&self) -> usize {
        self.traces.len()
    }

    /// `true` if there are no traces.
    pub fn is_empty(&self) -> bool {
        self.traces.is_empty()
    }

    /// Current traces.
    pub fn traces(&self) -> &[f32] {
        &self.traces
    }

    /// Age every trace by one time step.
    pub fn step(&mut self) {
        for t in &mut self.traces {
            *t *= self.decay;
        }
    }

    /// Mark synapse `i` as having just been active (adds `amount`, which may be negative to
    /// record an anti-causal pairing).
    pub fn mark(&mut self, i: usize, amount: f32) -> Result<(), EligibilityError> {
        let t = self.traces.get_mut(i).ok_or(EligibilityError::OutOfRange)?;
        *t += amount;
        Ok(())
    }

    /// Clear all traces.
    pub fn reset(&mut self) {
        self.traces.fill(0.0);
    }

    /// Apply a reward signal: `w += lr * reward * trace`, clamped to `[w_min, w_max]`.
    /// A negative `reward` weakens the synapses that were active. Traces are kept, so a
    /// later correction can still act on the same activity; call [`Eligibility::reset`]
    /// to consume them.
    pub fn apply_reward(
        &self,
        weights: &mut [f32],
        reward: f32,
        lr: f32,
        w_min: f32,
        w_max: f32,
    ) -> Result<(), EligibilityError> {
        if weights.len() != self.traces.len() {
            return Err(EligibilityError::LengthMismatch);
        }
        for (w, t) in weights.iter_mut().zip(&self.traces) {
            *w = (*w + lr * reward * t).clamp(w_min, w_max);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_decay() {
        assert_eq!(
            Eligibility::new(3, -0.1).unwrap_err(),
            EligibilityError::BadDecay
        );
        assert_eq!(
            Eligibility::new(3, 1.1).unwrap_err(),
            EligibilityError::BadDecay
        );
        assert_eq!(
            Eligibility::new(3, f32::NAN).unwrap_err(),
            EligibilityError::BadDecay
        );
        assert!(Eligibility::new(0, 1.0).unwrap().is_empty());
    }

    #[test]
    fn traces_accumulate_and_decay() {
        let mut e = Eligibility::new(2, 0.5).unwrap();
        e.mark(0, 1.0).unwrap();
        e.mark(0, 1.0).unwrap();
        e.mark(1, -1.0).unwrap();
        assert_eq!(e.traces(), &[2.0, -1.0]);
        e.step();
        assert_eq!(e.traces(), &[1.0, -0.5]);
        e.step();
        assert_eq!(e.traces(), &[0.5, -0.25]);
        assert_eq!(e.mark(2, 1.0), Err(EligibilityError::OutOfRange));
        e.reset();
        assert_eq!(e.traces(), &[0.0, 0.0]);
        assert_eq!(e.len(), 2);
    }

    #[test]
    fn decay_extremes() {
        let mut keep = Eligibility::new(1, 1.0).unwrap();
        keep.mark(0, 3.0).unwrap();
        for _ in 0..100 {
            keep.step();
        }
        assert_eq!(keep.traces(), &[3.0]);
        let mut forget = Eligibility::new(1, 0.0).unwrap();
        forget.mark(0, 3.0).unwrap();
        forget.step();
        assert_eq!(forget.traces(), &[0.0]);
    }

    #[test]
    fn reward_moves_weights_in_proportion_to_traces() {
        let mut e = Eligibility::new(3, 0.9).unwrap();
        e.mark(0, 1.0).unwrap();
        e.mark(1, 0.5).unwrap(); // synapse 2 never active
        let mut w = [0.5f32, 0.5, 0.5];
        e.apply_reward(&mut w, 1.0, 0.2, 0.0, 1.0).unwrap();
        assert!((w[0] - 0.7).abs() < 1e-6 && (w[1] - 0.6).abs() < 1e-6 && w[2] == 0.5);
        e.apply_reward(&mut w, -1.0, 0.2, 0.0, 1.0).unwrap(); // punishment undoes it
        assert!((w[0] - 0.5).abs() < 1e-6 && (w[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn delayed_reward_credits_earlier_activity() {
        let mut e = Eligibility::new(2, 0.8).unwrap();
        e.mark(0, 1.0).unwrap();
        for _ in 0..5 {
            e.step(); // time passes before the outcome is known
        }
        e.mark(1, 1.0).unwrap(); // synapse 1 fires just before the reward
        let mut w = [0.0f32, 0.0];
        e.apply_reward(&mut w, 1.0, 1.0, -1.0, 1.0).unwrap();
        assert!(w[0] > 0.0, "earlier activity still earns credit");
        assert!(w[1] > w[0], "recent activity earns more");
    }

    #[test]
    fn weights_are_clamped_and_lengths_checked() {
        let mut e = Eligibility::new(2, 1.0).unwrap();
        e.mark(0, 10.0).unwrap();
        e.mark(1, -10.0).unwrap();
        let mut w = [0.9f32, 0.1];
        e.apply_reward(&mut w, 1.0, 1.0, 0.0, 1.0).unwrap();
        assert_eq!(w, [1.0, 0.0]);
        assert_eq!(
            e.apply_reward(&mut [0.0], 1.0, 1.0, 0.0, 1.0),
            Err(EligibilityError::LengthMismatch)
        );
    }

    #[test]
    fn traces_survive_reward_until_reset() {
        let mut e = Eligibility::new(1, 1.0).unwrap();
        e.mark(0, 1.0).unwrap();
        let mut w = [0.0f32];
        e.apply_reward(&mut w, 0.1, 1.0, -1.0, 1.0).unwrap();
        e.apply_reward(&mut w, 0.1, 1.0, -1.0, 1.0).unwrap();
        assert!((w[0] - 0.2).abs() < 1e-6);
        e.reset();
        e.apply_reward(&mut w, 1.0, 1.0, -1.0, 1.0).unwrap();
        assert!((w[0] - 0.2).abs() < 1e-6);
    }
}
