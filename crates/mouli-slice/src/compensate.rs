//! Weight compensation after dropping neurons (the idea of Optimal Brain Surgeon, as used for
//! structured pruning by SlimGPT and, for sparsity, SparseGPT).
//!
//! Dropping neurons removes their share of a layer's output. Some of it can be given back by
//! nudging the down-projection weights of the neurons that stay: find a correction `Z` (one value
//! per kept neuron per output row) that makes `A_kept (W_kept + Z)^T` as close as possible to the
//! full layer output on calibration activations `A`.
//!
//! That is ridge regression of the removed neurons' contribution `D = A_removed W_removed^T` on
//! the kept activations. With far fewer calibration tokens `T` than kept neurons `k` (the usual
//! case here), the solve is done in token space, `Z = A_kept^T (A_kept A_kept^T + lambda I)^-1 D`,
//! which needs only a `T x T` system. The ridge strength is chosen by error on held-out prompts,
//! and the correction is used only if it lowers that held-out error; otherwise `Z` is zero.
//!
//! What this does not show: it makes the layer's output closer on the calibration prompts, and
//! the held-out check is on other prompts of the same set. It does not show the whole model is
//! better, it ignores that later layers see slightly changed inputs (each layer is fitted to the
//! original model's activations), and it needs enough calibration tokens to generalise.

use crate::{ActivationSink, SliceError};
use mouli_llm::quant::dot_f32;

/// Ridge strengths tried, as multiples of the mean diagonal of the token Gram matrix.
const RIDGE_GRID: [f64; 6] = [1e-4, 1e-3, 1e-2, 1e-1, 1.0, 10.0];
/// Every group whose id modulo this equals `HOLD_OUT_REMAINDER` is held out for choosing the ridge.
const HOLD_OUT_MODULUS: u32 = 4;
const HOLD_OUT_REMAINDER: u32 = 3;
/// Fewest tokens (training and held-out each) the fit will use.
pub const MIN_TOKENS_PER_SPLIT: usize = 8;

/// Feed-forward activations of every layer for a set of calibration prompts, kept in memory.
///
/// Memory is `tokens x d_ff x layers x 4` bytes, so the caller sets a token cap.
#[derive(Debug, Clone)]
pub struct Calibration {
    d_ff: usize,
    max_tokens: usize,
    layers: Vec<Vec<f32>>,
    /// The prompt each stored token came from.
    group: Vec<u32>,
    current_group: u32,
}

impl Calibration {
    /// An empty store for `layers` layers of `d_ff` neurons, holding at most `max_tokens` tokens.
    ///
    /// # Errors
    /// [`SliceError::BadParameter`] for a zero argument.
    pub fn new(layers: usize, d_ff: usize, max_tokens: usize) -> Result<Self, SliceError> {
        if layers == 0 || d_ff == 0 || max_tokens == 0 {
            return Err(SliceError::BadParameter("calibration size"));
        }
        Ok(Self {
            d_ff,
            max_tokens,
            layers: vec![Vec::new(); layers],
            group: Vec::new(),
            current_group: 0,
        })
    }

    /// Marks the start of the next prompt: later tokens belong to a new group.
    pub fn next_prompt(&mut self) {
        self.current_group = self.current_group.wrapping_add(1);
    }

    /// Tokens stored (every layer holds the same number once a pass is complete).
    #[must_use]
    pub fn tokens(&self) -> usize {
        self.group.len()
    }

    /// Neurons per layer.
    #[must_use]
    pub fn d_ff(&self) -> usize {
        self.d_ff
    }

    /// Number of layers.
    #[must_use]
    pub fn layers(&self) -> usize {
        self.layers.len()
    }

    /// One layer's activations, row-major `tokens x d_ff`.
    ///
    /// # Errors
    /// [`SliceError::LayerOutOfRange`] for a bad layer.
    pub fn layer(&self, layer: usize) -> Result<&[f32], SliceError> {
        self.layers
            .get(layer)
            .map(Vec::as_slice)
            .ok_or(SliceError::LayerOutOfRange {
                layer,
                layers: self.layers.len(),
            })
    }

    /// The prompt group of each stored token.
    #[must_use]
    pub fn groups(&self) -> &[u32] {
        &self.group
    }
}

impl ActivationSink for Calibration {
    fn begin_prompt(&mut self) {
        self.next_prompt();
    }

