//! The Aetheron dendritic layer for classification: branch sub-networks, a fixed mixing matrix,
//! batch normalisation with an EMA path, a straight-through gate, attention across units, and a
//! linear readout. Hand-written forward and backward passes, checked by finite differences.

use crate::core::{cross_entropy, mode_multiplier, EvalNorm, GradCheck, Learner, Mode};
use crate::linalg::{softmax_rows, Mat};
use crate::rng::Rng;
use crate::F;

#[derive(Clone)]
pub struct AetheronMlCfg {
    pub in_dim: usize,
    pub units: usize,
    pub branches: usize,
    pub hidden: usize,
    pub n_classes: usize,
    pub dk: usize,
    pub k_relax: usize,
    pub alpha: F,
    pub eps: F,
    pub bn_eps: F,
    pub ema_momentum: F,
    pub homeostatic: bool,
    /// Attention across units. When false the attention matrix is the identity (each unit keeps
    /// its own gated output) and the query and key weights receive no gradient.
    pub attention: bool,
    pub eta_init: F,
    pub eta_min: F,
    pub eta_max: F,
    pub eta_var_target: F,
    pub attn_lr: F,
    pub readout_lr: F,
    pub window: usize,
}

impl Default for AetheronMlCfg {
    fn default() -> Self {
        AetheronMlCfg {
            in_dim: 784,
            units: 16,
            branches: 3,
            hidden: 10,
            n_classes: 10,
            dk: 6,
            k_relax: 3,
            alpha: 0.3,
            eps: 0.15,
            bn_eps: 1e-6,
            ema_momentum: 0.05,
            homeostatic: true,
            attention: true,
            eta_init: 0.05,
            eta_min: 1e-4,
            eta_max: 0.5,
            eta_var_target: 1e-4,
            attn_lr: 0.02,
            readout_lr: 0.05,
            window: 5,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BnMode {
    Train,
    FixedEma,
    Transductive,
}

impl BnMode {
    fn from_eval(e: EvalNorm) -> Self {
        match e {
            EvalNorm::FixedEma => BnMode::FixedEma,
            EvalNorm::TransductiveBatch => BnMode::Transductive,
        }
    }
}

#[derive(Clone)]
pub struct AetheronMlUnit {
    pub cfg: AetheronMlCfg,
    pub w1: Mat,
    pub b1: Vec<F>,
    pub w2: Mat,
    pub b2: Vec<F>,
    pub w2_sign: Vec<F>,
    pub wq: Mat,
    pub wk: Mat,
    pub theta: Vec<F>,
    pub c_mix: Mat,
    pub ema_mean: Vec<F>,
    pub ema_var: Vec<F>,
    pub ema_rate: Vec<F>,
    pub eta: Vec<F>,
    pub grad_window: Vec<Vec<F>>,
}

#[derive(Clone)]
struct Cache {
    z1: Mat,
    h: Mat,
    out: Mat,
    mu: Vec<F>,
    sd: Vec<F>,
    normed: Mat,
    act: Mat,
    gated: Mat,
    q: Mat,
    k: Mat,
    attn: Mat,
}

struct LayerGrads {
    dw1: Mat,
    db1: Vec<F>,
    dw2: Mat,
    db2: Vec<F>,
    dwq: Mat,
    dwk: Mat,
    unit_norms: Vec<F>,
}

impl AetheronMlUnit {
    pub fn new(cfg: AetheronMlCfg, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let n = cfg.units;
        let db = cfg.branches;
        let h = cfg.hidden;
        let rows1 = n * db * h;
        let mut w1 = Mat::zeros(rows1, cfg.in_dim);
        let s1 = (2.0 / cfg.in_dim as F).sqrt();
        for v in w1.d.iter_mut() {
            *v = rng.normal() * s1;
        }
        let b1 = vec![0.0; rows1];
        let mut w2 = Mat::zeros(n * db, h);
        let s2 = (1.0 / h as F).sqrt();
        for v in w2.d.iter_mut() {
            *v = rng.normal() * s2;
        }
        let b2 = vec![0.0; n * db];
        let w2_sign: Vec<F> =
            w2.d.iter()
                .map(|v| if *v >= 0.0 { 1.0 } else { -1.0 })
                .collect();
        let mut wq = Mat::zeros(db, cfg.dk);
        let mut wk = Mat::zeros(db, cfg.dk);
        for v in wq.d.iter_mut() {
            *v = rng.normal() * 0.3;
        }
        for v in wk.d.iter_mut() {
            *v = rng.normal() * 0.3;
        }
        // A: fixed random symmetric matrix with spectral radius 0.5 (power iteration).
        let mut a = Mat::zeros(n, n);
        for i in 0..n {
            for j in i..n {
                a.d[i * n + j] = rng.normal();
                a.d[j * n + i] = a.d[i * n + j];
            }
        }
        let mut v = vec![1.0 / (n as F).sqrt(); n];
        for _ in 0..50 {
            let mut w = vec![0.0; n];
            for i in 0..n {
                for j in 0..n {
                    w[i] += a.d[i * n + j] * v[j];
                }
            }
            let norm: F = w.iter().map(|x| x * x).sum::<F>().sqrt();
            if norm > 1e-12 {
                for x in w.iter_mut() {
                    *x /= norm;
                }
                v = w;
            } else {
                break;
            }
        }
        let mut av = vec![0.0; n];
        for i in 0..n {
            for j in 0..n {
                av[i] += a.d[i * n + j] * v[j];
            }
        }
        let mut lambda = 0.0;
        for i in 0..n {
            lambda += v[i] * av[i];
        }
        if lambda.abs() > 1e-12 {
            let s = 0.5 / lambda.abs();
            for x in a.d.iter_mut() {
                *x *= s;
            }
        }
        // P = (1-alpha) I + alpha A ; C = P^k + alpha * sum_{j<k} P^j
        let mut id = vec![0.0; n * n];
        for i in 0..n {
            id[i * n + i] = 1.0;
        }
        let id = Mat::from_vec(n, n, id);
        let mut pd = vec![0.0; n * n];
        for i in 0..n {
            for j in 0..n {
                pd[i * n + j] =
                    (if i == j { 1.0 - cfg.alpha } else { 0.0 }) + cfg.alpha * a.d[i * n + j];
            }
        }
        let p = Mat::from_vec(n, n, pd);
        let mut c = Mat::zeros(n, n);
        let mut p_pow = id;
        for _ in 0..cfg.k_relax {
            for i in 0..c.d.len() {
                c.d[i] += cfg.alpha * p_pow.d[i];
            }
            p_pow = p_pow.matmul(&p);
        }
        for i in 0..c.d.len() {
            c.d[i] += p_pow.d[i];
        }
        AetheronMlUnit {
            w1,
            b1,
            w2,
            b2,
            w2_sign,
            wq,
            wk,
            theta: vec![0.0; n],
            c_mix: c,
            ema_mean: vec![0.0; n],
            ema_var: vec![1.0; n],
            ema_rate: vec![0.15; n],
            eta: vec![cfg.eta_init; n],
            grad_window: vec![Vec::new(); n],
            cfg,
        }
    }

    fn forward(&self, x: &Mat, bn: BnMode) -> (Mat, Cache) {
        let b = x.r;
        let n = self.cfg.units;
        let db = self.cfg.branches;
        let h = self.cfg.hidden;
        let rows1 = n * db * h;
        let mut z1 = x.matmul_t(&self.w1);
        for i in 0..b {
            for j in 0..rows1 {
                z1.d[i * rows1 + j] += self.b1[j];
            }
        }
        let mut hh = Mat::zeros(b, rows1);
        for i in 0..z1.d.len() {
            hh.d[i] = z1.d[i].max(0.0);
        }
        let mut out = Mat::zeros(b, n * db);
        for bi in 0..b {
            for j in 0..(n * db) {
                let mut acc = self.b2[j];
                for i in 0..h {
                    acc += hh.d[bi * rows1 + j * h + i] * self.w2.d[j * h + i];
                }
                out.d[bi * (n * db) + j] = acc;
            }
        }
        let mut s0 = Mat::zeros(b, n);
        for bi in 0..b {
            for nn in 0..n {
                let mut acc = 0.0;
                for d in 0..db {
                    acc += out.d[bi * (n * db) + nn * db + d];
                }
                s0.d[bi * n + nn] = acc;
            }
        }
        let sk = s0.matmul_t(&self.c_mix);
        let mut pre = Mat::zeros(b, n);
        for bi in 0..b {
            for nn in 0..n {
                pre.d[bi * n + nn] = sk.d[bi * n + nn] - self.theta[nn];
            }
        }
        let mut mu = vec![0.0; n];
        let mut sd = vec![1.0; n];
        match bn {
            BnMode::FixedEma => {
                for nn in 0..n {
                    mu[nn] = self.ema_mean[nn];
                    sd[nn] = (self.ema_var[nn] + self.cfg.bn_eps).sqrt();
                }
            }
            BnMode::Train | BnMode::Transductive => {
                for nn in 0..n {
                    let mut m = 0.0;
                    for bi in 0..b {
                        m += pre.d[bi * n + nn];
                    }
                    m /= b.max(1) as F;
                    let mut v = 0.0;
                    for bi in 0..b {
                        let d0 = pre.d[bi * n + nn] - m;
                        v += d0 * d0;
                    }
                    v /= b.max(1) as F;
                    mu[nn] = m;
                    sd[nn] = (v + self.cfg.bn_eps).sqrt();
                }
            }
        }
        let mut normed = Mat::zeros(b, n);
        for bi in 0..b {
            for nn in 0..n {
                normed.d[bi * n + nn] = (pre.d[bi * n + nn] - mu[nn]) / sd[nn];
            }
        }
        let mut act = Mat::zeros(b, n);
        for i in 0..act.d.len() {
            act.d[i] = normed.d[i].tanh();
        }
        let mut gated = Mat::zeros(b, n);
        for i in 0..act.d.len() {
            let a = act.d[i];
            gated.d[i] = if a.abs() > self.cfg.eps { a } else { 0.0 };
        }
        let dk = self.cfg.dk;
        let mut q = Mat::zeros(b * n, dk);
        let mut k = Mat::zeros(b * n, dk);
        for bi in 0..b {
            for nn in 0..n {
                for j in 0..dk {
                    let mut aq = 0.0;
                    let mut ak = 0.0;
                    for d in 0..db {
                        let e = out.d[bi * (n * db) + nn * db + d];
                        aq += e * self.wq.d[d * dk + j];
                        ak += e * self.wk.d[d * dk + j];
                    }
                    q.d[(bi * n + nn) * dk + j] = aq;
                    k.d[(bi * n + nn) * dk + j] = ak;
                }
            }
        }
        let scale = 1.0 / (dk as F).sqrt();
        let mut attn = Mat::zeros(b * n, n);
        for bi in 0..b {
            let mut scores = Mat::zeros(n, n);
            for nn in 0..n {
                for mm in 0..n {
                    let mut acc = 0.0;
                    for j in 0..dk {
                        acc += q.d[(bi * n + nn) * dk + j] * k.d[(bi * n + mm) * dk + j];
                    }
                    scores.d[nn * n + mm] = acc * scale;
                }
            }
            let sm = softmax_rows(&scores);
            for nn in 0..n {
                for mm in 0..n {
                    attn.d[(bi * n + nn) * n + mm] = if !self.cfg.attention {
                        if nn == mm {
                            1.0
                        } else {
                            0.0
                        }
                    } else {
                        sm.d[nn * n + mm]
                    };
                }
            }
        }
        let mut routed = Mat::zeros(b, n);
        for bi in 0..b {
            for nn in 0..n {
                let mut acc = 0.0;
                for mm in 0..n {
                    acc += attn.d[(bi * n + nn) * n + mm] * gated.d[bi * n + mm];
                }
                routed.d[bi * n + nn] = acc;
            }
        }
        let cache = Cache {
            z1,
            h: hh,
            out,
            mu,
            sd,
            normed,
            act,
            gated,
            q,
            k,
            attn,
        };
        (routed, cache)
    }

    /// bn == Train/Transductive: full batch-statistics BN gradient.
    /// bn == FixedEma: the EMA statistics are detached constants, so the gradient is dnormed/sd.
    fn backward(&self, x: &Mat, cache: &Cache, drouted: &Mat, bn: BnMode) -> LayerGrads {
        let b = x.r;
        let n = self.cfg.units;
        let db = self.cfg.branches;
        let h = self.cfg.hidden;
        let dk = self.cfg.dk;
        let rows1 = n * db * h;
        let mut d_attn = Mat::zeros(b * n, n);
        let mut d_gated = Mat::zeros(b, n);
        for bi in 0..b {
            for nn in 0..n {
                for mm in 0..n {
                    d_attn.d[(bi * n + nn) * n + mm] =
                        drouted.d[bi * n + nn] * cache.gated.d[bi * n + mm];
                }
            }
            for mm in 0..n {
                let mut acc = 0.0;
                for nn in 0..n {
                    acc += drouted.d[bi * n + nn] * cache.attn.d[(bi * n + nn) * n + mm];
                }
                d_gated.d[bi * n + mm] = acc;
            }
        }
        let scale = 1.0 / (dk as F).sqrt();
        let mut d_scores = Mat::zeros(b * n, n);
        // With attention off the matrix is fixed, so nothing flows into the scores.
        for bi in 0..if self.cfg.attention { b } else { 0 } {
            for nn in 0..n {
                let mut dot = 0.0;
                for mm in 0..n {
                    dot += d_attn.d[(bi * n + nn) * n + mm] * cache.attn.d[(bi * n + nn) * n + mm];
                }
                for mm in 0..n {
                    let a = cache.attn.d[(bi * n + nn) * n + mm];
                    d_scores.d[(bi * n + nn) * n + mm] =
                        a * (d_attn.d[(bi * n + nn) * n + mm] - dot) * scale;
                }
            }
        }
        let mut dq = Mat::zeros(b * n, dk);
        let mut dkm = Mat::zeros(b * n, dk);
        for bi in 0..b {
            for nn in 0..n {
                for j in 0..dk {
                    let mut aq = 0.0;
                    for mm in 0..n {
                        aq +=
                            d_scores.d[(bi * n + nn) * n + mm] * cache.k.d[(bi * n + mm) * dk + j];
                    }
                    dq.d[(bi * n + nn) * dk + j] = aq;
                }
            }
            for mm in 0..n {
                for j in 0..dk {
                    let mut ak = 0.0;
                    for nn in 0..n {
                        ak +=
                            d_scores.d[(bi * n + nn) * n + mm] * cache.q.d[(bi * n + nn) * dk + j];
                    }
                    dkm.d[(bi * n + mm) * dk + j] = ak;
                }
            }
        }
        // dWq/dWk use the batch SUM: `drouted` already carries 1/B from the loss.
        let mut dwq = Mat::zeros(db, dk);
        let mut dwk = Mat::zeros(db, dk);
        for bi in 0..b {
            for d in 0..db {
                for j in 0..dk {
                    let mut aq = 0.0;
                    let mut ak = 0.0;
                    for nn in 0..n {
                        let e = cache.out.d[bi * (n * db) + nn * db + d];
                        aq += e * dq.d[(bi * n + nn) * dk + j];
                        ak += e * dkm.d[(bi * n + nn) * dk + j];
                    }
                    dwq.d[d * dk + j] += aq;
                    dwk.d[d * dk + j] += ak;
                }
            }
        }
        let mut dout_attn = Mat::zeros(b, n * db);
        for bi in 0..b {
            for nn in 0..n {
                for d in 0..db {
                    let mut acc = 0.0;
                    for j in 0..dk {
                        acc += dq.d[(bi * n + nn) * dk + j] * self.wq.d[d * dk + j]
                            + dkm.d[(bi * n + nn) * dk + j] * self.wk.d[d * dk + j];
                    }
                    dout_attn.d[bi * (n * db) + nn * db + d] = acc;
                }
            }
        }
        // Straight-through gate: dnormed = dgated * (1 - act^2).
        let mut dnormed = Mat::zeros(b, n);
        for i in 0..dnormed.d.len() {
            let a = cache.act.d[i];
            dnormed.d[i] = d_gated.d[i] * (1.0 - a * a);
        }
        let mut dpre = Mat::zeros(b, n);
        match bn {
            BnMode::Train | BnMode::Transductive => {
                for nn in 0..n {
                    let mut sum_dn = 0.0;
                    let mut sum_dn_normed = 0.0;
                    for bi in 0..b {
                        sum_dn += dnormed.d[bi * n + nn];
                        sum_dn_normed += dnormed.d[bi * n + nn] * cache.normed.d[bi * n + nn];
                    }
                    let sd = cache.sd[nn];
                    for bi in 0..b {
                        let dn = dnormed.d[bi * n + nn];
                        dpre.d[bi * n + nn] =
                            (b as F * dn - sum_dn - cache.normed.d[bi * n + nn] * sum_dn_normed)
                                / (b as F * sd);
                    }
                }
            }
            BnMode::FixedEma => {
                for nn in 0..n {
                    let sd = cache.sd[nn];
                    for bi in 0..b {
                        dpre.d[bi * n + nn] = dnormed.d[bi * n + nn] / sd;
                    }
                }
            }
        }
        let ds0 = dpre.matmul(&self.c_mix);
        let mut dout = Mat::zeros(b, n * db);
        for bi in 0..b {
            for nn in 0..n {
                for d in 0..db {
                    dout.d[bi * (n * db) + nn * db + d] =
                        ds0.d[bi * n + nn] + dout_attn.d[bi * (n * db) + nn * db + d];
                }
            }
        }
        let mut dw2 = Mat::zeros(n * db, h);
        let mut db2 = vec![0.0; n * db];
        for bi in 0..b {
            for j in 0..(n * db) {
                let d0 = dout.d[bi * (n * db) + j];
                db2[j] += d0;
                for i in 0..h {
                    dw2.d[j * h + i] += cache.h.d[bi * rows1 + j * h + i] * d0;
                }
            }
        }
        let mut dz1 = Mat::zeros(b, rows1);
        for bi in 0..b {
            for j in 0..(n * db) {
                let d0 = dout.d[bi * (n * db) + j];
                for i in 0..h {
                    let idx = bi * rows1 + j * h + i;
                    dz1.d[idx] = if cache.z1.d[idx] > 0.0 {
                        d0 * self.w2.d[j * h + i]
                    } else {
                        0.0
                    };
                }
            }
        }
        let dw1 = dz1.t_matmul(x);
        let db1 = dz1.colsum();
        let mut unit_norms = vec![0.0; n];
        for nn in 0..n {
            let mut acc = 0.0;
            let r0 = nn * db * h;
            let r1 = (nn + 1) * db * h;
            for i in r0..r1 {
                for jj in 0..x.c {
                    acc += dw1.d[i * x.c + jj] * dw1.d[i * x.c + jj];
                }
                acc += db1[i] * db1[i];
            }
            let w0 = nn * db;
            let w1b = (nn + 1) * db;
            for j in w0..w1b {
                for i in 0..h {
                    acc += dw2.d[j * h + i] * dw2.d[j * h + i];
                }
                acc += db2[j] * db2[j];
            }
            unit_norms[nn] = acc.sqrt();
        }
        LayerGrads {
            dw1,
            db1,
            dw2,
            db2,
            dwq,
            dwk,
            unit_norms,
        }
    }
}

#[derive(Clone)]
pub struct AetheronMlReadout {
    pub w: Mat,
    pub b: Vec<F>,
    pub n_classes: usize,
    pub n_units: usize,
}

impl AetheronMlReadout {
    pub fn new(n_units: usize, n_classes: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed ^ 0xA57E000000000001);
        let mut w = Mat::zeros(n_classes, n_units);
        let s = (1.0 / n_units as F).sqrt();
        for v in w.d.iter_mut() {
            *v = rng.normal() * s;
        }
        AetheronMlReadout {
            w,
            b: vec![0.0; n_classes],
            n_classes,
            n_units,
        }
    }

    pub fn forward(&self, routed: &Mat) -> Mat {
        let mut logits = routed.matmul_t(&self.w);
        for i in 0..logits.r {
            for j in 0..logits.c {
                logits.d[i * logits.c + j] += self.b[j];
            }
        }
        logits
    }
}

#[derive(Clone)]
pub struct AetheronMlLearner {
    pub cfg: AetheronMlCfg,
    pub unit: AetheronMlUnit,
    pub readout: AetheronMlReadout,
}

impl AetheronMlLearner {
    pub fn new(cfg: AetheronMlCfg, seed: u64) -> Self {
        let unit = AetheronMlUnit::new(cfg.clone(), seed);
        let readout = AetheronMlReadout::new(cfg.units, cfg.n_classes, seed);
        AetheronMlLearner { cfg, unit, readout }
    }

    pub fn loss_value(&self, x: &Mat, y: &[u8], bn: BnMode) -> F {
        let (routed, _c) = self.unit.forward(x, bn);
        let logits = self.readout.forward(&routed);
        cross_entropy(&logits, y).0
    }
}

impl Learner for AetheronMlLearner {
    fn n_params(&self) -> usize {
        self.unit.w1.d.len()
            + self.unit.b1.len()
            + self.unit.w2.d.len()
            + self.unit.b2.len()
            + self.unit.wq.d.len()
            + self.unit.wk.d.len()
            + self.readout.w.d.len()
            + self.readout.b.len()
    }

    fn step(&mut self, x: &Mat, y: &[u8], mode: Mode) {
        let (routed, cache) = self.unit.forward(x, BnMode::Train);
        let logits = self.readout.forward(&routed);
        let (_loss, dlogits) = cross_entropy(&logits, y);
        // Readout backward uses the OLD readout weights; then the readout is updated.
        let drouted = dlogits.matmul(&self.readout.w);
        let dwr = dlogits.t_matmul(&routed);
        let dbr = dlogits.colsum();
        let grads = self.unit.backward(x, &cache, &drouted, BnMode::Train);
        let lr = self.cfg.readout_lr;
        for i in 0..self.readout.w.d.len() {
            self.readout.w.d[i] -= lr * dwr.d[i];
        }
        for i in 0..self.readout.b.len() {
            self.readout.b[i] -= lr * dbr[i];
        }
        let mult = mode_multiplier(mode);
        let n = self.cfg.units;
        let db = self.cfg.branches;
        let h = self.cfg.hidden;
        for nn in 0..n {
            let lrn = self.unit.eta[nn] * mult;
            let r0 = nn * db * h;
            let r1 = (nn + 1) * db * h;
            for i in r0..r1 {
                let base = i * self.cfg.in_dim;
                for jj in 0..self.cfg.in_dim {
                    self.unit.w1.d[base + jj] -= lrn * grads.dw1.d[base + jj];
                }
                self.unit.b1[i] -= lrn * grads.db1[i];
            }
            let w0 = nn * db;
            let w1b = (nn + 1) * db;
            for j in w0..w1b {
                for i in 0..h {
                    self.unit.w2.d[j * h + i] -= lrn * grads.dw2.d[j * h + i];
                }
                self.unit.b2[j] -= lrn * grads.db2[j];
            }
        }
        for i in 0..self.unit.wq.d.len() {
            self.unit.wq.d[i] -= self.cfg.attn_lr * grads.dwq.d[i];
        }
        for i in 0..self.unit.wk.d.len() {
            self.unit.wk.d[i] -= self.cfg.attn_lr * grads.dwk.d[i];
        }
        // Dale's principle: the sign of W2 is fixed at initialisation and re-imposed after each update.
        for i in 0..self.unit.w2.d.len() {
            self.unit.w2.d[i] = self.unit.w2_sign[i] * self.unit.w2.d[i].abs();
        }
        let m = self.cfg.ema_momentum;
        for nn in 0..n {
            self.unit.ema_mean[nn] = (1.0 - m) * self.unit.ema_mean[nn] + m * cache.mu[nn];
            let var = (cache.sd[nn] * cache.sd[nn] - self.cfg.bn_eps).max(0.0);
            self.unit.ema_var[nn] = (1.0 - m) * self.unit.ema_var[nn] + m * var;
        }
        if self.cfg.homeostatic {
            let b = cache.act.r;
            for nn in 0..n {
                let mut s = 0.0;
                for bi in 0..b {
                    s += cache.act.d[bi * n + nn].abs();
                }
                let ma = s / b.max(1) as F;
                self.unit.ema_rate[nn] = 0.9 * self.unit.ema_rate[nn] + 0.1 * ma;
                self.unit.theta[nn] += 0.05 * (self.unit.ema_rate[nn] - 0.15);
            }
        }
        for nn in 0..n {
            let w = &mut self.unit.grad_window[nn];
            w.push(grads.unit_norms[nn]);
            if w.len() > self.cfg.window {
                w.remove(0);
            }
            if w.len() >= 2 {
                let mean: F = w.iter().sum::<F>() / w.len() as F;
                let var: F = w.iter().map(|v| (v - mean) * (v - mean)).sum::<F>() / w.len() as F;
                self.unit.eta[nn] = (self.unit.eta[nn] + 0.01 * (var - self.cfg.eta_var_target))
                    .clamp(self.cfg.eta_min, self.cfg.eta_max);
            }
        }
    }

    fn logits(&mut self, x: &Mat, eval: EvalNorm) -> Mat {
        let (routed, _c) = self.unit.forward(x, BnMode::from_eval(eval));
        self.readout.forward(&routed)
    }
}

// ---- finite differences by clone-and-perturb (no closure aliasing problem)
#[derive(Clone, Copy)]
enum Arr {
    W1,
    B1,
    W2,
    B2,
    Wq,
    Wk,
    Rdw,
    Rdb,
}

fn arr_name(a: Arr) -> &'static str {
    match a {
        Arr::W1 => "w1",
        Arr::B1 => "b1",
        Arr::W2 => "w2",
        Arr::B2 => "b2",
        Arr::Wq => "wq",
        Arr::Wk => "wk",
        Arr::Rdw => "readout.w",
        Arr::Rdb => "readout.b",
    }
}

impl AetheronMlLearner {
    fn arr_len(&self, a: Arr) -> usize {
        match a {
            Arr::W1 => self.unit.w1.d.len(),
            Arr::B1 => self.unit.b1.len(),
            Arr::W2 => self.unit.w2.d.len(),
            Arr::B2 => self.unit.b2.len(),
            Arr::Wq => self.unit.wq.d.len(),
            Arr::Wk => self.unit.wk.d.len(),
            Arr::Rdw => self.readout.w.d.len(),
            Arr::Rdb => self.readout.b.len(),
        }
    }

