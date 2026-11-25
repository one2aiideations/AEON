//! Baselines: a plain MLP with Adam and online EWC.

use crate::core::{cross_entropy, mode_multiplier, EvalNorm, GradCheck, Learner, Mode};
use crate::data::Dataset;
use crate::linalg::{softmax_rows, Mat};
use crate::rng::Rng;
use crate::F;

#[derive(Clone)]
pub struct AdamState {
    pub m: Vec<F>,
    pub v: Vec<F>,
    pub t: u64,
    pub lr: F,
    pub beta1: F,
    pub beta2: F,
    pub eps: F,
}

impl AdamState {
    pub fn new(n: usize, lr: F) -> Self {
        AdamState {
            m: vec![0.0; n],
            v: vec![0.0; n],
            t: 0,
            lr,
            beta1: 0.9,
            beta2: 0.999,
            eps: 1e-8,
        }
    }

    pub fn update(&mut self, p: &mut [F], g: &[F]) {
        self.t += 1;
        let bc1 = 1.0 - self.beta1.powi(self.t as i32);
        let bc2 = 1.0 - self.beta2.powi(self.t as i32);
        for i in 0..p.len() {
            self.m[i] = self.beta1 * self.m[i] + (1.0 - self.beta1) * g[i];
            self.v[i] = self.beta2 * self.v[i] + (1.0 - self.beta2) * g[i] * g[i];
            let mh = self.m[i] / bc1;
            let vh = self.v[i] / bc2;
            p[i] -= self.lr * mh / (vh.sqrt() + self.eps);
        }
    }
}

#[derive(Clone)]
pub struct EwcState {
    pub lambda: F,
    pub fisher: Vec<F>,
    pub anchor: Vec<F>,
}

#[derive(Clone)]
pub struct MlpLearner {
    pub in_dim: usize,
    pub hidden: usize,
    pub n_classes: usize,
    pub w_in: Mat,
    pub b_in: Vec<F>,
    pub w_out: Mat,
    pub b_out: Vec<F>,
    pub hidden_lr: F,
    pub readout_lr: F,
    pub ewc: Option<EwcState>,
}

impl MlpLearner {
    pub fn new(in_dim: usize, hidden: usize, n_classes: usize, seed: u64, hidden_lr: F) -> Self {
        let mut rng = Rng::new(seed);
        let mut w_in = Mat::zeros(hidden, in_dim);
        let s = (2.0 / in_dim as F).sqrt();
        for v in w_in.d.iter_mut() {
            *v = rng.normal() * s;
        }
        let mut w_out = Mat::zeros(n_classes, hidden);
        let s2 = (1.0 / hidden as F).sqrt();
        for v in w_out.d.iter_mut() {
            *v = rng.normal() * s2;
        }
        MlpLearner {
            in_dim,
            hidden,
            n_classes,
            w_in,
            b_in: vec![0.0; hidden],
            w_out,
            b_out: vec![0.0; n_classes],
            hidden_lr,
            readout_lr: 0.05,
            ewc: None,
        }
    }

    fn param_vec(&self) -> Vec<F> {
        let mut v = Vec::with_capacity(self.n_params_ewc());
        v.extend_from_slice(&self.w_in.d);
        v.extend_from_slice(&self.b_in);
        v.extend_from_slice(&self.w_out.d);
        v.extend_from_slice(&self.b_out);
        v
    }

    fn n_params_ewc(&self) -> usize {
        self.w_in.d.len() + self.b_in.len() + self.w_out.d.len() + self.b_out.len()
    }

    pub fn enable_ewc(&mut self, lambda: F) {
        let n = self.n_params_ewc();
        self.ewc = Some(EwcState {
            lambda,
            fisher: vec![0.0; n],
            anchor: vec![0.0; n],
        });
    }

    fn forward_cache(&self, x: &Mat) -> (Mat, Mat) {
        let mut z = x.matmul_t(&self.w_in);
        for i in 0..z.r {
            for j in 0..z.c {
                z.d[i * z.c + j] += self.b_in[j];
            }
        }
        let mut h = Mat::zeros(z.r, z.c);
        for i in 0..z.d.len() {
            h.d[i] = z.d[i].max(0.0);
        }
        let mut logits = h.matmul_t(&self.w_out);
        for i in 0..logits.r {
            for j in 0..logits.c {
                logits.d[i * logits.c + j] += self.b_out[j];
            }
        }
        (h, logits)
    }

    /// Loss and the concatenated gradients (w_in, b_in, w_out, b_out), mean over the batch.
    fn backward_grads(&self, x: &Mat, y: &[u8]) -> (F, Vec<F>) {
        let (h, logits) = self.forward_cache(x);
        let (loss, dlogits) = cross_entropy(&logits, y);
        let dh = dlogits.matmul(&self.w_out);
        let dw_out = dlogits.t_matmul(&h);
        let db_out = dlogits.colsum();
        let mut dz = Mat::zeros(dh.r, dh.c);
        // relu(z) > 0 exactly when z > 0, so the activation mask can be read from h.
        for i in 0..dz.d.len() {
            dz.d[i] = if h.d[i] > 0.0 { dh.d[i] } else { 0.0 };
        }
        let dw_in = dz.t_matmul(x);
        let db_in = dz.colsum();
        let mut g = Vec::with_capacity(self.n_params_ewc());
        g.extend_from_slice(&dw_in.d);
        g.extend_from_slice(&db_in);
        g.extend_from_slice(&dw_out.d);
        g.extend_from_slice(&db_out);
        (loss, g)
    }

