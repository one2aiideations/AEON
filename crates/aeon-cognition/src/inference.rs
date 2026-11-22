//! Free-energy bookkeeping and active-inference action selection.

use crate::{finite, non_negative, CogError};

/// Snapshot of the agent's belief quality.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InferenceState {
    /// Observation minus prediction.
    pub prediction_error: f32,
    /// `0.5 e^2 + uncertainty`.
    pub free_energy: f32,
    /// Free energy plus goal error (or the chosen action's score).
    pub expected_free_energy: f32,
    /// `1 / (1 + |e| + uncertainty)`, in (0, 1].
    pub confidence: f32,
}

impl InferenceState {
    /// Update from one prediction / observation pair.
    pub fn update(
        &mut self,
        prediction: f32,
        observation: f32,
        uncertainty: f32,
        goal_error: f32,
    ) -> Result<(), CogError> {
        let e = finite(observation)? - finite(prediction)?;
        let u = finite(uncertainty)?.max(0.0);
        let g = finite(goal_error)?.max(0.0);
        self.prediction_error = e;
        self.free_energy = 0.5 * e * e + u;
        self.expected_free_energy = self.free_energy + g;
        self.confidence = 1.0 / (1.0 + e.abs() + u);
        Ok(())
    }
}

/// Pick the action with the lowest expected free energy.
///
/// Action `a` scores `risk = err * (1 + 0.1 a)` plus
/// `epistemic = uncertainty / (a + 1)`: higher actions explore more but
/// carry more risk. Returns the action and the resulting state.
pub fn select_action(
    prediction_error: f32,
    uncertainty: f32,
    actions: usize,
) -> Result<(usize, InferenceState), CogError> {
    let err = non_negative(prediction_error)?;
    let unc = non_negative(uncertainty)?;
    if actions == 0 {
        return Err(CogError::Empty);
    }
    let score = |a: usize| err * (1.0 + 0.1 * a as f32) + unc / (a + 1) as f32;
    let (best, efe) = (0..actions)
        .map(|a| (a, score(a)))
        .min_by(|x, y| x.1.total_cmp(&y.1))
        .ok_or(CogError::Empty)?;
    Ok((
        best,
        InferenceState {
            prediction_error: err,
            free_energy: 0.5 * err * err + unc,
            expected_free_energy: efe,
            confidence: 1.0 / (1.0 + err + unc),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exploit_when_certain_explore_when_uncertain() {
        let (a, s) = select_action(1.0, 0.0, 4).unwrap();
        assert_eq!(a, 0);
        assert!((s.confidence - 0.5).abs() < 1e-6);
        let (a, _) = select_action(0.1, 10.0, 4).unwrap();
        assert_eq!(a, 3);
        assert!(select_action(-1.0, 0.0, 4).is_err());
        assert!(select_action(1.0, 0.0, 0).is_err());
    }

    #[test]
    fn update_values() {
        let mut s = InferenceState::default();
        s.update(1.0, 3.0, 0.5, 1.0).unwrap();
        assert!((s.free_energy - 2.5).abs() < 1e-6);
        assert!((s.expected_free_energy - 3.5).abs() < 1e-6);
    }
}
