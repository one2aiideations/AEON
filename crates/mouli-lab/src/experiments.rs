//! Temporal experiments and stability checks.
//!
//! [`run_temporal`] trains one model on the delayed-match task and reports its accuracy on fresh
//! batches together with the stability counters (how often the gradient guard tripped, and
//! whether any loss or probability went non-finite). Nothing here is tuned to a target number.

use crate::rng::Rng;
use crate::temporal::{make_batch, GatedAetheron, MiniTransformer, RecurrentAetheron};
use crate::F;
use std::time::Instant;

/// Which temporal model to train.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalModel {
    /// Recurrent Aetheron.
    Recurrent,
    /// Gated Aetheron.
    Gated,
    /// One-layer single-head transformer baseline.
    Transformer,
}

impl TemporalModel {
    /// Reads `recurrent`, `gated` or `transformer`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "recurrent" => Some(Self::Recurrent),
            "gated" => Some(Self::Gated),
            "transformer" => Some(Self::Transformer),
            _ => None,
        }
    }

    /// The name used in result files.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Recurrent => "recurrent",
            Self::Gated => "gated",
            Self::Transformer => "transformer",
        }
    }
}

/// Settings for one temporal run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemporalCfg {
    /// Sequence length (at least 2: the cue is the first token, the probe the last).
    pub length: usize,
    /// Number of content classes (at least 2).
    pub classes: usize,
    /// Standard deviation of the noise on the non-marker inputs.
    pub noise: F,
    /// Training steps.
    pub steps: usize,
    /// Sequences per step.
    pub batch: usize,
    /// Learning rate.
    pub lr: F,
    /// Sequences used to measure accuracy afterwards.
    pub eval_sequences: usize,
}

impl Default for TemporalCfg {
    fn default() -> Self {
        Self {
            length: 8,
            classes: 4,
            noise: 0.05,
            steps: 400,
            batch: 32,
            lr: 0.01,
            eval_sequences: 512,
        }
    }
}

/// What one temporal run measured.
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalResult {
    /// Model name.
    pub model: String,
    /// Seed.
    pub seed: u64,
    /// Parameters in the model.
    pub params: usize,
    /// Accuracy on fresh sequences (chance is 0.5).
    pub accuracy: F,
    /// Mean loss over the last tenth of training.
    pub final_loss: F,
    /// Times the gradient guard zeroed a non-finite or clipped gradient.
    pub guard_events: u64,
    /// Steps whose loss was not finite.
    pub non_finite_losses: u64,
    /// Evaluation probabilities that were not finite.
    pub non_finite_probs: u64,
    /// Seconds taken.
    pub seconds: F,
}

impl TemporalResult {
    /// Whether the run was numerically clean: nothing non-finite anywhere.
    #[must_use]
    pub fn is_stable(&self) -> bool {
        self.non_finite_losses == 0
            && self.non_finite_probs == 0
            && self.final_loss.is_finite()
            && self.accuracy.is_finite()
    }
}

/// Why a run could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadCfg(pub String);

impl std::fmt::Display for BadCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn check(cfg: &TemporalCfg) -> Result<(), BadCfg> {
    if cfg.length < 2 {
        return Err(BadCfg("the sequence length must be at least 2".into()));
    }
    if cfg.classes < 2 {
        return Err(BadCfg("there must be at least 2 classes".into()));
    }
    if cfg.steps == 0 || cfg.batch == 0 || cfg.eval_sequences == 0 {
        return Err(BadCfg(
            "steps, batch and eval sequences must be above 0".into(),
        ));
    }
    if !(cfg.lr.is_finite() && cfg.lr > 0.0) || !(cfg.noise.is_finite() && cfg.noise >= 0.0) {
        return Err(BadCfg(
            "the learning rate must be above 0 and the noise at least 0".into(),
        ));
    }
    Ok(())
}

