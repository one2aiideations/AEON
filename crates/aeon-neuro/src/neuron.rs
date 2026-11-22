//! Leaky integrate-and-fire neuron with an adaptive threshold.

/// Neuron parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Membrane retention per time step (0 < decay <= 1).
    pub decay: f32,
    /// Starting threshold.
    pub threshold: f32,
    /// Desired firing rate (fires per step) for homeostasis.
    pub target_rate: f32,
    /// Threshold adaptation rate (0 disables homeostasis).
    pub adapt_rate: f32,
    /// Threshold bounds.
    pub min_threshold: f32,
    /// Threshold bounds.
    pub max_threshold: f32,
    /// Steps after firing during which input is ignored.
    pub refractory: u32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            decay: 0.95,
            threshold: 1.0,
            target_rate: 0.1,
            adapt_rate: 0.0,
            min_threshold: 0.01,
            max_threshold: 10.0,
            refractory: 1,
        }
    }
}

impl Params {
    /// Parameters are in range.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.decay > 0.0
            && self.decay <= 1.0
            && self.min_threshold > 0.0
            && self.min_threshold <= self.threshold
            && self.threshold <= self.max_threshold
            && self.adapt_rate >= 0.0
            && (0.0..=1.0).contains(&self.target_rate)
    }
}

/// Neuron state.
#[derive(Debug, Clone, PartialEq)]
pub struct Neuron {
    /// Parameters.
    pub params: Params,
    /// Membrane potential.
    pub potential: f32,
    /// Current threshold.
    pub threshold: f32,
    /// Time of last update.
    pub last_time: u64,
    /// Time of last spike, if any.
    pub last_spike: Option<u64>,
    /// Total spikes.
    pub spikes: u64,
}

impl Neuron {
    /// Resting neuron.
    #[must_use]
    pub fn new(params: Params) -> Self {
        Self {
            threshold: params.threshold,
            params,
            potential: 0.0,
            last_time: 0,
            last_spike: None,
            spikes: 0,
        }
    }

    /// Apply leak for the steps elapsed since the last update.
    fn leak_to(&mut self, t: u64) {
        let dt = t.saturating_sub(self.last_time);
        if dt > 0 {
            let steps = i32::try_from(dt).unwrap_or(i32::MAX);
            self.potential *= self.params.decay.powi(steps);
            self.last_time = t;
        }
    }

    fn refractory_at(&self, t: u64) -> bool {
        self.last_spike
            .is_some_and(|s| (s..=s.saturating_add(u64::from(self.params.refractory))).contains(&t))
    }

    /// Add `input` at time `t`. Returns the membrane value at firing, if it fired.
    pub fn receive(&mut self, t: u64, input: f32) -> Option<f32> {
        self.leak_to(t);
        if self.refractory_at(t) {
            return None;
        }
        self.potential += input;
        if self.potential >= self.threshold {
            let v = self.potential;
            self.potential = 0.0;
            self.last_spike = Some(t);
            self.spikes += 1;
            Some(v)
        } else {
            None
        }
    }

    /// Homeostasis: move the threshold toward producing `target_rate`
    /// given the observed `rate`.
    pub fn adapt(&mut self, rate: f32) {
        let p = &self.params;
        self.threshold = (self.threshold + p.adapt_rate * (rate - p.target_rate))
            .clamp(p.min_threshold, p.max_threshold);
    }

    /// Return to rest (keeps the adapted threshold).
    pub fn reset(&mut self) {
        self.potential = 0.0;
        self.last_spike = None;
        self.last_time = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrates_leaks_and_fires() {
        let mut n = Neuron::new(Params {
            decay: 0.5,
            ..Params::default()
        });
        assert_eq!(n.receive(0, 0.6), None);
        // After one step: 0.3 + 0.6 = 0.9 < 1.
        assert_eq!(n.receive(1, 0.6), None);
        let v = n.receive(1, 0.2).unwrap();
        assert!((v - 1.1).abs() < 1e-6);
        assert_eq!(n.potential, 0.0);
        // Refractory for one step.
        assert_eq!(n.receive(2, 5.0), None);
        assert!(n.receive(3, 5.0).is_some());
        assert_eq!(n.spikes, 2);
    }

    #[test]
    fn homeostasis_moves_threshold() {
        let mut n = Neuron::new(Params {
            adapt_rate: 1.0,
            ..Params::default()
        });
        n.adapt(0.6);
        assert!((n.threshold - 1.5).abs() < 1e-6);
        n.adapt(-100.0);
        assert!((n.threshold - 0.01).abs() < 1e-6);
        assert!(Params::default().is_valid());
        assert!(!Params {
            decay: 0.0,
            ..Params::default()
        }
        .is_valid());
    }
}
