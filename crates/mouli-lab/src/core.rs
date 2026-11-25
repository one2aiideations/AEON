//! Shared pieces: the learner interface, cross-entropy, and a gradient checker.

use crate::linalg::Mat;
use crate::rng::Rng;
use crate::F;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Encode,
    Consolidate,
    Generate,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EvalNorm {
    FixedEma,
    TransductiveBatch,
}

pub fn mode_multiplier(m: Mode) -> F {
    match m {
        Mode::Encode => 1.0,
        Mode::Consolidate => 0.5,
        Mode::Generate => 0.5,
    }
}

pub trait Learner {
    fn n_params(&self) -> usize;
    fn step(&mut self, x: &Mat, y: &[u8], mode: Mode);
    fn logits(&mut self, x: &Mat, eval: EvalNorm) -> Mat;
}

#[derive(Clone, Debug)]
pub struct GradCheck {
    pub name: String,
    pub max_abs: F,
    pub max_rel: F,
    pub pass: bool,
}

/// Mean-batch cross-entropy. d(loss)/d(logits) = (p - onehot)/B is computed ONCE here; no
/// downstream backward divides by B again.
pub fn cross_entropy(logits: &Mat, y: &[u8]) -> (F, Mat) {
    let b = logits.r.max(1);
    let c = logits.c;
    let p = crate::linalg::softmax_rows(logits);
    let mut loss = 0.0;
    let mut dl = Mat::zeros(logits.r, c);
    for bi in 0..logits.r {
        let yi = y[bi] as usize;
        loss -= p.d[bi * c + yi].max(1e-12).ln();
        for ci in 0..c {
            let t = if ci == yi { 1.0 } else { 0.0 };
            dl.d[bi * c + ci] = (p.d[bi * c + ci] - t) / b as F;
        }
    }
    (loss / b as F, dl)
}

/// Finite-difference check (central differences, h = 1e-5) of a flat parameter slice.
///
/// `loss` must return the objective at the CURRENT contents of `p`. In safe Rust a closure cannot
/// borrow `p` while this function also holds it exclusively, so the model checks in this crate
/// use the clone-and-perturb helpers instead; this function is kept for callers that can sync the
/// buffer themselves. Passes when max_rel < 1e-6 or max_abs < 1e-9.
pub fn gradcheck_param(
    name: &str,
    p: &mut [F],
    analytic: &[F],
    loss: &mut dyn FnMut() -> F,
    n: usize,
    rng: &mut Rng,
) -> GradCheck {
    let h = 1e-5;
    let len = p.len();
    if len == 0 || analytic.len() != len {
        return GradCheck {
            name: name.to_string(),
            max_abs: f64::NAN,
            max_rel: f64::NAN,
            pass: false,
        };
    }
    let mut idxs: Vec<usize> = (0..len).collect();
    rng.shuffle(&mut idxs);
    idxs.truncate(n.min(len));
    let mut max_abs = 0.0f64;
    let mut max_rel = 0.0f64;
    for &i in &idxs {
        let orig = p[i];
        p[i] = orig + h;
        let lp = loss();
        p[i] = orig - h;
        let lm = loss();
        p[i] = orig;
        let num = (lp - lm) / (2.0 * h);
        let a = analytic[i];
        let abs = (num - a).abs();
        let rel = abs / num.abs().max(a.abs()).max(1e-12);
        if abs > max_abs {
            max_abs = abs;
        }
        if rel > max_rel {
            max_rel = rel;
        }
    }
    let pass = max_rel < 1e-6 || max_abs < 1e-9;
    GradCheck {
        name: name.to_string(),
        max_abs,
        max_rel,
        pass,
    }
}