/// Trains `model` on the delayed-match task and measures it on fresh sequences.
///
/// # Errors
/// [`BadCfg`] for settings that cannot run.
pub fn run_temporal(
    model: TemporalModel,
    seed: u64,
    cfg: &TemporalCfg,
) -> Result<TemporalResult, BadCfg> {
    check(cfg)?;
    let start = Instant::now();
    let din = 2 + cfg.classes;
    let mut rng = Rng::new(seed ^ 0x9E37_79B9);
    // One closure per model family would need dynamic dispatch; a small enum keeps it plain.
    enum M {
        R(RecurrentAetheron),
        G(GatedAetheron),
        T(MiniTransformer),
    }
    let mut m = match model {
        TemporalModel::Recurrent => M::R(RecurrentAetheron::new(din, 8, 2, 6, seed)),
        TemporalModel::Gated => M::G(GatedAetheron::new(din, 8, 2, 6, seed)),
        TemporalModel::Transformer => M::T(MiniTransformer::new(din, 8, seed)),
    };
    let params = match &m {
        M::R(x) => x.n_params(),
        M::G(x) => x.n_params(),
        M::T(x) => x.n_params(),
    };
    let mut non_finite_losses = 0u64;
    let tail_from = cfg.steps - (cfg.steps / 10).max(1);
    let mut tail = Vec::new();
    for i in 0..cfg.steps {
        let (xs, y) = make_batch(cfg.length, cfg.batch, cfg.classes, cfg.noise, &mut rng);
        let loss = match &mut m {
            M::R(x) => x.step(&xs, &y, cfg.lr),
            M::G(x) => x.step(&xs, &y, cfg.lr),
            M::T(x) => x.step(&xs, &y, cfg.lr),
        };
        if !loss.is_finite() {
            non_finite_losses += 1;
        } else if i >= tail_from {
            tail.push(loss);
        }
    }
    // Evaluation on sequences the model has not seen (a different stream of the generator).
    let mut eval_rng = Rng::new(seed ^ 0xA5A5_5A5A_1234_5678);
    let (xs, y) = make_batch(
        cfg.length,
        cfg.eval_sequences,
        cfg.classes,
        cfg.noise,
        &mut eval_rng,
    );
    let probs = match &m {
        M::R(x) => x.predict_prob(&xs),
        M::G(x) => x.predict_prob(&xs),
        M::T(x) => x.predict_prob(&xs),
    };
    let non_finite_probs = probs.iter().filter(|p| !p.is_finite()).count() as u64;
    let correct = probs
        .iter()
        .zip(&y)
        .filter(|(p, t)| p.is_finite() && ((**p >= 0.5) == (**t >= 0.5)))
        .count();
    let guard_events = match &m {
        M::R(x) => x.guard_events,
        M::G(x) => x.guard_events,
        M::T(x) => x.guard_events,
    };
    Ok(TemporalResult {
        model: model.name().to_string(),
        seed,
        params,
        accuracy: correct as F / y.len() as F,
        final_loss: if tail.is_empty() {
            F::NAN
        } else {
            tail.iter().sum::<F>() / tail.len() as F
        },
        guard_events,
        non_finite_losses,
        non_finite_probs,
        seconds: start.elapsed().as_secs_f64(),
    })
}

