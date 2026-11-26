//! Activation profiling.
//!
//! Records absolute activation sums, squared activation sums, threshold
//! firing counts, and token counts independently for each layer.
//!
//! Profiling does not establish that high-importance neurons are necessary
//! for a task. It only measures their activation statistics on observed
//! inputs.

use crate::{ActivationSink, SliceError};

/// Accumulates activation statistics for a dense MLP.
///
/// Each layer has independent statistics for its `d_ff` neurons.
#[derive(Debug, Clone, PartialEq)]
pub struct Profiler {
    layers: usize,
    d_ff: usize,
    threshold: f32,
    sum_abs: Vec<Vec<f64>>,
    sum_sq: Vec<Vec<f64>>,
    fire_count: Vec<Vec<usize>>,
    token_count: Vec<usize>,
}

impl Profiler {
    /// Creates an empty profiler.
    ///
    /// The default firing threshold is `1e-3`.
    ///
    /// Zero layers or zero neurons per layer are rejected.
    ///
    /// This constructor does not establish anything about model quality.
    ///
    /// # Errors
    /// [`SliceError::BadParameter`] for zero `layers` or `d_ff`.
    pub fn new(layers: usize, d_ff: usize) -> Result<Self, SliceError> {
        if layers == 0 {
            return Err(SliceError::BadParameter("layers"));
        }
        if d_ff == 0 {
            return Err(SliceError::BadParameter("d_ff"));
        }

        let mut sum_abs = Vec::with_capacity(layers);
        let mut sum_sq = Vec::with_capacity(layers);
        let mut fire_count = Vec::with_capacity(layers);

        for _ in 0..layers {
            sum_abs.push(vec![0.0; d_ff]);
            sum_sq.push(vec![0.0; d_ff]);
            fire_count.push(vec![0; d_ff]);
        }

        Ok(Self {
            layers,
            d_ff,
            threshold: 1e-3,
            sum_abs,
            sum_sq,
            fire_count,
            token_count: vec![0; layers],
        })
    }

    /// Sets the activation threshold used for firing counts.
    ///
    /// Firing means strictly `abs(activation) > threshold`.
    ///
    /// The threshold must be finite and non-negative.
    /// This changes future firing statistics, not existing observations.
    ///
    /// # Errors
    /// A [`SliceError`] for a non-finite or negative threshold.
    pub fn with_threshold(mut self, threshold: f32) -> Result<Self, SliceError> {
        if !threshold.is_finite() {
            return Err(SliceError::NonFinite("threshold"));
        }
        if threshold < 0.0 {
            return Err(SliceError::BadParameter("threshold"));
        }

        self.threshold = threshold;
        Ok(self)
    }

    /// Returns the configured number of layers.
    #[must_use]
    pub fn layers(&self) -> usize {
        self.layers
    }

    /// Returns the configured neurons per layer.
    #[must_use]
    pub fn d_ff(&self) -> usize {
        self.d_ff
    }

    /// Returns the configured firing threshold.
    #[must_use]
    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// Returns the mean absolute activation for each neuron.
    ///
    /// Neurons in a layer with no observations return zero.
    ///
    /// This statistic does not prove that removing a neuron preserves
    /// model behaviour.
    ///
    /// # Errors
    /// [`SliceError::LayerOutOfRange`] for a bad layer.
    pub fn importance(&self, layer: usize) -> Result<Vec<f64>, SliceError> {
        self.check_layer(layer)?;

        let tokens = self.token_count[layer];
        if tokens == 0 {
            return Ok(vec![0.0; self.d_ff]);
        }

        let divisor = tokens as f64;

        Ok(self.sum_abs[layer]
            .iter()
            .map(|sum| sum / divisor)
            .collect())
    }

    /// Returns the root-mean-square activation for each neuron.
    ///
    /// Neurons in a layer with no observations return zero. This is the
    /// reader for the squared sums the profiler keeps.
    ///
    /// # Errors
    /// [`SliceError::LayerOutOfRange`] for a bad layer.
    pub fn rms(&self, layer: usize) -> Result<Vec<f64>, SliceError> {
        self.check_layer(layer)?;

        let tokens = self.token_count[layer];
        if tokens == 0 {
            return Ok(vec![0.0; self.d_ff]);
        }

        let divisor = tokens as f64;

        Ok(self.sum_sq[layer]
            .iter()
            .map(|sum| (sum / divisor).sqrt())
            .collect())
    }

    /// Returns the fraction of observed tokens on which each neuron fired.
    ///
    /// A neuron fires when its absolute activation is strictly greater
    /// than the configured threshold.
    ///
    /// A layer without observations returns zeros.
    /// Firing frequency does not establish causal importance.
    ///
    /// # Errors
    /// [`SliceError::LayerOutOfRange`] for a bad layer.
    pub fn fire_rate(&self, layer: usize) -> Result<Vec<f64>, SliceError> {
        self.check_layer(layer)?;

        let tokens = self.token_count[layer];
        if tokens == 0 {
            return Ok(vec![0.0; self.d_ff]);
        }

        let divisor = tokens as f64;

        Ok(self.fire_count[layer]
            .iter()
            .map(|count| *count as f64 / divisor)
            .collect())
    }