    fn arr_mut(&mut self, a: Arr) -> &mut Vec<F> {
        match a {
            Arr::W1 => &mut self.unit.w1.d,
            Arr::B1 => &mut self.unit.b1,
            Arr::W2 => &mut self.unit.w2.d,
            Arr::B2 => &mut self.unit.b2,
            Arr::Wq => &mut self.unit.wq.d,
            Arr::Wk => &mut self.unit.wk.d,
            Arr::Rdw => &mut self.readout.w.d,
            Arr::Rdb => &mut self.readout.b,
        }
    }
}

fn fd_check(
    model: &AetheronMlLearner,
    arr: Arr,
    x: &Mat,
    y: &[u8],
    n: usize,
    rng: &mut Rng,
    bn: BnMode,
    tag: &str,
) -> GradCheck {
    // Analytic gradients on the unchanged model (no update is applied).
    let (routed, cache) = model.unit.forward(x, bn);
    let logits = model.readout.forward(&routed);
    let (_l, dlogits) = cross_entropy(&logits, y);
    let drouted = dlogits.matmul(&model.readout.w);
    let grads = model.unit.backward(x, &cache, &drouted, bn);
    let dwr = dlogits.t_matmul(&routed);
    let dbr = dlogits.colsum();
    let analytic: Vec<F> = match arr {
        Arr::W1 => grads.dw1.d.clone(),
        Arr::B1 => grads.db1.clone(),
        Arr::W2 => grads.dw2.d.clone(),
        Arr::B2 => grads.db2.clone(),
        Arr::Wq => grads.dwq.d.clone(),
        Arr::Wk => grads.dwk.d.clone(),
        Arr::Rdw => dwr.d.clone(),
        Arr::Rdb => dbr,
    };
    let base = model.clone();
    let len = model.arr_len(arr);
    let mut idxs: Vec<usize> = (0..len).collect();
    rng.shuffle(&mut idxs);
    idxs.truncate(n.min(len));
    let h = 1e-5;
    let mut max_abs = 0.0f64;
    let mut max_rel = 0.0f64;
    for &i in &idxs {
        let mut m1 = base.clone();
        m1.arr_mut(arr)[i] += h;
        let lp = m1.loss_value(x, y, bn);
        let mut m2 = base.clone();
        m2.arr_mut(arr)[i] -= h;
        let lm = m2.loss_value(x, y, bn);
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
        name: format!("{}:{}", tag, arr_name(arr)),
        max_abs,
        max_rel,
        pass,
    }
}

/// Checks EVERY parameter array of the unit and readout, with the gate disabled (`cfg.eps = -1`)
/// and on both normalisation paths. Coordinate perturbation only.
pub fn gradcheck_model(
    model: &AetheronMlLearner,
    x: &Mat,
    y: &[u8],
    n: usize,
    rng: &mut Rng,
) -> Vec<GradCheck> {
    let mut out = Vec::new();
    for (bn, tag) in [(BnMode::Train, "trainBN"), (BnMode::FixedEma, "fixedEma")] {
        for arr in [
            Arr::W1,
            Arr::B1,
            Arr::W2,
            Arr::B2,
            Arr::Wq,
            Arr::Wk,
            Arr::Rdw,
            Arr::Rdb,
        ] {
            out.push(fd_check(model, arr, x, y, n, rng, bn, tag));
        }
    }
    out
}