    fn observe(&mut self, layer: usize, activations: &[f32]) -> Result<(), SliceError> {
        if layer >= self.layers.len() {
            return Err(SliceError::LayerOutOfRange {
                layer,
                layers: self.layers.len(),
            });
        }
        if activations.len() != self.d_ff {
            return Err(SliceError::BadShape(
                "activation length does not equal d_ff",
            ));
        }
        if activations.iter().any(|v| !v.is_finite()) {
            return Err(SliceError::NonFinite("activation"));
        }
        // Layer 0 decides whether this token is kept; the other layers follow it.
        let stored = self.layers[layer].len() / self.d_ff;
        if layer == 0 {
            if self.group.len() >= self.max_tokens {
                return Ok(());
            }
            self.group.push(self.current_group);
        } else if stored >= self.group.len() {
            return Ok(());
        }
        self.layers[layer].extend_from_slice(activations);
        Ok(())
    }
}

/// Sends every observation to two sinks.
pub struct Tee<'a>(
    pub &'a mut dyn ActivationSink,
    pub &'a mut dyn ActivationSink,
);

impl ActivationSink for Tee<'_> {
    fn begin_prompt(&mut self) {
        self.0.begin_prompt();
        self.1.begin_prompt();
    }

    fn observe(&mut self, layer: usize, activations: &[f32]) -> Result<(), SliceError> {
        self.0.observe(layer, activations)?;
        self.1.observe(layer, activations)
    }
}

/// Computes the correction of every layer of a GGUF file for the plan `keep`, in parallel.
///
/// `workers` threads each hold one layer's working matrices at a time (roughly
/// `tokens^2 x 8 x 4` bytes plus `tokens x kept x 4`).
///
/// # Errors
/// A [`SliceError`] for an unreadable or unsupported file, a plan or calibration that does not
/// match the model, or a failure in any layer.
pub fn compensate_gguf(
    file: &[u8],
    calibration: &Calibration,
    keep: &[Vec<usize>],
    workers: usize,
) -> Result<Vec<Compensation>, SliceError> {
    use mouli_llm::gguf::parse;
    use mouli_llm::quant::dequantize_vec;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    let bad = |_: &dyn std::fmt::Debug| SliceError::BadShape("the model file could not be read");
    let gguf = parse(file).map_err(|e| bad(&e))?;
    let get = |suffix: &str| -> Result<usize, SliceError> {
        usize::try_from(gguf.arch_u32(suffix).map_err(|e| bad(&e))?)
            .map_err(|_| SliceError::BadParameter("metadata size"))
    };
    let (layers, d_model, d_ff) = (
        get("block_count")?,
        get("embedding_length")?,
        get("feed_forward_length")?,
    );
    if keep.len() != layers || calibration.layers() != layers || calibration.d_ff() != d_ff {
        return Err(SliceError::BadShape("plan, calibration and model disagree"));
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<Compensation, SliceError>>>> =
        Mutex::new((0..layers).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, layers) {
            scope.spawn(|| loop {
                let layer = next.fetch_add(1, Ordering::Relaxed);
                if layer >= layers {
                    break;
                }
                let outcome = (|| {
                    let name = format!("blk.{layer}.ffn_down.weight");
                    let info = gguf
                        .tensor(&name)
                        .ok_or(SliceError::BadShape("down tensor missing"))?;
                    let bytes = gguf.tensor_data(file, &name).map_err(|e| bad(&e))?;
                    let n = usize::try_from(info.n_elements)
                        .map_err(|_| SliceError::BadParameter("tensor size"))?;
                    let down = dequantize_vec(info.ty, bytes, n).map_err(|e| bad(&e))?;
                    compensate_layer(
                        calibration.layer(layer)?,
                        calibration.groups(),
                        d_ff,
                        &down,
                        d_model,
                        &keep[layer],
                    )
                })();
                if let Ok(mut guard) = results.lock() {
                    guard[layer] = Some(outcome);
                }
            });
        }
    });
    let collected = results
        .into_inner()
        .map_err(|_| SliceError::BadShape("a worker panicked"))?;
    collected
        .into_iter()
        .map(|r| r.unwrap_or(Err(SliceError::BadShape("a layer was not processed"))))
        .collect()
}