    /// Returns the number of successfully recorded tokens for a layer.
    ///
    /// An out-of-range layer returns zero. Use `importance` or `fire_rate`
    /// when an invalid layer must be distinguished from an empty layer.
    #[must_use]
    pub fn tokens(&self, layer: usize) -> usize {
        self.token_count.get(layer).copied().unwrap_or(0)
    }

    /// Merges another profiler with identical dimensions and threshold.
    ///
    /// The merge is transactional: if a dimension mismatch, threshold
    /// mismatch, or arithmetic overflow occurs, this profiler is unchanged.
    ///
    /// Merging statistics does not make separate profiling datasets
    /// representative of any particular deployment distribution.
    ///
    /// # Errors
    /// A [`SliceError`] for mismatched shape or threshold, or overflow.
    pub fn merge(&mut self, other: &Profiler) -> Result<(), SliceError> {
        if self.layers != other.layers || self.d_ff != other.d_ff {
            return Err(SliceError::BadShape("profiler dimensions differ"));
        }

        if self.threshold != other.threshold {
            return Err(SliceError::BadParameter("profiler thresholds differ"));
        }

        let mut new_abs = self.sum_abs.clone();
        let mut new_sq = self.sum_sq.clone();
        let mut new_fire = self.fire_count.clone();
        let mut new_tokens = self.token_count.clone();

        for layer in 0..self.layers {
            new_tokens[layer] = new_tokens[layer]
                .checked_add(other.token_count[layer])
                .ok_or(SliceError::BadParameter("token count overflow"))?;

            for neuron in 0..self.d_ff {
                new_abs[layer][neuron] += other.sum_abs[layer][neuron];
                new_sq[layer][neuron] += other.sum_sq[layer][neuron];

                if !new_abs[layer][neuron].is_finite() || !new_sq[layer][neuron].is_finite() {
                    return Err(SliceError::NonFinite("merged statistics"));
                }

                new_fire[layer][neuron] = new_fire[layer][neuron]
                    .checked_add(other.fire_count[layer][neuron])
                    .ok_or(SliceError::BadParameter("firing count overflow"))?;
            }
        }

        self.sum_abs = new_abs;
        self.sum_sq = new_sq;
        self.fire_count = new_fire;
        self.token_count = new_tokens;

        Ok(())
    }

    fn check_layer(&self, layer: usize) -> Result<(), SliceError> {
        if layer >= self.layers {
            return Err(SliceError::LayerOutOfRange {
                layer,
                layers: self.layers,
            });
        }
        Ok(())
    }
}

