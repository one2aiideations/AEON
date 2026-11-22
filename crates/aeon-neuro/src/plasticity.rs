//! Local learning rules. Each updates weights in place using only
//! information available at the synapse.

/// Hebbian: `w_i += lr * x_i * y`.
pub fn hebbian(weights: &mut [f32], pre: &[f32], post: f32, lr: f32) {
    for (w, &x) in weights.iter_mut().zip(pre) {
        *w += lr * x * post;
    }
}

/// Oja's rule: `w_i += lr * y * (x_i - y * w_i)`; keeps the weight norm
/// bounded and converges to the first principal component.
pub fn oja(weights: &mut [f32], pre: &[f32], post: f32, lr: f32) {
    for (w, &x) in weights.iter_mut().zip(pre) {
        *w += lr * post * (x - post * *w);
    }
}

/// Pair-based STDP parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stdp {
    /// Potentiation amplitude (pre before post).
    pub a_plus: f32,
    /// Depression amplitude (post before pre).
    pub a_minus: f32,
    /// Potentiation time constant (steps).
    pub tau_plus: f32,
    /// Depression time constant (steps).
    pub tau_minus: f32,
    /// Weight bounds.
    pub w_min: f32,
    /// Weight bounds.
    pub w_max: f32,
}

impl Default for Stdp {
    fn default() -> Self {
        Self {
            a_plus: 0.01,
            a_minus: 0.012,
            tau_plus: 20.0,
            tau_minus: 20.0,
            w_min: 0.0,
            w_max: 1.0,
        }
    }
}

impl Stdp {
    /// Weight change for one pre/post spike pair.
    #[must_use]
    pub fn delta(&self, pre_time: f32, post_time: f32) -> f32 {
        let dt = post_time - pre_time;
        if dt > 0.0 {
            self.a_plus * (-dt / self.tau_plus).exp()
        } else if dt < 0.0 {
            -self.a_minus * (dt / self.tau_minus).exp()
        } else {
            0.0
        }
    }

    /// Update each synapse from its last presynaptic spike time (`None` =
    /// no spike) relative to one postsynaptic spike.
    pub fn apply(&self, weights: &mut [f32], pre_times: &[Option<f32>], post_time: f32) {
        for (w, t) in weights.iter_mut().zip(pre_times) {
            if let Some(t) = *t {
                *w = (*w + self.delta(t, post_time)).clamp(self.w_min, self.w_max);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hebb_and_oja() {
        let mut w = [0.0f32, 0.0];
        hebbian(&mut w, &[1.0, 0.0], 1.0, 0.5);
        assert_eq!(w, [0.5, 0.0]);
        // Oja keeps a single weight near 1 for x = y = constant input.
        let mut v = [0.1f32];
        for _ in 0..2000 {
            let y = v[0];
            oja(&mut v, &[1.0], y, 0.05);
        }
        assert!((v[0] - 1.0).abs() < 0.05, "{}", v[0]);
    }

    #[test]
    fn stdp_sign_and_bounds() {
        let s = Stdp::default();
        assert!(s.delta(0.0, 5.0) > 0.0);
        assert!(s.delta(5.0, 0.0) < 0.0);
        assert_eq!(s.delta(3.0, 3.0), 0.0);
        assert!(s.delta(0.0, 1.0) > s.delta(0.0, 10.0));
        let mut w = [0.995f32, 0.5, 0.005];
        s.apply(&mut w, &[Some(9.0), None, Some(11.0)], 10.0);
        assert_eq!(w[0], 1.0);
        assert_eq!(w[1], 0.5);
        assert_eq!(w[2], 0.0);
    }
}