/// The outcome of compensating one layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Compensation {
    /// The correction to add to the kept neurons' down weights, row-major `d_model x k`
    /// (all zeros when it was not applied).
    pub correction: Vec<f32>,
    /// Whether the correction lowered the held-out error.
    pub applied: bool,
    /// The ridge strength chosen, as a multiple of the mean Gram diagonal.
    pub ridge: f64,
    /// Held-out squared error of the layer output with no correction, relative to the removed
    /// contribution's energy (1.0 means the removed neurons' output is entirely missing).
    pub error_before: f64,
    /// The same with the correction (equals `error_before` when not applied).
    pub error_after: f64,
}

/// In-place Cholesky factor (lower triangle) of a symmetric positive definite `n x n` matrix.
fn cholesky(a: &mut [f64], n: usize) -> bool {
    for j in 0..n {
        let mut d = a[j * n + j];
        for k in 0..j {
            d -= a[j * n + k] * a[j * n + k];
        }
        if d <= 0.0 || !d.is_finite() {
            return false;
        }
        let d = d.sqrt();
        a[j * n + j] = d;
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k];
            }
            a[i * n + j] = s / d;
        }
    }
    true
}

/// Solves `L L^T X = B` in place for `nrhs` right-hand sides stored row-major `n x nrhs`.
fn cholesky_solve(l: &[f64], n: usize, b: &mut [f64], nrhs: usize) {
    for i in 0..n {
        for k in 0..i {
            let f = l[i * n + k];
            if f != 0.0 {
                let (head, tail) = b.split_at_mut(i * nrhs);
                for (x, y) in tail[..nrhs].iter_mut().zip(&head[k * nrhs..(k + 1) * nrhs]) {
                    *x -= f * y;
                }
            }
        }
        let d = l[i * n + i];
        for x in &mut b[i * nrhs..(i + 1) * nrhs] {
            *x /= d;
        }
    }
    for i in (0..n).rev() {
        for k in i + 1..n {
            let f = l[k * n + i];
            if f != 0.0 {
                let (head, tail) = b.split_at_mut(k * nrhs);
                for (x, y) in head[i * nrhs..(i + 1) * nrhs].iter_mut().zip(&tail[..nrhs]) {
                    *x -= f * y;
                }
            }
        }
        let d = l[i * n + i];
        for x in &mut b[i * nrhs..(i + 1) * nrhs] {
            *x /= d;
        }
    }
}

/// Gram matrix `rows_a rows_b^T` of two row sets (each row `k` long), as f64.
fn cross_gram(a: &[f32], na: usize, b: &[f32], nb: usize, k: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; na * nb];
    for i in 0..na {
        let ra = &a[i * k..(i + 1) * k];
        for j in 0..nb {
            out[i * nb + j] = f64::from(dot_f32(ra, &b[j * k..(j + 1) * k]));
        }
    }
    out
}

fn rows_of(src: &[f32], width: usize, which: &[usize]) -> Vec<f32> {
    let mut out = Vec::with_capacity(which.len() * width);
    for &t in which {
        out.extend_from_slice(&src[t * width..(t + 1) * width]);
    }
    out
}

fn energy(x: &[f64]) -> f64 {
    x.iter().map(|v| v * v).sum()
}