impl ActivationSink for Profiler {
    /// Records a single token's activations for a layer.
    ///
    /// The entire observation is validated before any state is modified.
    /// Non-finite values, incorrect vector lengths, and counter overflow
    /// reject the entire call.
    ///
    /// Recording an activation does not prove that the corresponding
    /// neuron is important to the model's predictions.
    fn observe(&mut self, layer: usize, activations: &[f32]) -> Result<(), SliceError> {
        self.check_layer(layer)?;

        if activations.len() != self.d_ff {
            return Err(SliceError::BadShape(
                "activation length does not equal d_ff",
            ));
        }

        if self.token_count[layer] == usize::MAX {
            return Err(SliceError::BadParameter("token count overflow"));
        }

        // Validate and calculate all prospective values before mutation.
        let mut next_abs = Vec::with_capacity(self.d_ff);
        let mut next_sq = Vec::with_capacity(self.d_ff);
        let mut next_fire = Vec::with_capacity(self.d_ff);

        for (neuron, &activation) in activations.iter().enumerate() {
            if !activation.is_finite() {
                return Err(SliceError::NonFinite("activation"));
            }

            let value = f64::from(activation);
            let absolute = value.abs();

            let sum_abs = self.sum_abs[layer][neuron] + absolute;
            let sum_sq = self.sum_sq[layer][neuron] + value * value;

            if !sum_abs.is_finite() || !sum_sq.is_finite() {
                return Err(SliceError::NonFinite("activation accumulation"));
            }

            let fired = absolute > f64::from(self.threshold);

            let count = if fired {
                self.fire_count[layer][neuron]
                    .checked_add(1)
                    .ok_or(SliceError::BadParameter("firing count overflow"))?
            } else {
                self.fire_count[layer][neuron]
            };

            next_abs.push(sum_abs);
            next_sq.push(sum_sq);
            next_fire.push(count);
        }

        self.sum_abs[layer] = next_abs;
        self.sum_sq[layer] = next_sq;
        self.fire_count[layer] = next_fire;

        self.token_count[layer] += 1;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_empty_statistics() {
        let profiler = Profiler::new(2, 3).unwrap();

        assert_eq!(profiler.tokens(0), 0);
        assert_eq!(profiler.importance(0).unwrap(), vec![0.0; 3]);
        assert_eq!(profiler.fire_rate(1).unwrap(), vec![0.0; 3]);
        assert_eq!(profiler.rms(0).unwrap(), vec![0.0; 3]);
        assert!((profiler.threshold() - 1e-3).abs() < f32::EPSILON);
    }

    #[test]
    fn invalid_dimensions_are_rejected() {
        assert!(Profiler::new(0, 2).is_err());
        assert!(Profiler::new(2, 0).is_err());
    }

    #[test]
    fn shape_errors_and_layer_errors() {
        let mut profiler = Profiler::new(2, 2).unwrap();

        assert!(matches!(
            profiler.observe(0, &[1.0]),
            Err(SliceError::BadShape(_))
        ));

        assert!(matches!(
            profiler.observe(2, &[1.0, 2.0]),
            Err(SliceError::LayerOutOfRange { .. })
        ));

        assert!(matches!(
            profiler.importance(2),
            Err(SliceError::LayerOutOfRange { .. })
        ));
    }

    #[test]
    fn nan_rejection_leaves_state_unchanged() {
        let mut profiler = Profiler::new(1, 2).unwrap();
        profiler.observe(0, &[2.0, 3.0]).unwrap();

        let before = profiler.clone();

        assert!(matches!(
            profiler.observe(0, &[4.0, f32::NAN]),
            Err(SliceError::NonFinite(_))
        ));

        assert_eq!(profiler, before);
    }

    #[test]
    fn infinity_is_rejected() {
        let mut profiler = Profiler::new(1, 1).unwrap();

        assert!(matches!(
            profiler.observe(0, &[f32::INFINITY]),
            Err(SliceError::NonFinite(_))
        ));
        assert_eq!(profiler.tokens(0), 0);
    }

    #[test]
    fn importance_and_firing_rate() {
        let mut profiler = Profiler::new(1, 3).unwrap().with_threshold(0.5).unwrap();

        profiler.observe(0, &[1.0, 0.25, -2.0]).unwrap();
        profiler.observe(0, &[-3.0, 0.5, 0.0]).unwrap();

        let importance = profiler.importance(0).unwrap();
        let rate = profiler.fire_rate(0).unwrap();

        assert_eq!(importance, vec![2.0, 0.375, 1.0]);
        // Neuron 2 sees -2.0 then 0.0: it fires on one of two tokens.
        assert_eq!(rate, vec![1.0, 0.0, 0.5]);
        assert_eq!(profiler.tokens(0), 2);
    }

    #[test]
    fn rms_matches_hand_calculation() {
        let mut profiler = Profiler::new(1, 2).unwrap();
        profiler.observe(0, &[3.0, 0.0]).unwrap();
        profiler.observe(0, &[-4.0, 0.0]).unwrap();

        let rms = profiler.rms(0).unwrap();
        assert!((rms[0] - (12.5f64).sqrt()).abs() < 1e-12);
        assert_eq!(rms[1], 0.0);
        assert!(profiler.rms(5).is_err());
    }

    #[test]
    fn invalid_thresholds() {
        assert!(Profiler::new(1, 1).unwrap().with_threshold(-1.0).is_err());

        assert!(Profiler::new(1, 1)
            .unwrap()
            .with_threshold(f32::NAN)
            .is_err());
    }

    #[test]
    fn merge_matches_single_run() {
        let mut combined = Profiler::new(2, 2).unwrap();
        let mut first = Profiler::new(2, 2).unwrap();
        let mut second = Profiler::new(2, 2).unwrap();

        let observations = [
            (0, [1.0, 2.0]),
            (1, [3.0, 4.0]),
            (0, [5.0, 6.0]),
            (1, [7.0, 8.0]),
        ];

        for (index, (layer, values)) in observations.iter().enumerate() {
            combined.observe(*layer, values).unwrap();

            if index < 2 {
                first.observe(*layer, values).unwrap();
            } else {
                second.observe(*layer, values).unwrap();
            }
        }

        first.merge(&second).unwrap();
        assert_eq!(first, combined);
    }

    #[test]
    fn incompatible_merge_is_transactional() {
        let mut first = Profiler::new(1, 2).unwrap();
        let second = Profiler::new(1, 3).unwrap();
        let before = first.clone();

        assert!(first.merge(&second).is_err());
        assert_eq!(first, before);
    }

    #[test]
    fn different_thresholds_cannot_be_merged() {
        let mut first = Profiler::new(1, 1).unwrap();
        let second = Profiler::new(1, 1).unwrap().with_threshold(0.5).unwrap();

        assert!(first.merge(&second).is_err());
    }
}
