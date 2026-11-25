//! Temporal models for the delayed-match task: a recurrent Aetheron, a gated Aetheron, and a
//! one-layer single-head transformer baseline. Full back-propagation through time, hand-written
//! and checked by finite differences.

use crate::core::GradCheck;
use crate::linalg::{softmax_rows, Mat};
use crate::rng::Rng;
use crate::F;

pub const D_CONTENT: usize = 4;

pub fn d_in() -> usize {
    2 + D_CONTENT
}

/// Delayed-match batch. Token 0: marker channel 0 = 1 plus a one-hot of cA. Token T-1: marker
/// channel 1 = 1 plus a one-hot of cB. Other entries are N(0, noise). cB == cA with probability
/// 0.5, otherwise cB = (cA + 1 + U{0..d_content-2}) mod d_content. Label 1.0 iff cB == cA.
/// The marker channels are exactly 1.0 (not noisy).
pub fn make_batch(
    t: usize,
    b: usize,
    d_content: usize,
    noise: F,
    rng: &mut Rng,
) -> (Vec<Mat>, Vec<F>) {
    assert!(
        t >= 1 && d_content >= 2,
        "make_batch: need t>=1, d_content>=2"
    );
    let din = 2 + d_content;
    let mut xs: Vec<Mat> = Vec::with_capacity(t);
    for _ in 0..t {
        let mut m = Mat::zeros(b, din);
        for v in m.d.iter_mut() {
            *v = rng.normal() * noise;
        }
        xs.push(m);
    }
    let mut y = Vec::with_capacity(b);
    for bi in 0..b {
        let ca = rng.below(d_content);
        let is_match = rng.uniform() < 0.5;
        let cb = if is_match {
            ca
        } else {
            (ca + 1 + rng.below(d_content - 1)) % d_content
        };
        y.push(if cb == ca { 1.0 } else { 0.0 });
        for ti in 0..t {
            let row = &mut xs[ti].d[bi * din..(bi + 1) * din];
            if ti == 0 {
                row[0] = 1.0;
                for j in 0..d_content {
                    row[2 + j] = 0.0;
                }
                row[2 + ca] = 1.0;
            }
            if ti == t - 1 {
                row[1] = 1.0;
                for j in 0..d_content {
                    row[2 + j] = 0.0;
                }
                row[2 + cb] = 1.0;
            }
        }
    }
    (xs, y)
}