/// Markdown for temporal results: one row per model with mean accuracy over seeds, and the
/// stability counters summed. A model with any non-finite value is flagged, not averaged away.
#[must_use]
pub fn temporal_table(results: &[TemporalResult]) -> String {
    let mut out = String::from(
        "| model | seeds | params | accuracy mean | accuracy min | guard events | non-finite | stable |\n|---|---|---|---|---|---|---|---|\n",
    );
    let mut names: Vec<&str> = results.iter().map(|r| r.model.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        let rs: Vec<&TemporalResult> = results.iter().filter(|r| r.model == name).collect();
        let accs: Vec<F> = rs
            .iter()
            .map(|r| r.accuracy)
            .filter(|a| a.is_finite())
            .collect();
        let mean = if accs.is_empty() {
            F::NAN
        } else {
            accs.iter().sum::<F>() / accs.len() as F
        };
        let min = accs.iter().copied().fold(F::INFINITY, F::min);
        let guards: u64 = rs.iter().map(|r| r.guard_events).sum();
        let bad: u64 = rs
            .iter()
            .map(|r| r.non_finite_losses + r.non_finite_probs)
            .sum();
        let stable = rs.iter().all(|r| r.is_stable());
        out.push_str(&format!(
            "| {name} | {} | {} | {mean:.4} | {min:.4} | {guards} | {bad} | {} |\n",
            rs.len(),
            rs[0].params,
            if stable { "yes" } else { "NO" }
        ));
    }
    out.push_str("\nChance accuracy is 0.5. Accuracy is measured on fresh sequences, not the training stream.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick() -> TemporalCfg {
        TemporalCfg {
            steps: 60,
            batch: 16,
            eval_sequences: 64,
            ..TemporalCfg::default()
        }
    }

    #[test]
    fn a_run_is_reproducible_and_reports_its_counters() {
        let a = run_temporal(TemporalModel::Gated, 3, &quick()).unwrap();
        let b = run_temporal(TemporalModel::Gated, 3, &quick()).unwrap();
        assert_eq!(
            (a.accuracy, a.final_loss, a.guard_events),
            (b.accuracy, b.final_loss, b.guard_events)
        );
        assert!(a.is_stable(), "{a:?}");
        assert!(a.params > 0 && (0.0..=1.0).contains(&a.accuracy));
        let c = run_temporal(TemporalModel::Gated, 4, &quick()).unwrap();
        assert_ne!(
            a.final_loss, c.final_loss,
            "another seed trains differently"
        );
    }

    #[test]
    fn every_model_runs_and_a_trained_model_beats_chance() {
        for m in [
            TemporalModel::Recurrent,
            TemporalModel::Gated,
            TemporalModel::Transformer,
        ] {
            assert!(run_temporal(m, 1, &quick()).unwrap().is_stable(), "{m:?}");
        }
        let cfg = TemporalCfg {
            steps: 400,
            ..TemporalCfg::default()
        };
        let r = run_temporal(TemporalModel::Gated, 21, &cfg).unwrap();
        assert!(
            r.accuracy > 0.6,
            "a gated model should clearly beat chance after 400 steps: {r:?}"
        );
    }

    #[test]
    fn impossible_settings_are_refused_with_a_reason() {
        let bad = |c: TemporalCfg| run_temporal(TemporalModel::Gated, 1, &c).unwrap_err().0;
        assert!(bad(TemporalCfg {
            length: 1,
            ..quick()
        })
        .contains("length"));
        assert!(bad(TemporalCfg {
            classes: 1,
            ..quick()
        })
        .contains("classes"));
        assert!(bad(TemporalCfg {
            steps: 0,
            ..quick()
        })
        .contains("above 0"));
        assert!(bad(TemporalCfg {
            lr: F::NAN,
            ..quick()
        })
        .contains("learning rate"));
        assert!(bad(TemporalCfg {
            noise: -1.0,
            ..quick()
        })
        .contains("noise"));
    }

    #[test]
    fn a_huge_learning_rate_is_reported_as_instability_not_as_a_result() {
        // The guard clips or zeroes gradients, so the run may survive; what must hold is that the
        // table says honestly whether anything went non-finite and never averages it away.
        let cfg = TemporalCfg {
            lr: 1.0e6,
            steps: 80,
            ..quick()
        };
        let r = run_temporal(TemporalModel::Recurrent, 5, &cfg).unwrap();
        let table = temporal_table(std::slice::from_ref(&r));
        assert!(
            table.contains(if r.is_stable() { "| yes |" } else { "| NO |" }),
            "{table}"
        );
        let broken = TemporalResult {
            non_finite_losses: 2,
            ..r
        };
        assert!(temporal_table(&[broken]).contains("| NO |"));
    }

    #[test]
    fn the_model_names_round_trip() {
        for m in [
            TemporalModel::Recurrent,
            TemporalModel::Gated,
            TemporalModel::Transformer,
        ] {
            assert_eq!(TemporalModel::parse(m.name()), Some(m));
        }
        assert_eq!(TemporalModel::parse("lstm"), None);
    }
}
