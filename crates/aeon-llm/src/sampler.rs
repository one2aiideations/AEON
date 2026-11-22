//! Token sampling: greedy, temperature, top-k, top-p, with a seeded generator.

/// Chooses the next token from logits.
#[derive(Debug, Clone)]
pub struct Sampler {
    temperature: f32,
    top_k: usize,
    top_p: f32,
    rng: u64,
}

impl Sampler {
    /// Always pick the highest logit (ties go to the lowest id).
    pub fn greedy() -> Self {
        Sampler {
            temperature: 0.0,
            top_k: 0,
            top_p: 1.0,
            rng: 0,
        }
    }

    /// Random sampling. `temperature <= 0` means greedy. `top_k == 0` disables top-k;
    /// `top_p >= 1` disables nucleus filtering. The same seed gives the same draws.
    pub fn new(temperature: f32, top_k: usize, top_p: f32, seed: u64) -> Self {
        Sampler {
            temperature,
            top_k,
            top_p,
            rng: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_unit(&mut self) -> f64 {
        // SplitMix64
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Pick a token id. NaN logits count as negative infinity; with no usable logit (or an
    /// empty slice) the result is 0.
    pub fn sample(&mut self, logits: &[f32]) -> u32 {
        let argmax = || {
            let mut best: Option<(usize, f32)> = None;
            for (i, v) in logits.iter().enumerate() {
                if v.is_nan() {
                    continue;
                }
                if best.is_none_or(|(_, b)| *v > b) {
                    best = Some((i, *v));
                }
            }
            best.map_or(0, |(i, _)| i as u32)
        };
        if self.temperature <= 0.0 || !self.temperature.is_finite() || logits.len() < 2 {
            return argmax();
        }
        let mut cand: Vec<(usize, f32)> = logits
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, v)| !v.is_nan())
            .collect();
        if cand.is_empty() {
            return 0;
        }
        cand.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        if self.top_k > 0 && self.top_k < cand.len() {
            cand.truncate(self.top_k);
        }
        let max = cand[0].1;
        if max == f32::NEG_INFINITY {
            return argmax();
        }
        let mut probs: Vec<f64> = cand
            .iter()
            .map(|(_, v)| (f64::from(*v - max) / f64::from(self.temperature)).exp())
            .collect();
        let total: f64 = probs.iter().sum();
        for p in &mut probs {
            *p /= total;
        }
        if self.top_p > 0.0 && self.top_p < 1.0 {
            let mut cum = 0.0;
            let mut keep = probs.len();
            for (i, p) in probs.iter().enumerate() {
                cum += p;
                if cum >= f64::from(self.top_p) {
                    keep = i + 1;
                    break;
                }
            }
            cand.truncate(keep);
            probs.truncate(keep);
            let t: f64 = probs.iter().sum();
            for p in &mut probs {
                *p /= t;
            }
        }
        let u = self.next_unit();
        let mut cum = 0.0;
        for ((id, _), p) in cand.iter().zip(&probs) {
            cum += p;
            if u < cum {
                return *id as u32;
            }
        }
        cand[cand.len() - 1].0 as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greedy_picks_the_max_and_breaks_ties_low() {
        let mut s = Sampler::greedy();
        assert_eq!(s.sample(&[0.1, 0.9, 0.5]), 1);
        assert_eq!(s.sample(&[1.0, 1.0, 0.5]), 0);
        assert_eq!(s.sample(&[f32::NAN, 0.2, f32::NAN]), 1);
        assert_eq!(s.sample(&[f32::NEG_INFINITY, f32::NEG_INFINITY]), 0);
        assert_eq!(s.sample(&[]), 0);
        assert_eq!(s.sample(&[5.0]), 0);
    }

    #[test]
    fn zero_temperature_is_greedy() {
        let mut s = Sampler::new(0.0, 5, 0.9, 1);
        for _ in 0..20 {
            assert_eq!(s.sample(&[0.0, 3.0, 1.0]), 1);
        }
    }

    #[test]
    fn same_seed_same_sequence_different_seed_differs() {
        let logits: Vec<f32> = (0..50).map(|i| (i % 7) as f32 * 0.3).collect();
        let draw = |seed| {
            let mut s = Sampler::new(1.0, 0, 1.0, seed);
            (0..40).map(|_| s.sample(&logits)).collect::<Vec<_>>()
        };
        assert_eq!(draw(5), draw(5));
        assert_ne!(draw(5), draw(6));
    }

    #[test]
    fn top_k_restricts_candidates() {
        let logits = [5.0f32, 4.0, 3.0, 2.0, 1.0, 0.0];
        let mut s = Sampler::new(5.0, 2, 1.0, 9); // hot, but only ids 0 and 1 allowed
        for _ in 0..200 {
            assert!(s.sample(&logits) <= 1);
        }
    }

    #[test]
    fn top_p_keeps_the_nucleus() {
        // probabilities ~ [0.88, 0.12, tiny...]: top_p 0.5 keeps only the first
        let logits = [4.0f32, 2.0, -5.0, -5.0];
        let mut s = Sampler::new(1.0, 0, 0.5, 3);
        for _ in 0..200 {
            assert_eq!(s.sample(&logits), 0);
        }
    }

    #[test]
    fn sampling_follows_the_distribution() {
        // two equal logits: roughly half each
        let mut s = Sampler::new(1.0, 0, 1.0, 11);
        let n = 4000;
        let ones = (0..n).filter(|_| s.sample(&[1.0, 1.0]) == 1).count();
        assert!((1700..2300).contains(&ones), "{ones}");
    }

    #[test]
    fn hot_temperature_flattens_cold_sharpens() {
        let logits = [2.0f32, 1.0, 0.0];
        let count_top = |t: f32| {
            let mut s = Sampler::new(t, 0, 1.0, 17);
            (0..2000).filter(|_| s.sample(&logits) == 0).count()
        };
        assert!(count_top(0.2) > count_top(1.0) && count_top(1.0) > count_top(5.0));
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let mut s = Sampler::new(1.0, 0, 0.9, 1);
        assert_eq!(s.sample(&[f32::NAN, f32::NAN]), 0);
        assert_eq!(
            s.sample(&[f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY]),
            0
        );
        let _ = s.sample(&[f32::INFINITY, 1.0]);
        let mut hot = Sampler::new(f32::NAN, 0, 1.0, 1);
        assert_eq!(hot.sample(&[0.0, 1.0]), 1);
    }
}