/// Fits the correction for one layer.
///
/// `acts` is row-major `tokens x d_ff`, `down` is the layer's down matrix row-major
/// `d_model x d_ff`, `keep` lists the kept neurons (strictly ascending), and `group` gives the
/// prompt of each token (whole prompts are held out to choose the ridge).
///
/// # Errors
/// A [`SliceError`] for inconsistent sizes, bad `keep`, non-finite input, or too few tokens on
/// either side of the split (at least [`MIN_TOKENS_PER_SPLIT`] each).
pub fn compensate_layer(
    acts: &[f32],
    group: &[u32],
    d_ff: usize,
    down: &[f32],
    d_model: usize,
    keep: &[usize],
) -> Result<Compensation, SliceError> {
    let tokens = group.len();
    if d_ff == 0 || d_model == 0 || tokens == 0 {
        return Err(SliceError::EmptyInput("compensation input"));
    }
    if acts.len() != tokens * d_ff || down.len() != d_model * d_ff {
        return Err(SliceError::BadShape("activation or down matrix size"));
    }
    if keep.is_empty() || keep.windows(2).any(|w| w[0] >= w[1]) || keep.iter().any(|&j| j >= d_ff) {
        return Err(SliceError::BadParameter(
            "keep must be non-empty, ascending and in range",
        ));
    }
    if acts.iter().chain(down).any(|v| !v.is_finite()) {
        return Err(SliceError::NonFinite("compensation input"));
    }
    let k = keep.len();
    let mut kept = vec![false; d_ff];
    for &j in keep {
        kept[j] = true;
    }
    let removed: Vec<usize> = (0..d_ff).filter(|j| !kept[*j]).collect();
    let none = |before: f64| Compensation {
        correction: vec![0.0; d_model * k],
        applied: false,
        ridge: 0.0,
        error_before: before,
        error_after: before,
    };
    if removed.is_empty() {
        return Ok(none(0.0));
    }

    // D = removed contribution per token: tokens x d_model.
    let mut w_removed_t = vec![0.0f32; removed.len() * d_model];
    for (jr, &j) in removed.iter().enumerate() {
        for row in 0..d_model {
            w_removed_t[jr * d_model + row] = down[row * d_ff + j];
        }
    }
    let mut delta = vec![0.0f32; tokens * d_model];
    for t in 0..tokens {
        let out = &mut delta[t * d_model..(t + 1) * d_model];
        for (jr, &j) in removed.iter().enumerate() {
            let c = acts[t * d_ff + j];
            if c != 0.0 {
                for (o, w) in out
                    .iter_mut()
                    .zip(&w_removed_t[jr * d_model..(jr + 1) * d_model])
                {
                    *o += c * w;
                }
            }
        }
    }
    let delta: Vec<f64> = delta.iter().map(|v| f64::from(*v)).collect();

    // Kept activations: tokens x k.
    let mut a_kept = vec![0.0f32; tokens * k];
    for t in 0..tokens {
        for (c, &j) in keep.iter().enumerate() {
            a_kept[t * k + c] = acts[t * d_ff + j];
        }
    }

    let held = |t: usize| group[t] % HOLD_OUT_MODULUS == HOLD_OUT_REMAINDER;
    let train: Vec<usize> = (0..tokens).filter(|t| !held(*t)).collect();
    let val: Vec<usize> = (0..tokens).filter(|t| held(*t)).collect();
    if train.len() < MIN_TOKENS_PER_SPLIT || val.len() < MIN_TOKENS_PER_SPLIT {
        return Err(SliceError::BadParameter(
            "too few calibration tokens on one side of the train / held-out split",
        ));
    }
    let a_tr = rows_of(&a_kept, k, &train);
    let a_va = rows_of(&a_kept, k, &val);
    let d_tr: Vec<f64> = train
        .iter()
        .flat_map(|&t| delta[t * d_model..(t + 1) * d_model].iter().copied())
        .collect();
    let d_va: Vec<f64> = val
        .iter()
        .flat_map(|&t| delta[t * d_model..(t + 1) * d_model].iter().copied())
        .collect();
    let baseline = energy(&d_va);
    if baseline <= 0.0 {
        return Ok(none(0.0));
    }

    let (ntr, nva) = (train.len(), val.len());
    let k_tt = cross_gram(&a_tr, ntr, &a_tr, ntr, k);
    let k_vt = cross_gram(&a_va, nva, &a_tr, ntr, k);
    let scale = (0..ntr).map(|i| k_tt[i * ntr + i]).sum::<f64>() / ntr as f64;
    if scale <= 0.0 {
        return Ok(none(1.0));
    }

    let mut best: Option<(f64, f64)> = None; // (held-out error, ridge multiple)
    for &c in &RIDGE_GRID {
        let mut m = k_tt.clone();
        for i in 0..ntr {
            m[i * ntr + i] += c * scale;
        }
        if !cholesky(&mut m, ntr) {
            continue;
        }
        let mut alpha = d_tr.clone();
        cholesky_solve(&m, ntr, &mut alpha, d_model);
        // Held-out prediction: K_vt alpha, compared with the true removed contribution.
        let mut err = 0.0;
        for i in 0..nva {
            let mut pred = vec![0.0f64; d_model];
            for j in 0..ntr {
                let f = k_vt[i * ntr + j];
                for (p, a) in pred.iter_mut().zip(&alpha[j * d_model..(j + 1) * d_model]) {
                    *p += f * a;
                }
            }
            err += pred
                .iter()
                .zip(&d_va[i * d_model..(i + 1) * d_model])
                .map(|(p, d)| (d - p) * (d - p))
                .sum::<f64>();
        }
        if best.is_none_or(|(e, _)| err < e) {
            best = Some((err, c));
        }
    }
    let Some((best_err, ridge)) = best else {
        return Ok(none(1.0));
    };
    let error_before = 1.0;
    let error_after = best_err / baseline;
    if best_err >= baseline {
        return Ok(Compensation {
            correction: vec![0.0; d_model * k],
            applied: false,
            ridge,
            error_before,
            error_after: error_before,
        });
    }

    // Refit on every token with the chosen ridge, then expand to neuron space.
    let all: Vec<usize> = (0..tokens).collect();
    let _ = all;
    let k_all = cross_gram(&a_kept, tokens, &a_kept, tokens, k);
    let scale_all = (0..tokens).map(|i| k_all[i * tokens + i]).sum::<f64>() / tokens as f64;
    let mut m = k_all;
    for i in 0..tokens {
        m[i * tokens + i] += ridge * scale_all;
    }
    if !cholesky(&mut m, tokens) {
        return Ok(Compensation {
            correction: vec![0.0; d_model * k],
            applied: false,
            ridge,
            error_before,
            error_after: error_before,
        });
    }
    let mut alpha = delta;
    cholesky_solve(&m, tokens, &mut alpha, d_model);
    // Z^T (k x d_model) = A_kept^T alpha.
    let mut zt = vec![0.0f64; k * d_model];
    for t in 0..tokens {
        let a = &alpha[t * d_model..(t + 1) * d_model];
        for c in 0..k {
            let f = f64::from(a_kept[t * k + c]);
            if f != 0.0 {
                for (z, x) in zt[c * d_model..(c + 1) * d_model].iter_mut().zip(a) {
                    *z += f * x;
                }
            }
        }
    }
    let mut correction = vec![0.0f32; d_model * k];
    for c in 0..k {
        for row in 0..d_model {
            correction[row * k + c] = zt[c * d_model + row] as f32;
        }
    }
    Ok(Compensation {
        correction,
        applied: true,
        ridge,
        error_before,
        error_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32
        }
    }

    fn groups(tokens: usize, per_prompt: usize) -> Vec<u32> {
        (0..tokens).map(|t| (t / per_prompt) as u32).collect()
    }

    #[test]
    fn cholesky_solves_a_small_system() {
        // A = [[4,2],[2,3]], B = [[2],[1]] -> X = A^-1 B = [[0.5],[0.0]].
        let mut a = vec![4.0, 2.0, 2.0, 3.0];
        assert!(cholesky(&mut a, 2));
        let mut b = vec![2.0, 1.0];
        cholesky_solve(&a, 2, &mut b, 1);
        assert!((b[0] - 0.5).abs() < 1e-12 && b[1].abs() < 1e-12, "{b:?}");
        let mut bad = vec![1.0, 2.0, 2.0, 1.0];
        assert!(!cholesky(&mut bad, 2));
        // Two right-hand sides at once.
        let mut a = vec![4.0, 2.0, 2.0, 3.0];
        assert!(cholesky(&mut a, 2));
        let mut b = vec![2.0, 1.0, 1.0, 0.0];
        cholesky_solve(&a, 2, &mut b, 2);
        assert!(
            (b[0] - 0.5).abs() < 1e-12 && (b[1] - 0.375).abs() < 1e-12,
            "{b:?}"
        );
    }

    /// Activations where neuron `d_ff - 1` is a copy of neuron 0 (plus a little noise elsewhere).
    fn data(tokens: usize, d_ff: usize, copy: bool) -> Vec<f32> {
        let mut rng = Rng(42);
        let mut a = vec![0.0f32; tokens * d_ff];
        for t in 0..tokens {
            for j in 0..d_ff {
                a[t * d_ff + j] = rng.next();
            }
            if copy {
                a[t * d_ff + d_ff - 1] = a[t * d_ff];
            }
        }
        a
    }

    fn down_matrix(d_model: usize, d_ff: usize) -> Vec<f32> {
        let mut rng = Rng(7);
        (0..d_model * d_ff).map(|_| rng.next()).collect()
    }

    /// The layer output for the given neurons and down weights.
    fn output(
        a: &[f32],
        d_ff: usize,
        down: &[f32],
        d_model: usize,
        cols: &[usize],
        t: usize,
    ) -> Vec<f64> {
        (0..d_model)
            .map(|row| {
                cols.iter()
                    .enumerate()
                    .map(|(c, &j)| {
                        f64::from(a[t * d_ff + j]) * f64::from(down[row * cols.len() + c])
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn a_dropped_neuron_that_copies_a_kept_one_is_fully_recovered() {
        let (tokens, d_ff, d_model) = (80, 12, 5);
        let a = data(tokens, d_ff, true);
        let down = down_matrix(d_model, d_ff);
        let keep: Vec<usize> = (0..d_ff - 1).collect(); // drop the copy
        let c = compensate_layer(&a, &groups(tokens, 10), d_ff, &down, d_model, &keep).unwrap();
        assert!(c.applied, "{c:?}");
        assert!(c.error_after < 1e-3, "held-out error {}", c.error_after);
        // Check the algebra on every token: kept neurons with corrected weights reproduce the
        // full output far better than the kept neurons with their original weights.
        let k = keep.len();
        let mut corrected = Vec::with_capacity(d_model * k);
        let mut plain_kept = Vec::with_capacity(d_model * k);
        for row in 0..d_model {
            for (ci, &j) in keep.iter().enumerate() {
                plain_kept.push(down[row * d_ff + j]);
                corrected.push(down[row * d_ff + j] + c.correction[row * k + ci]);
            }
        }
        let all: Vec<usize> = (0..d_ff).collect();
        for t in 0..tokens {
            let full = output(&a, d_ff, &down, d_model, &all, t);
            let fixed = output(&a, d_ff, &corrected, d_model, &keep, t);
            let plain = output(&a, d_ff, &plain_kept, d_model, &keep, t);
            let err_fixed: f64 = full
                .iter()
                .zip(&fixed)
                .map(|(x, y)| (x - y) * (x - y))
                .sum();
            let err_plain: f64 = full
                .iter()
                .zip(&plain)
                .map(|(x, y)| (x - y) * (x - y))
                .sum();
            assert!(
                err_fixed < err_plain * 0.01 + 1e-6,
                "token {t}: {err_fixed} vs {err_plain}"
            );
        }
    }

    #[test]
    fn independent_neurons_cannot_be_compensated_and_the_correction_is_refused() {
        let (tokens, d_ff, d_model) = (80, 12, 5);
        let a = data(tokens, d_ff, false);
        let down = down_matrix(d_model, d_ff);
        let keep: Vec<usize> = (0..6).collect();
        let c = compensate_layer(&a, &groups(tokens, 10), d_ff, &down, d_model, &keep).unwrap();
        // Random independent activations carry no information about the dropped neurons.
        assert!(!c.applied, "{c:?}");
        assert!(c.correction.iter().all(|v| *v == 0.0));
        assert!((c.error_after - c.error_before).abs() < 1e-12);
    }

    #[test]
    fn keeping_everything_needs_no_correction() {
        let (tokens, d_ff, d_model) = (40, 6, 3);
        let a = data(tokens, d_ff, false);
        let down = down_matrix(d_model, d_ff);
        let keep: Vec<usize> = (0..d_ff).collect();
        let c = compensate_layer(&a, &groups(tokens, 10), d_ff, &down, d_model, &keep).unwrap();
        assert!(!c.applied && c.correction.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn bad_input_is_refused() {
        let (tokens, d_ff, d_model) = (40, 6, 3);
        let a = data(tokens, d_ff, false);
        let down = down_matrix(d_model, d_ff);
        let g = groups(tokens, 10);
        assert!(compensate_layer(&a, &g, d_ff, &down, d_model, &[]).is_err());
        assert!(compensate_layer(&a, &g, d_ff, &down, d_model, &[2, 1]).is_err());
        assert!(compensate_layer(&a, &g, d_ff, &down, d_model, &[9]).is_err());
        assert!(compensate_layer(&a[1..], &g, d_ff, &down, d_model, &[0]).is_err());
        assert!(compensate_layer(&a, &g, d_ff, &down[1..], d_model, &[0]).is_err());
        assert!(compensate_layer(&a, &[], d_ff, &down, d_model, &[0]).is_err());
        let mut nan = a.clone();
        nan[3] = f32::NAN;
        assert!(compensate_layer(&nan, &g, d_ff, &down, d_model, &[0]).is_err());
        // Too few prompts to hold some out.
        assert!(compensate_layer(&a[..6 * 4], &groups(4, 10), d_ff, &down, d_model, &[0]).is_err());
    }

    #[test]
    fn calibration_stores_tokens_per_layer_and_respects_its_cap() {
        let mut c = Calibration::new(2, 3, 2).unwrap();
        for _ in 0..2 {
            c.next_prompt();
            for t in 0..2 {
                c.observe(0, &[t as f32, 1.0, 2.0]).unwrap();
                c.observe(1, &[10.0 + t as f32, 1.0, 2.0]).unwrap();
            }
        }
        // Cap of 2 tokens: the third and fourth are dropped on every layer alike.
        assert_eq!(c.tokens(), 2);
        assert_eq!(c.layer(0).unwrap().len(), 6);
        assert_eq!(c.layer(1).unwrap().len(), 6);
        assert_eq!(c.layer(1).unwrap()[0], 10.0);
        assert_eq!(c.groups(), &[1, 1]);
        assert!(c.observe(0, &[1.0]).is_err());
        assert!(c.observe(5, &[1.0, 2.0, 3.0]).is_err());
        assert!(c.observe(0, &[f32::NAN, 0.0, 0.0]).is_err());
        assert!(c.layer(2).is_err());
        assert!(Calibration::new(0, 3, 2).is_err());
        assert!(Calibration::new(2, 3, 0).is_err());
    }

    fn tiny_model(d_model: usize, d_ff: usize) -> Vec<u8> {
        use mouli_llm::gguf::{GgmlType, GgufWriter, Value};
        let mut w = GgufWriter::new();
        w.add_metadata("general.architecture", Value::String("llama".into()));
        w.add_metadata("llama.block_count", Value::U32(2));
        w.add_metadata("llama.embedding_length", Value::U32(d_model as u32));
        w.add_metadata("llama.feed_forward_length", Value::U32(d_ff as u32));
        let bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        for layer in 0..2u64 {
            let mut rng = Rng(100 + layer);
            for (suffix, dims) in [
                ("ffn_gate.weight", [d_model as u64, d_ff as u64]),
                ("ffn_up.weight", [d_model as u64, d_ff as u64]),
                ("ffn_down.weight", [d_ff as u64, d_model as u64]),
            ] {
                let v: Vec<f32> = (0..d_model * d_ff).map(|_| rng.next()).collect();
                w.add_tensor(
                    &format!("blk.{layer}.{suffix}"),
                    &dims,
                    GgmlType::F32,
                    bytes(&v),
                )
                .unwrap();
            }
        }
        w.finish().unwrap()
    }

    #[test]
    fn compensating_a_whole_file_in_parallel_recovers_copied_neurons_in_every_layer() {
        let (d_model, d_ff) = (32, 64);
        let file = tiny_model(d_model, d_ff);
        let mut cal = Calibration::new(2, d_ff, 400).unwrap();
        let mut rng = Rng(5);
        for _ in 0..10 {
            cal.begin_prompt();
            for _ in 0..16 {
                for layer in 0..2 {
                    let mut a: Vec<f32> = (0..d_ff).map(|_| rng.next()).collect();
                    // The upper half of the neurons repeats the lower half.
                    for j in 0..d_ff / 2 {
                        a[j + d_ff / 2] = a[j];
                    }
                    cal.observe(layer, &a).unwrap();
                }
            }
        }
        assert_eq!(cal.tokens(), 160);
        let keep: Vec<Vec<usize>> = vec![(0..32).collect(), (0..32).collect()];
        for workers in [1, 2] {
            let out = compensate_gguf(&file, &cal, &keep, workers).unwrap();
            assert_eq!(out.len(), 2);
            for c in &out {
                assert!(c.applied, "{c:?}");
                assert!(c.error_after < 0.01, "held-out error {}", c.error_after);
                assert_eq!(c.correction.len(), d_model * 32);
            }
        }
        // The plan, calibration and model must agree.
        assert!(compensate_gguf(&file, &cal, &keep[..1], 1).is_err());
        assert!(compensate_gguf(b"not a gguf", &cal, &keep, 1).is_err());
    }

    #[test]
    fn a_tee_feeds_both_sinks() {
        let mut a = Calibration::new(1, 2, 10).unwrap();
        let mut b = Calibration::new(1, 2, 10).unwrap();
        Tee(&mut a, &mut b).observe(0, &[1.0, 2.0]).unwrap();
        assert_eq!(a.tokens(), 1);
        assert_eq!(b.tokens(), 1);
    }
}