fn sigmoid(x: F) -> F {
    if x >= 0.0 {
        let e = (-x).exp();
        1.0 / (1.0 + e)
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

/// Stable binary cross-entropy on raw logits, mean over the batch. dlogit = (p - y)/B, once.
fn bce_mean(logit: &[F], y: &[F]) -> (F, Vec<F>) {
    let b = y.len().max(1);
    let mut loss = 0.0;
    let mut dl = Vec::with_capacity(y.len());
    for i in 0..y.len() {
        let p = sigmoid(logit[i]);
        loss += -(y[i] * p.max(1e-12).ln() + (1.0 - y[i]) * (1.0 - p).max(1e-12).ln());
        dl.push((p - y[i]) / b as F);
    }
    (loss / b as F, dl)
}

/// Clips to `max_norm`; a non-finite gradient is zeroed. Returns true when the guard tripped.
fn clip_and_guard(g: &mut [F], max_norm: F) -> bool {
    let norm: F = g.iter().map(|v| v * v).sum::<F>().sqrt();
    if !norm.is_finite() {
        for v in g.iter_mut() {
            *v = 0.0;
        }
        return true;
    }
    if norm > max_norm {
        let s = max_norm / norm;
        for v in g.iter_mut() {
            *v *= s;
        }
    }
    false
}

fn adam_step(p: &mut [F], g: &[F], m: &mut [F], v: &mut [F], t: &mut u64, lr: F) {
    *t += 1;
    let (b1, b2, eps) = (0.9f64, 0.999f64, 1e-8);
    let bc1 = 1.0 - b1.powi(*t as i32);
    let bc2 = 1.0 - b2.powi(*t as i32);
    for i in 0..p.len() {
        m[i] = b1 * m[i] + (1.0 - b1) * g[i];
        v[i] = b2 * v[i] + (1.0 - b2) * g[i] * g[i];
        p[i] -= lr * (m[i] / bc1) / ((v[i] / bc2).sqrt() + eps);
    }
}

fn concat_cols(x: &Mat, s: &Mat) -> Mat {
    let b = x.r;
    let mut out = Mat::zeros(b, x.c + s.c);
    for i in 0..b {
        out.d[i * (x.c + s.c)..i * (x.c + s.c) + x.c].copy_from_slice(&x.d[i * x.c..(i + 1) * x.c]);
        out.d[i * (x.c + s.c) + x.c..i * (x.c + s.c) + x.c + s.c]
            .copy_from_slice(&s.d[i * s.c..(i + 1) * s.c]);
    }
    out
}

fn slice_cols(inp: &Mat, start: usize, len: usize) -> Mat {
    let b = inp.r;
    let mut out = Mat::zeros(b, len);
    for i in 0..b {
        out.d[i * len..(i + 1) * len]
            .copy_from_slice(&inp.d[i * inp.c + start..i * inp.c + start + len]);
    }
    out
}

// ---------- shared dendritic two-layer block ----------
struct BranchCache {
    z1: Mat,
    h: Mat,
    cand: Mat,
}

struct BranchGrads {
    dw1: Vec<F>,
    db1: Vec<F>,
    dw2: Vec<F>,
    db2: Vec<F>,
    dtheta: Vec<F>,
}

fn branch_forward(
    w1: &Mat,
    b1: &[F],
    w2: &Mat,
    b2: &[F],
    theta: &[F],
    inp: &Mat,
    h_state: usize,
    branches: usize,
    hidden: usize,
) -> (Mat, BranchCache) {
    let b = inp.r;
    let rows1 = h_state * branches * hidden;
    let mut z1 = inp.matmul_t(w1);
    for i in 0..b {
        for j in 0..rows1 {
            z1.d[i * rows1 + j] += b1[j];
        }
    }
    let mut hh = Mat::zeros(b, rows1);
    for i in 0..z1.d.len() {
        hh.d[i] = z1.d[i].max(0.0);
    }
    let mut out = Mat::zeros(b, h_state * branches);
    for bi in 0..b {
        for j in 0..(h_state * branches) {
            let mut acc = b2[j];
            for i in 0..hidden {
                acc += hh.d[bi * rows1 + j * hidden + i] * w2.d[j * hidden + i];
            }
            out.d[bi * (h_state * branches) + j] = acc;
        }
    }
    let mut cand = Mat::zeros(b, h_state);
    for bi in 0..b {
        for nn in 0..h_state {
            let mut s = 0.0;
            for d in 0..branches {
                s += out.d[bi * (h_state * branches) + nn * branches + d];
            }
            cand.d[bi * h_state + nn] = (s - theta[nn]).tanh();
        }
    }
    (cand.clone(), BranchCache { z1, h: hh, cand })
}

fn branch_backward(
    w1: &Mat,
    w2: &Mat,
    cache: &BranchCache,
    dcand: &Mat,
    inp: &Mat,
    h_state: usize,
    branches: usize,
    hidden: usize,
) -> (Mat, BranchGrads) {
    let b = inp.r;
    let rows1 = h_state * branches * hidden;
    let mut ds = Mat::zeros(b, h_state);
    for i in 0..ds.d.len() {
        ds.d[i] = dcand.d[i] * (1.0 - cache.cand.d[i] * cache.cand.d[i]);
    }
    let mut dtheta = vec![0.0; h_state];
    for bi in 0..b {
        for nn in 0..h_state {
            dtheta[nn] -= ds.d[bi * h_state + nn];
        }
    }
    let mut dw2 = vec![0.0; h_state * branches * hidden];
    let mut db2 = vec![0.0; h_state * branches];
    for bi in 0..b {
        for j in 0..(h_state * branches) {
            let nn = j / branches;
            let d0 = ds.d[bi * h_state + nn];
            db2[j] += d0;
            for i in 0..hidden {
                dw2[j * hidden + i] += cache.h.d[bi * rows1 + j * hidden + i] * d0;
            }
        }
    }
    let mut dz1 = Mat::zeros(b, rows1);
    for bi in 0..b {
        for j in 0..(h_state * branches) {
            let nn = j / branches;
            let d0 = ds.d[bi * h_state + nn];
            for i in 0..hidden {
                let idx = bi * rows1 + j * hidden + i;
                dz1.d[idx] = if cache.z1.d[idx] > 0.0 {
                    d0 * w2.d[j * hidden + i]
                } else {
                    0.0
                };
            }
        }
    }
    let dw1 = dz1.t_matmul(inp).d;
    let db1 = dz1.colsum();
    let dinp = dz1.matmul(w1);
    (
        dinp,
        BranchGrads {
            dw1,
            db1,
            dw2,
            db2,
            dtheta,
        },
    )
}

fn add_branch_grads(acc: &mut BranchGrads, g: &BranchGrads) {
    for i in 0..acc.dw1.len() {
        acc.dw1[i] += g.dw1[i];
    }
    for i in 0..acc.db1.len() {
        acc.db1[i] += g.db1[i];
    }
    for i in 0..acc.dw2.len() {
        acc.dw2[i] += g.dw2[i];
    }
    for i in 0..acc.db2.len() {
        acc.db2[i] += g.db2[i];
    }
    for i in 0..acc.dtheta.len() {
        acc.dtheta[i] += g.dtheta[i];
    }
}

fn fd_check(
    name: &str,
    base_flat: &[F],
    arrays: &[(String, usize, usize)],
    analytic: &[(String, Vec<F>)],
    loss_at: &dyn Fn(&[F]) -> F,
    n: usize,
    rng: &mut Rng,
) -> Vec<GradCheck> {
    let mut outs = Vec::new();
    let h = 1e-5;
    for (aname, start, len) in arrays {
        let a = analytic
            .iter()
            .find(|(n, _)| n == aname)
            .map(|(_, g)| g.clone())
            .unwrap_or_else(|| vec![f64::NAN; *len]);
        let mut idxs: Vec<usize> = (*start..*start + len).collect();
        rng.shuffle(&mut idxs);
        idxs.truncate(n.min(*len));
        let mut max_abs = 0.0f64;
        let mut max_rel = 0.0f64;
        for &i in &idxs {
            let mut p1 = base_flat.to_vec();
            p1[i] += h;
            let lp = loss_at(&p1);
            let mut p2 = base_flat.to_vec();
            p2[i] -= h;
            let lm = loss_at(&p2);
            let num = (lp - lm) / (2.0 * h);
            let av = a[i - *start];
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
            name: format!("{}:{}", name, aname),
            max_abs,
            max_rel,
            pass,
        });
    }
    outs
}

fn ranges(roles: Vec<(&'static str, usize)>) -> Vec<(String, usize, usize)> {
    let mut o = 0;
    roles
        .into_iter()
        .map(|(nm, l)| {
            let s = o;
            o += l;
            (nm.to_string(), s, l)
        })
        .collect()
}

// ===================== RecurrentAetheron =====================
/// S_t = tanh(sum_d W2_d relu(W1_d [x_t; S_{t-1}] + b1_d) + b2 - theta), S_{-1} = 0.
/// Full BPTT; a linear readout on the last state through a sigmoid. theta is TRAINABLE here.
#[derive(Clone)]
pub struct RecurrentAetheron {
    pub d_in: usize,
    pub h: usize,
    pub branches: usize,
    pub hidden: usize,
    pub w1: Mat,
    pub b1: Vec<F>,
    pub w2: Mat,
    pub b2: Vec<F>,
    pub theta: Vec<F>,
    pub w_out: Vec<F>,
    pub b_out: F,
    pub m: Vec<F>,
    pub v: Vec<F>,
    pub t: u64,
    pub guard_events: u64,
}

impl RecurrentAetheron {
    pub fn new(d_in: usize, h: usize, branches: usize, hidden: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let rows1 = h * branches * hidden;
        let hin = d_in + h;
        let mut w1 = Mat::zeros(rows1, hin);
        let s1 = (2.0 / hin as F).sqrt();
        for v in w1.d.iter_mut() {
            *v = rng.normal() * s1;
        }
        let mut w2 = Mat::zeros(h * branches, hidden);
        let s2 = (1.0 / hidden as F).sqrt();
        for v in w2.d.iter_mut() {
            *v = rng.normal() * s2;
        }
        let mut w_out = vec![0.0; h];
        for v in w_out.iter_mut() {
            *v = rng.normal() * (1.0 / (h as F).sqrt());
        }
        let mut model = RecurrentAetheron {
            d_in,
            h,
            branches,
            hidden,
            w1,
            b1: vec![0.0; rows1],
            w2,
            b2: vec![0.0; h * branches],
            theta: vec![0.0; h],
            w_out,
            b_out: 0.0,
            m: Vec::new(),
            v: Vec::new(),
            t: 0,
            guard_events: 0,
        };
        let n = model.flat_len();
        model.m = vec![0.0; n];
        model.v = vec![0.0; n];
        model
    }

    pub fn n_params(&self) -> usize {
        self.flat_len()
    }

    fn flat_len(&self) -> usize {
        self.w1.d.len()
            + self.b1.len()
            + self.w2.d.len()
            + self.b2.len()
            + self.theta.len()
            + self.w_out.len()
            + 1
    }

    fn flatten(&self) -> Vec<F> {
        let mut v = Vec::with_capacity(self.flat_len());
        v.extend_from_slice(&self.w1.d);
        v.extend_from_slice(&self.b1);
        v.extend_from_slice(&self.w2.d);
        v.extend_from_slice(&self.b2);
        v.extend_from_slice(&self.theta);
        v.extend_from_slice(&self.w_out);
        v.push(self.b_out);
        v
    }

    fn load_flat(&mut self, src: &[F]) {
        let mut o = 0usize;
        let put = |dst: &mut Vec<F>, src: &[F], o: &mut usize| {
            let n = dst.len();
            dst.copy_from_slice(&src[*o..*o + n]);
            *o += n;
        };
        put(&mut self.w1.d, src, &mut o);
        put(&mut self.b1, src, &mut o);
        put(&mut self.w2.d, src, &mut o);
        put(&mut self.b2, src, &mut o);
        put(&mut self.theta, src, &mut o);
        put(&mut self.w_out, src, &mut o);
        self.b_out = src[o];
    }

    fn roles(&self) -> Vec<(&'static str, usize)> {
        vec![
            ("w1", self.w1.d.len()),
            ("b1", self.b1.len()),
            ("w2", self.w2.d.len()),
            ("b2", self.b2.len()),
            ("theta", self.theta.len()),
            ("w_out", self.w_out.len()),
            ("b_out", 1),
        ]
    }

    fn final_state(&self, xs: &[Mat], b: usize) -> Mat {
        let mut s = Mat::zeros(b, self.h);
        for x in xs {
            let inp = concat_cols(x, &s);
            let (cand, _c) = branch_forward(
                &self.w1,
                &self.b1,
                &self.w2,
                &self.b2,
                &self.theta,
                &inp,
                self.h,
                self.branches,
                self.hidden,
            );
            s = cand;
        }
        s
    }

    fn logits_of(&self, s: &Mat) -> Vec<F> {
        (0..s.r)
            .map(|bi| {
                let mut acc = self.b_out;
                for i in 0..self.h {
                    acc += s.d[bi * self.h + i] * self.w_out[i];
                }
                acc
            })
            .collect()
    }

    pub fn loss(&self, xs: &[Mat], y: &[F]) -> F {
        let s = self.final_state(xs, y.len());
        bce_mean(&self.logits_of(&s), y).0
    }

    pub fn predict_prob(&self, xs: &[Mat]) -> Vec<F> {
        let b = xs.first().map(|m| m.r).unwrap_or(0);
        let s = self.final_state(xs, b);
        self.logits_of(&s).into_iter().map(sigmoid).collect()
    }

    fn loss_and_arrays(&self, xs: &[Mat], y: &[F]) -> (F, Vec<(String, Vec<F>)>) {
        let b = y.len();
        let mut s = Mat::zeros(b, self.h);
        let mut caches = Vec::with_capacity(xs.len());
        let mut inps = Vec::with_capacity(xs.len());
        for x in xs {
            let inp = concat_cols(x, &s);
            let (cand, cache) = branch_forward(
                &self.w1,
                &self.b1,
                &self.w2,
                &self.b2,
                &self.theta,
                &inp,
                self.h,
                self.branches,
                self.hidden,
            );
            inps.push(inp);
            caches.push(cache);
            s = cand;
        }
        let (loss, dlogit) = bce_mean(&self.logits_of(&s), y);
        let mut dw_out = vec![0.0; self.h];
        let mut db_out = 0.0;
        for bi in 0..b {
            db_out += dlogit[bi];
            for i in 0..self.h {
                dw_out[i] += dlogit[bi] * s.d[bi * self.h + i];
            }
        }
        let mut ds_acc = Mat::zeros(b, self.h);
        for bi in 0..b {
            for i in 0..self.h {
                ds_acc.d[bi * self.h + i] = dlogit[bi] * self.w_out[i];
            }
        }
        let mut acc = BranchGrads {
            dw1: vec![0.0; self.w1.d.len()],
            db1: vec![0.0; self.b1.len()],
            dw2: vec![0.0; self.w2.d.len()],
            db2: vec![0.0; self.b2.len()],
            dtheta: vec![0.0; self.theta.len()],
        };
        for t in (0..xs.len()).rev() {
            let (dinp, g) = branch_backward(
                &self.w1,
                &self.w2,
                &caches[t],
                &ds_acc,
                &inps[t],
                self.h,
                self.branches,
                self.hidden,
            );
            add_branch_grads(&mut acc, &g);
            ds_acc = slice_cols(&dinp, self.d_in, self.h);
        }
        (
            loss,
            vec![
                ("w1".into(), acc.dw1),
                ("b1".into(), acc.db1),
                ("w2".into(), acc.dw2),
                ("b2".into(), acc.db2),
                ("theta".into(), acc.dtheta),
                ("w_out".into(), dw_out),
                ("b_out".into(), vec![db_out]),
            ],
        )
    }

    pub fn step(&mut self, xs: &[Mat], y: &[F], lr: F) -> F {
        let (loss, mut arrays) = self.loss_and_arrays(xs, y);
        for (_n, g) in arrays.iter_mut() {
            if clip_and_guard(g, 5.0) {
                self.guard_events += 1;
            }
        }
        let flat_g: Vec<F> = arrays.into_iter().flat_map(|(_n, g)| g).collect();
        let mut p = self.flatten();
        adam_step(&mut p, &flat_g, &mut self.m, &mut self.v, &mut self.t, lr);
        self.load_flat(&p);
        loss
    }

    pub fn gradcheck(&self, xs: &[Mat], y: &[F], n: usize, rng: &mut Rng) -> Vec<GradCheck> {
        let analytic = self.loss_and_arrays(xs, y).1;
        let roles = ranges(self.roles());
        let flat = self.flatten();
        let base = self.clone();
        let loss_at = |p: &[F]| -> F {
            let mut m = base.clone();
            m.load_flat(p);
            m.loss(xs, y)
        };
        fd_check("recurrent", &flat, &roles, &analytic, &loss_at, n, rng)
    }
}

// ===================== GatedAetheron =====================
/// cand_t as in RecurrentAetheron; f = sigmoid([x;S] Wf + bf) (bf initialised to 1.0);
/// i = sigmoid([x;S] Wi + bi); S_t = f * S_{t-1} + i * cand_t.
#[derive(Clone)]
pub struct GatedAetheron {
    pub d_in: usize,
    pub h: usize,
    pub branches: usize,
    pub hidden: usize,
    pub w1: Mat,
    pub b1: Vec<F>,
    pub w2: Mat,
    pub b2: Vec<F>,
    pub theta: Vec<F>,
    pub wf: Mat,
    pub bf: Vec<F>,
    pub wi: Mat,
    pub bi: Vec<F>,
    pub w_out: Vec<F>,
    pub b_out: F,
    pub m: Vec<F>,
    pub v: Vec<F>,
    pub t: u64,
    pub guard_events: u64,
}

impl GatedAetheron {
    pub fn new(d_in: usize, h: usize, branches: usize, hidden: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let rows1 = h * branches * hidden;
        let hin = d_in + h;
        let mut w1 = Mat::zeros(rows1, hin);
        let s1 = (2.0 / hin as F).sqrt();
        for v in w1.d.iter_mut() {
            *v = rng.normal() * s1;
        }
        let mut w2 = Mat::zeros(h * branches, hidden);
        let s2 = (1.0 / hidden as F).sqrt();
        for v in w2.d.iter_mut() {
            *v = rng.normal() * s2;
        }
        let mk_gate = |rng: &mut Rng| -> Mat {
            let mut g = Mat::zeros(h, hin);
            for v in g.d.iter_mut() {
                *v = rng.normal() * 0.1;
            }
            g
        };
        let wf = mk_gate(&mut rng);
        let wi = mk_gate(&mut rng);
        let mut w_out = vec![0.0; h];
        for v in w_out.iter_mut() {
            *v = rng.normal() * (1.0 / (h as F).sqrt());
        }
        let mut model = GatedAetheron {
            d_in,
            h,
            branches,
            hidden,
            w1,
            b1: vec![0.0; rows1],
            w2,
            b2: vec![0.0; h * branches],
            theta: vec![0.0; h],
            wf,
            bf: vec![1.0; h],
            wi,
            bi: vec![0.0; h],
            w_out,
            b_out: 0.0,
            m: Vec::new(),
            v: Vec::new(),
            t: 0,
            guard_events: 0,
        };
        let n = model.flat_len();
        model.m = vec![0.0; n];
        model.v = vec![0.0; n];
        model
    }

    pub fn n_params(&self) -> usize {
        self.flat_len()
    }

    fn flat_len(&self) -> usize {
        self.w1.d.len()
            + self.b1.len()
            + self.w2.d.len()
            + self.b2.len()
            + self.theta.len()
            + self.wf.d.len()
            + self.bf.len()
            + self.wi.d.len()
            + self.bi.len()
            + self.w_out.len()
            + 1
    }

    fn flatten(&self) -> Vec<F> {
        let mut v = Vec::with_capacity(self.flat_len());
        v.extend_from_slice(&self.w1.d);
        v.extend_from_slice(&self.b1);
        v.extend_from_slice(&self.w2.d);
        v.extend_from_slice(&self.b2);
        v.extend_from_slice(&self.theta);
        v.extend_from_slice(&self.wf.d);
        v.extend_from_slice(&self.bf);
        v.extend_from_slice(&self.wi.d);
        v.extend_from_slice(&self.bi);
        v.extend_from_slice(&self.w_out);
        v.push(self.b_out);
        v
    }

    fn load_flat(&mut self, src: &[F]) {
        let mut o = 0usize;
        let put = |dst: &mut Vec<F>, src: &[F], o: &mut usize| {
            let n = dst.len();
            dst.copy_from_slice(&src[*o..*o + n]);
            *o += n;
        };
        put(&mut self.w1.d, src, &mut o);
        put(&mut self.b1, src, &mut o);
        put(&mut self.w2.d, src, &mut o);
        put(&mut self.b2, src, &mut o);
        put(&mut self.theta, src, &mut o);
        put(&mut self.wf.d, src, &mut o);
        put(&mut self.bf, src, &mut o);
        put(&mut self.wi.d, src, &mut o);
        put(&mut self.bi, src, &mut o);
        put(&mut self.w_out, src, &mut o);
        self.b_out = src[o];
    }

    fn roles(&self) -> Vec<(&'static str, usize)> {
        vec![
            ("w1", self.w1.d.len()),
            ("b1", self.b1.len()),
            ("w2", self.w2.d.len()),
            ("b2", self.b2.len()),
            ("theta", self.theta.len()),
            ("wf", self.wf.d.len()),
            ("bf", self.bf.len()),
            ("wi", self.wi.d.len()),
            ("bi", self.bi.len()),
            ("w_out", self.w_out.len()),
            ("b_out", 1),
        ]
    }

    /// The forget and input gates for one step.
    fn gates(&self, inp: &Mat) -> (Mat, Mat) {
        let b = inp.r;
        let mut f = Mat::zeros(b, self.h);
        let mut i = Mat::zeros(b, self.h);
        for bi in 0..b {
            for j in 0..self.h {
                let mut af = self.bf[j];
                let mut ai = self.bi[j];
                for k in 0..inp.c {
                    af += inp.d[bi * inp.c + k] * self.wf.d[j * inp.c + k];
                    ai += inp.d[bi * inp.c + k] * self.wi.d[j * inp.c + k];
                }
                f.d[bi * self.h + j] = sigmoid(af);
                i.d[bi * self.h + j] = sigmoid(ai);
            }
        }
        (f, i)
    }

    fn final_state(&self, xs: &[Mat], b: usize) -> Mat {
        let mut s = Mat::zeros(b, self.h);
        for x in xs {
            let inp = concat_cols(x, &s);
            let (cand, _c) = branch_forward(
                &self.w1,
                &self.b1,
                &self.w2,
                &self.b2,
                &self.theta,
                &inp,
                self.h,
                self.branches,
                self.hidden,
            );
            let (f, i) = self.gates(&inp);
            let mut ns = Mat::zeros(b, self.h);
            for q in 0..ns.d.len() {
                ns.d[q] = f.d[q] * s.d[q] + i.d[q] * cand.d[q];
            }
            s = ns;
        }
        s
    }

    fn logits_of(&self, s: &Mat) -> Vec<F> {
        (0..s.r)
            .map(|bi| {
                let mut acc = self.b_out;
                for i in 0..self.h {
                    acc += s.d[bi * self.h + i] * self.w_out[i];
                }
                acc
            })
            .collect()
    }

    pub fn loss(&self, xs: &[Mat], y: &[F]) -> F {
        let s = self.final_state(xs, y.len());
        bce_mean(&self.logits_of(&s), y).0
    }

    pub fn predict_prob(&self, xs: &[Mat]) -> Vec<F> {
        let b = xs.first().map(|m| m.r).unwrap_or(0);
        let s = self.final_state(xs, b);
        self.logits_of(&s).into_iter().map(sigmoid).collect()
    }

    fn loss_and_arrays(&self, xs: &[Mat], y: &[F]) -> (F, Vec<(String, Vec<F>)>) {
        let b = y.len();
        let mut s = Mat::zeros(b, self.h);
        let mut caches = Vec::with_capacity(xs.len());
        let mut inps = Vec::with_capacity(xs.len());
        let mut prevs = Vec::with_capacity(xs.len());
        let mut fs = Vec::with_capacity(xs.len());
        let mut is_ = Vec::with_capacity(xs.len());
        for x in xs {
            let inp = concat_cols(x, &s);
            let (cand, cache) = branch_forward(
                &self.w1,
                &self.b1,
                &self.w2,
                &self.b2,
                &self.theta,
                &inp,
                self.h,
                self.branches,
                self.hidden,
            );
            let (f, i) = self.gates(&inp);
            let mut ns = Mat::zeros(b, self.h);
            for q in 0..ns.d.len() {
                ns.d[q] = f.d[q] * s.d[q] + i.d[q] * cand.d[q];
            }
            prevs.push(s);
            inps.push(inp);
            fs.push(f);
            is_.push(i);
            caches.push(cache);
            s = ns;
        }
        let (loss, dlogit) = bce_mean(&self.logits_of(&s), y);
        let mut dw_out = vec![0.0; self.h];
        let mut db_out = 0.0;
        for bi in 0..b {
            db_out += dlogit[bi];
            for i in 0..self.h {
                dw_out[i] += dlogit[bi] * s.d[bi * self.h + i];
            }
        }
        let mut ds_acc = Mat::zeros(b, self.h);
        for bi in 0..b {
            for i in 0..self.h {
                ds_acc.d[bi * self.h + i] = dlogit[bi] * self.w_out[i];
            }
        }
        let mut acc = BranchGrads {
            dw1: vec![0.0; self.w1.d.len()],
            db1: vec![0.0; self.b1.len()],
            dw2: vec![0.0; self.w2.d.len()],
            db2: vec![0.0; self.b2.len()],
            dtheta: vec![0.0; self.theta.len()],
        };
        let mut dwf = vec![0.0; self.wf.d.len()];
        let mut dbf = vec![0.0; self.bf.len()];
        let mut dwi = vec![0.0; self.wi.d.len()];
        let mut dbi = vec![0.0; self.bi.len()];
        for t in (0..xs.len()).rev() {
            let f = &fs[t];
            let i = &is_[t];
            let cand = &caches[t].cand;
            let s_prev = &prevs[t];
            let inp = &inps[t];
            let mut dcand = Mat::zeros(b, self.h);
            let mut ds_prev = Mat::zeros(b, self.h);
            let mut dpre_f = Mat::zeros(b, self.h);
            let mut dpre_i = Mat::zeros(b, self.h);
            for q in 0..dcand.d.len() {
                let dsv = ds_acc.d[q];
                dcand.d[q] = dsv * i.d[q];
                ds_prev.d[q] = dsv * f.d[q];
                let df = dsv * s_prev.d[q];
                let di = dsv * cand.d[q];
                dpre_f.d[q] = df * f.d[q] * (1.0 - f.d[q]);
                dpre_i.d[q] = di * i.d[q] * (1.0 - i.d[q]);
            }
            let df_cm = dpre_f.t_matmul(inp);
            for q in 0..dwf.len() {
                dwf[q] += df_cm.d[q];
            }
            let di_cm = dpre_i.t_matmul(inp);
            for q in 0..dwi.len() {
                dwi[q] += di_cm.d[q];
            }
            for bi in 0..b {
                for j in 0..self.h {
                    dbf[j] += dpre_f.d[bi * self.h + j];
                    dbi[j] += dpre_i.d[bi * self.h + j];
                }
            }
            let mut dinp = dpre_f.matmul(&self.wf);
            let di_in = dpre_i.matmul(&self.wi);
            for q in 0..dinp.d.len() {
                dinp.d[q] += di_in.d[q];
            }
            let (dinp_b, g) = branch_backward(
                &self.w1,
                &self.w2,
                &caches[t],
                &dcand,
                inp,
                self.h,
                self.branches,
                self.hidden,
            );
            add_branch_grads(&mut acc, &g);
            for q in 0..dinp.d.len() {
                dinp.d[q] += dinp_b.d[q];
            }
            let ds_from_inp = slice_cols(&dinp, self.d_in, self.h);
            for q in 0..ds_prev.d.len() {
                ds_prev.d[q] += ds_from_inp.d[q];
            }
            ds_acc = ds_prev;
        }
        (
            loss,
            vec![
                ("w1".into(), acc.dw1),
                ("b1".into(), acc.db1),
                ("w2".into(), acc.dw2),
                ("b2".into(), acc.db2),
                ("theta".into(), acc.dtheta),
                ("wf".into(), dwf),
                ("bf".into(), dbf),
                ("wi".into(), dwi),
                ("bi".into(), dbi),
                ("w_out".into(), dw_out),
                ("b_out".into(), vec![db_out]),
            ],
        )
    }

    pub fn step(&mut self, xs: &[Mat], y: &[F], lr: F) -> F {
        let (loss, mut arrays) = self.loss_and_arrays(xs, y);
        for (_n, g) in arrays.iter_mut() {
            if clip_and_guard(g, 5.0) {
                self.guard_events += 1;
            }
        }
        let flat_g: Vec<F> = arrays.into_iter().flat_map(|(_n, g)| g).collect();
        let mut p = self.flatten();
        adam_step(&mut p, &flat_g, &mut self.m, &mut self.v, &mut self.t, lr);
        self.load_flat(&p);
        loss
    }

    pub fn gradcheck(&self, xs: &[Mat], y: &[F], n: usize, rng: &mut Rng) -> Vec<GradCheck> {
        let analytic = self.loss_and_arrays(xs, y).1;
        let roles = ranges(self.roles());
        let flat = self.flatten();
        let base = self.clone();
        let loss_at = |p: &[F]| -> F {
            let mut m = base.clone();
            m.load_flat(p);
            m.loss(xs, y)
        };
        fd_check("gated", &flat, &roles, &analytic, &loss_at, n, rng)
    }
}

// ===================== MiniTransformer =====================
/// One layer, one head: Q, K, V = (X + PE) W (internal dk), pairwise softmax attention, the
/// context at the LAST position feeds a linear logit. Sinusoidal positions scaled by 0.1; no FFN.
#[derive(Clone)]
pub struct MiniTransformer {
    pub d_in: usize,
    pub dk: usize,
    pub wq: Mat,
    pub wk: Mat,
    pub wv: Mat,
    pub w_out: Vec<F>,
    pub b_out: F,
    pub m: Vec<F>,
    pub v: Vec<F>,
    pub t: u64,
    pub guard_events: u64,
}

impl MiniTransformer {
    pub fn new(d_in: usize, dk: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let s = (1.0 / d_in as F).sqrt();
        let mut mk = || -> Mat {
            let mut m = Mat::zeros(d_in, dk);
            for v in m.d.iter_mut() {
                *v = rng.normal() * s;
            }
            m
        };
        let wq = mk();
        let wk = mk();
        let wv = mk();
        let mut w_out = vec![0.0; dk];
        for v in w_out.iter_mut() {
            *v = rng.normal() * (1.0 / (dk as F).sqrt());
        }
        let n = d_in * dk * 3 + dk + 1;
        MiniTransformer {
            d_in,
            dk,
            wq,
            wk,
            wv,
            w_out,
            b_out: 0.0,
            m: vec![0.0; n],
            v: vec![0.0; n],
            t: 0,
            guard_events: 0,
        }
    }

    pub fn n_params(&self) -> usize {
        self.d_in * self.dk * 3 + self.dk + 1
    }

    fn flatten(&self) -> Vec<F> {
        let mut v = Vec::with_capacity(self.n_params());
        v.extend_from_slice(&self.wq.d);
        v.extend_from_slice(&self.wk.d);
        v.extend_from_slice(&self.wv.d);
        v.extend_from_slice(&self.w_out);
        v.push(self.b_out);
        v
    }

    fn load_flat(&mut self, src: &[F]) {
        let mut o = 0usize;
        let put = |dst: &mut Vec<F>, src: &[F], o: &mut usize| {
            let n = dst.len();
            dst.copy_from_slice(&src[*o..*o + n]);
            *o += n;
        };
        put(&mut self.wq.d, src, &mut o);
        put(&mut self.wk.d, src, &mut o);
        put(&mut self.wv.d, src, &mut o);
        put(&mut self.w_out, src, &mut o);
        self.b_out = src[o];
    }

    fn roles(&self) -> Vec<(&'static str, usize)> {
        vec![
            ("wq", self.wq.d.len()),
            ("wk", self.wk.d.len()),
            ("wv", self.wv.d.len()),
            ("w_out", self.w_out.len()),
            ("b_out", 1),
        ]
    }

    fn pe(t: usize, j: usize, din: usize) -> F {
        let ang = t as F / 10000f64.powf(2.0 * (j / 2) as F / din as F);
        0.1 * if j.is_multiple_of(2) {
            ang.sin()
        } else {
            ang.cos()
        }
    }

    pub fn loss(&self, xs: &[Mat], y: &[F]) -> F {
        self.loss_and_arrays(xs, y).0
    }

    pub fn predict_prob(&self, xs: &[Mat]) -> Vec<F> {
        let b = xs.first().map(|m| m.r).unwrap_or(0);
        let t_max = xs.len();
        let din = self.d_in;
        let dk = self.dk;
        let scale = 1.0 / (dk as F).sqrt();
        let mut xps = Vec::with_capacity(t_max);
        for t in 0..t_max {
            let mut xp = xs[t].clone();
            for i in 0..b {
                for j in 0..din {
                    xp.d[i * din + j] += Self::pe(t, j, din);
                }
            }
            xps.push(xp);
        }
        let qs: Vec<Mat> = xps.iter().map(|xp| xp.matmul(&self.wq)).collect();
        let ks: Vec<Mat> = xps.iter().map(|xp| xp.matmul(&self.wk)).collect();
        let vs: Vec<Mat> = xps.iter().map(|xp| xp.matmul(&self.wv)).collect();
        let mut probs = Vec::with_capacity(b);
        for bi in 0..b {
            let mut scores = Mat::zeros(t_max, t_max);
            for t in 0..t_max {
                for m in 0..t_max {
                    let mut acc = 0.0;
                    for j in 0..dk {
                        acc += qs[t].d[bi * dk + j] * ks[m].d[bi * dk + j];
                    }
                    scores.d[t * t_max + m] = acc * scale;
                }
            }
            let attn = softmax_rows(&scores);
            let mut ctx_last = vec![0.0; dk];
            let t = t_max - 1;
            for m in 0..t_max {
                let a = attn.d[t * t_max + m];
                for j in 0..dk {
                    ctx_last[j] += a * vs[m].d[bi * dk + j];
                }
            }
            let mut acc = self.b_out;
            for j in 0..dk {
                acc += ctx_last[j] * self.w_out[j];
            }
            probs.push(sigmoid(acc));
        }
        probs
    }

    fn loss_and_arrays(&self, xs: &[Mat], y: &[F]) -> (F, Vec<(String, Vec<F>)>) {
        let b = y.len();
        let t_max = xs.len();
        let din = self.d_in;
        let dk = self.dk;
        let scale = 1.0 / (dk as F).sqrt();
        let mut xps = Vec::with_capacity(t_max);
        for t in 0..t_max {
            let mut xp = xs[t].clone();
            for i in 0..b {
                for j in 0..din {
                    xp.d[i * din + j] += Self::pe(t, j, din);
                }
            }
            xps.push(xp);
        }
        let qs: Vec<Mat> = xps.iter().map(|xp| xp.matmul(&self.wq)).collect();
        let ks: Vec<Mat> = xps.iter().map(|xp| xp.matmul(&self.wk)).collect();
        let vs: Vec<Mat> = xps.iter().map(|xp| xp.matmul(&self.wv)).collect();
        let mut dwq = vec![0.0; self.wq.d.len()];
        let mut dwk = vec![0.0; self.wk.d.len()];
        let mut dwv = vec![0.0; self.wv.d.len()];
        let mut dw_out = vec![0.0; dk];
        let mut db_out = 0.0;
        let mut loss_acc = 0.0;
        for bi in 0..b {
            let mut scores = Mat::zeros(t_max, t_max);
            for t in 0..t_max {
                for m in 0..t_max {
                    let mut acc = 0.0;
                    for j in 0..dk {
                        acc += qs[t].d[bi * dk + j] * ks[m].d[bi * dk + j];
                    }
                    scores.d[t * t_max + m] = acc * scale;
                }
            }
            let attn = softmax_rows(&scores);
            let mut ctx = Mat::zeros(t_max, dk);
            for t in 0..t_max {
                for m in 0..t_max {
                    let a = attn.d[t * t_max + m];
                    for j in 0..dk {
                        ctx.d[t * dk + j] += a * vs[m].d[bi * dk + j];
                    }
                }
            }
            let t = t_max - 1;
            let mut logit = self.b_out;
            for j in 0..dk {
                logit += ctx.d[t * dk + j] * self.w_out[j];
            }
            let p = sigmoid(logit);
            let yi = y[bi];
            loss_acc += -(yi * p.max(1e-12).ln() + (1.0 - yi) * (1.0 - p).max(1e-12).ln());
            let inv = 1.0 / (b.max(1) as F);
            let dlogit = (p - yi) * inv;
            db_out += dlogit;
            for j in 0..dk {
                dw_out[j] += dlogit * ctx.d[t * dk + j];
            }
            let mut dctx = Mat::zeros(t_max, dk);
            for j in 0..dk {
                dctx.d[t * dk + j] = dlogit * self.w_out[j];
            }
            let mut dv = Mat::zeros(t_max, dk);
            let mut dattn = Mat::zeros(t_max, t_max);
            for a in 0..t_max {
                for m in 0..t_max {
                    let mut acc = 0.0;
                    for j in 0..dk {
                        acc += dctx.d[a * dk + j] * vs[m].d[bi * dk + j];
                    }
                    dattn.d[a * t_max + m] = acc;
                }
            }
            for m in 0..t_max {
                for a in 0..t_max {
                    let w = attn.d[a * t_max + m];
                    for j in 0..dk {
                        dv.d[m * dk + j] += w * dctx.d[a * dk + j];
                    }
                }
            }
            let mut dscores = Mat::zeros(t_max, t_max);
            for a in 0..t_max {
                let mut dot = 0.0;
                for m in 0..t_max {
                    dot += dattn.d[a * t_max + m] * attn.d[a * t_max + m];
                }
                for m in 0..t_max {
                    dscores.d[a * t_max + m] =
                        attn.d[a * t_max + m] * (dattn.d[a * t_max + m] - dot) * scale;
                }
            }
            let mut dq = Mat::zeros(t_max, dk);
            let mut dkm = Mat::zeros(t_max, dk);
            for a in 0..t_max {
                for j in 0..dk {
                    let mut acc = 0.0;
                    for m in 0..t_max {
                        acc += dscores.d[a * t_max + m] * ks[m].d[bi * dk + j];
                    }
                    dq.d[a * dk + j] = acc;
                }
            }
            for m in 0..t_max {
                for j in 0..dk {
                    let mut acc = 0.0;
                    for a in 0..t_max {
                        acc += dscores.d[a * t_max + m] * qs[a].d[bi * dk + j];
                    }
                    dkm.d[m * dk + j] = acc;
                }
            }
            for tt in 0..t_max {
                let xp_row = &xps[tt].d[bi * din..(bi + 1) * din];
                for ii in 0..din {
                    let xv = xp_row[ii];
                    if xv == 0.0 {
                        continue;
                    }
                    for j in 0..dk {
                        dwq[ii * dk + j] += xv * dq.d[tt * dk + j];
                        dwk[ii * dk + j] += xv * dkm.d[tt * dk + j];
                        dwv[ii * dk + j] += xv * dv.d[tt * dk + j];
                    }
                }
            }
        }
        (
            loss_acc / b.max(1) as F,
            vec![
                ("wq".into(), dwq),
                ("wk".into(), dwk),
                ("wv".into(), dwv),
                ("w_out".into(), dw_out),
                ("b_out".into(), vec![db_out]),
            ],
        )
    }

    pub fn step(&mut self, xs: &[Mat], y: &[F], lr: F) -> F {
        let (loss, mut arrays) = self.loss_and_arrays(xs, y);
        for (_n, g) in arrays.iter_mut() {
            if clip_and_guard(g, 5.0) {
                self.guard_events += 1;
            }
        }
        let flat_g: Vec<F> = arrays.into_iter().flat_map(|(_n, g)| g).collect();
        let mut p = self.flatten();
        adam_step(&mut p, &flat_g, &mut self.m, &mut self.v, &mut self.t, lr);
        self.load_flat(&p);
        loss
    }

    pub fn gradcheck(&self, xs: &[Mat], y: &[F], n: usize, rng: &mut Rng) -> Vec<GradCheck> {
        let analytic = self.loss_and_arrays(xs, y).1;
        let roles = ranges(self.roles());
        let flat = self.flatten();
        let base = self.clone();
        let loss_at = |p: &[F]| -> F {
            let mut m = base.clone();
            m.load_flat(p);
            m.loss(xs, y)
        };
        fd_check("transformer", &flat, &roles, &analytic, &loss_at, n, rng)
    }
}