    /// Call AFTER each task: accumulates the diagonal Fisher using labels SAMPLED from the model,
    /// gamma = 1 (Fisher sums across tasks), and re-anchors at the latest parameters.
    pub fn ewc_end_task(&mut self, data: &Dataset, rng: &mut Rng) {
        if self.ewc.is_none() {
            self.enable_ewc(100.0);
        }
        let mut fisher = vec![0.0; self.n_params_ewc()];
        for i in 0..data.y.len() {
            let xi = data.subset(&[i]);
            let (_h, logits) = self.forward_cache(&xi.x);
            let probs = softmax_rows(&logits);
            let row = &probs.d[..self.n_classes];
            let cls = rng.categorical(row);
            let (_l, g) = self.backward_grads(&xi.x, &[cls as u8]);
            for j in 0..fisher.len() {
                fisher[j] += g[j] * g[j];
            }
        }
        for f in fisher.iter_mut() {
            *f /= data.y.len().max(1) as F;
        }
        let anchor = self.param_vec();
        if let Some(e) = &mut self.ewc {
            for j in 0..e.fisher.len() {
                e.fisher[j] += fisher[j];
            }
            e.anchor = anchor;
        }
    }
}

impl Learner for MlpLearner {
    fn n_params(&self) -> usize {
        self.n_params_ewc()
    }

    fn step(&mut self, x: &Mat, y: &[u8], mode: Mode) {
        let (_loss, mut g) = self.backward_grads(x, y);
        let mult = mode_multiplier(mode);
        let lr = self.hidden_lr;
        // Online EWC: add min(lambda * F, 0.5 / lr) * (theta - anchor) to the gradient.
        if let Some(e) = &self.ewc {
            let pv = self.param_vec();
            let cap = 0.5 / lr.max(1e-8);
            for j in 0..g.len() {
                let pen = (e.lambda * e.fisher[j]).min(cap) * (pv[j] - e.anchor[j]);
                g[j] += pen;
            }
        }
        let n_wi = self.w_in.d.len();
        let n_bi = self.b_in.len();
        let n_wo = self.w_out.d.len();
        let mut o = 0usize;
        for i in 0..n_wi {
            self.w_in.d[i] -= lr * mult * g[o];
            o += 1;
        }
        for i in 0..n_bi {
            self.b_in[i] -= lr * mult * g[o];
            o += 1;
        }
        for i in 0..n_wo {
            self.w_out.d[i] -= self.readout_lr * g[o];
            o += 1;
        }
        for i in 0..self.b_out.len() {
            self.b_out[i] -= self.readout_lr * g[o];
            o += 1;
        }
    }

    fn logits(&mut self, x: &Mat, _eval: EvalNorm) -> Mat {
        self.forward_cache(x).1
    }
}

/// Clone-and-perturb finite differences for every MLP array.
pub fn gradcheck_mlp(
    model: &MlpLearner,
    x: &Mat,
    y: &[u8],
    n: usize,
    rng: &mut Rng,
) -> Vec<GradCheck> {
    #[derive(Clone, Copy)]
    enum A {
        WIn,
        BIn,
        WOut,
        BOut,
    }
    fn name(a: A) -> &'static str {
        match a {
            A::WIn => "w_in",
            A::BIn => "b_in",
            A::WOut => "w_out",
            A::BOut => "b_out",
        }
    }
    fn len(m: &MlpLearner, a: A) -> usize {
        match a {
            A::WIn => m.w_in.d.len(),
            A::BIn => m.b_in.len(),
            A::WOut => m.w_out.d.len(),
            A::BOut => m.b_out.len(),
        }
    }
    fn mut_slice(m: &mut MlpLearner, a: A) -> &mut Vec<F> {
        match a {
            A::WIn => &mut m.w_in.d,
            A::BIn => &mut m.b_in,
            A::WOut => &mut m.w_out.d,
            A::BOut => &mut m.b_out,
        }
    }
    let (_l, analytic) = model.backward_grads(x, y);
    let mut outs = Vec::new();
    let (n_wi, n_bi, n_wo) = (model.w_in.d.len(), model.b_in.len(), model.w_out.d.len());
    for (a, slice) in [
        (A::WIn, &analytic[..n_wi]),
        (A::BIn, &analytic[n_wi..n_wi + n_bi]),
        (A::WOut, &analytic[n_wi + n_bi..n_wi + n_bi + n_wo]),
        (A::BOut, &analytic[n_wi + n_bi + n_wo..]),
    ] {
        let base = model.clone();
        let l = len(model, a);
        let mut idxs: Vec<usize> = (0..l).collect();
        rng.shuffle(&mut idxs);
        idxs.truncate(n.min(l));
        let h = 1e-5;
        let mut max_abs = 0.0f64;
        let mut max_rel = 0.0f64;
        for &i in &idxs {
            let mut m1 = base.clone();
            mut_slice(&mut m1, a)[i] += h;
            let (_h1, lg1) = m1.forward_cache(x);
            let lp = cross_entropy(&lg1, y).0;
            let mut m2 = base.clone();
            mut_slice(&mut m2, a)[i] -= h;
            let (_h2, lg2) = m2.forward_cache(x);
            let lm = cross_entropy(&lg2, y).0;
            let num = (lp - lm) / (2.0 * h);
            let av = slice[i];
            let abs = (num - av).abs();
            let rel = abs / num.abs().max(av.abs()).max(1e-12);
            if abs > max_abs {
                max_abs = abs;
            }
            if rel > max_rel {
                max_rel = rel;
            }
        }
        let pass = max_rel < 1e-6 || max_abs < 1e-9;
        outs.push(GradCheck {
            name: format!("mlp:{}", name(a)),
            max_abs,
            max_rel,
            pass,
        });
    }
    outs
}
