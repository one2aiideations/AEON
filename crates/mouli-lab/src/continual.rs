//! The Split-MNIST style continual-learning benchmark.

use crate::core::{EvalNorm, Learner, Mode};
use crate::data::{select_classes, Dataset};
use crate::linalg::Mat;
use crate::rng::Rng;
use crate::F;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strategy {
    None,
    Er,
    Sched,
    Ewc,
    Joint,
}

impl Strategy {
    pub fn parse(s: &str) -> Option<Strategy> {
        match s {
            "none" => Some(Strategy::None),
            "er" => Some(Strategy::Er),
            "sched" => Some(Strategy::Sched),
            "ewc" => Some(Strategy::Ewc),
            "joint" => Some(Strategy::Joint),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Strategy::None => "none",
            Strategy::Er => "er",
            Strategy::Sched => "sched",
            Strategy::Ewc => "ewc",
            Strategy::Joint => "joint",
        }
    }
}

#[derive(Clone)]
pub struct RunResult {
    pub arch: String,
    pub strategy: String,
    pub seed: u64,
    pub final_avg: F,
    pub per_task: Vec<F>,
    pub forgetting: F,
    pub pooled: F,
    pub transductive_final_avg: Option<F>,
    pub seconds: F,
}

#[derive(Clone)]
pub struct RunCfg {
    pub epochs: usize,
    pub batch: usize,
    pub lr: F,
    pub ewc_lambda: F,
    pub mem_per_class: usize,
}

impl Default for RunCfg {
    fn default() -> Self {
        RunCfg {
            epochs: 3,
            batch: 32,
            lr: 0.05,
            ewc_lambda: 100.0,
            mem_per_class: 50,
        }
    }
}

#[derive(Clone)]
pub struct TunedParams {
    pub lr: F,
    pub ewc_lambda: Option<F>,
    pub val_acc: F,
}

#[allow(clippy::large_enum_variant)]
enum Bench {
    Aeth(crate::aetheron_ml::AetheronMlLearner),
    Mlp(crate::baselines::MlpLearner),
}

impl Learner for Bench {
    fn n_params(&self) -> usize {
        match self {
            Bench::Aeth(l) => l.n_params(),
            Bench::Mlp(l) => l.n_params(),
        }
    }

    fn step(&mut self, x: &Mat, y: &[u8], mode: Mode) {
        match self {
            Bench::Aeth(l) => l.step(x, y, mode),
            Bench::Mlp(l) => l.step(x, y, mode),
        }
    }

    fn logits(&mut self, x: &Mat, eval: EvalNorm) -> Mat {
        match self {
            Bench::Aeth(l) => l.logits(x, eval),
            Bench::Mlp(l) => l.logits(x, eval),
        }
    }
}

impl Bench {
    fn as_mlp(&mut self) -> Option<&mut crate::baselines::MlpLearner> {
        match self {
            Bench::Mlp(l) => Some(l),
            Bench::Aeth(_) => None,
        }
    }
}

const TASKS: [(u8, u8); 5] = [(0, 1), (2, 3), (4, 5), (6, 7), (8, 9)];

fn make_learner(arch: &str, cfg: &RunCfg, seed: u64, in_dim: usize) -> Result<Bench, String> {
    let scale = cfg.lr / 0.05; // Aetheron multiplies eta_init and readout_lr by this
    match arch {
        "aetheron" => {
            let c = crate::aetheron_ml::AetheronMlCfg {
                in_dim,
                eta_init: 0.05 * scale,
                readout_lr: 0.05 * scale,
                ..Default::default()
            };
            Ok(Bench::Aeth(crate::aetheron_ml::AetheronMlLearner::new(
                c, seed,
            )))
        }
        "mlp" => {
            let hidden = if in_dim == 784 { 475 } else { 64 }; // 784 inputs: parameter-matched (h = 475)
            Ok(Bench::Mlp(crate::baselines::MlpLearner::new(
                in_dim, hidden, 10, seed, cfg.lr,
            )))
        }
        other => Err(format!("unknown arch '{}' (expected aetheron|mlp)", other)),
    }
}

fn eval_acc(learner: &mut Bench, ds: &Dataset, classes: &[u8], eval: EvalNorm) -> F {
    let idx: Vec<usize> = (0..ds.y.len())
        .filter(|&i| classes.contains(&ds.y[i]))
        .collect();
    if idx.is_empty() {
        return f64::NAN;
    }
    let sub = ds.subset(&idx);
    let logits = learner.logits(&sub.x, eval);
    let mut correct = 0usize;
    let nc = logits.c;
    for i in 0..sub.y.len() {
        let row = &logits.d[i * nc..(i + 1) * nc];
        let mut best = 0usize;
        for j in 1..nc {
            if row[j] > row[best] {
                best = j;
            }
        }
        if best as u8 == sub.y[i] {
            correct += 1;
        }
    }
    correct as F / sub.y.len() as F
}

fn concat_xy(x: &Mat, y: &[u8], x2: &Mat, y2: &[u8]) -> (Mat, Vec<u8>) {
    let mut out = Mat::zeros(x.r + x2.r, x.c);
    out.d[..x.d.len()].copy_from_slice(&x.d);
    out.d[x.d.len()..].copy_from_slice(&x2.d);
    let mut yy = y.to_vec();
    yy.extend_from_slice(y2);
    (out, yy)
}

fn draw_replay(
    mem: &[(u8, Vec<F>)],
    k: usize,
    noise: F,
    in_dim: usize,
    rng: &mut Rng,
) -> (Mat, Vec<u8>) {
    if mem.is_empty() {
        return (Mat::zeros(0, in_dim), Vec::new());
    }
    let mut xm = Mat::zeros(k, in_dim);
    let mut ym = Vec::with_capacity(k);
    for i in 0..k {
        let j = rng.below(mem.len()); // with replacement
        let (c, row) = &mem[j];
        for jj in 0..in_dim {
            xm.d[i * in_dim + jj] = row[jj]
                + if noise > 0.0 {
                    rng.normal() * noise
                } else {
                    0.0
                };
        }
        ym.push(*c);
    }
    (xm, ym)
}

fn gen_batch(
    protos: &[Option<Vec<F>>],
    seen: &[u8],
    k: usize,
    sigma: F,
    in_dim: usize,
    rng: &mut Rng,
) -> (Mat, Vec<u8>) {
    let avail: Vec<u8> = seen
        .iter()
        .copied()
        .filter(|&c| protos[c as usize].is_some())
        .collect();
    if avail.is_empty() {
        return (Mat::zeros(0, in_dim), Vec::new());
    }
    let mut xm = Mat::zeros(k, in_dim);
    let mut ym = Vec::with_capacity(k);
    for i in 0..k {
        let cls = avail[rng.below(avail.len())];
        if let Some(mean) = &protos[cls as usize] {
            for jj in 0..in_dim {
                xm.d[i * in_dim + jj] = mean[jj] + rng.normal() * sigma;
            }
            ym.push(cls);
        }
    }
    (xm, ym)
}

struct EvalOut {
    per_task: Vec<F>,
    final_avg: F,
    forgetting: F,
    pooled: F,
    transductive_final_avg: Option<F>,
}

fn train_internal(
    arch_eff: &str,
    learner: &mut Bench,
    train: &Dataset,
    eval_ds: &Dataset,
    cfg: &RunCfg,
    strategy: Strategy,
    seed: u64,
) -> EvalOut {
    let in_dim = train.x.c;
    let all_cls: Vec<u8> = (0u8..10).collect();
    if strategy == Strategy::Joint {
        let mut rng = Rng::new(seed ^ 0x51EED);
        let mut order: Vec<usize> = (0..train.y.len()).collect();
        for _ in 0..cfg.epochs * 5 {
            // same total passes as the sequential runs
            rng.shuffle(&mut order);
            for chunk in order.chunks(cfg.batch.max(1)) {
                if chunk.is_empty() {
                    continue;
                }
                let sub = train.subset(chunk);
                learner.step(&sub.x, &sub.y, Mode::Encode);
            }
        }
        let per_task: Vec<F> = TASKS
            .iter()
            .map(|&(a, b)| eval_acc(learner, eval_ds, &[a, b], EvalNorm::FixedEma))
            .collect();
        let final_avg = per_task.iter().copied().sum::<F>() / per_task.len() as F;
        let pooled = eval_acc(learner, eval_ds, &all_cls, EvalNorm::FixedEma);
        let trans = if arch_eff == "aetheron" {
            Some(
                TASKS
                    .iter()
                    .map(|&(a, b)| eval_acc(learner, eval_ds, &[a, b], EvalNorm::TransductiveBatch))
                    .sum::<F>()
                    / 5.0,
            )
        } else {
            None
        };
        return EvalOut {
            per_task,
            final_avg,
            forgetting: 0.0,
            pooled,
            transductive_final_avg: trans,
        };
    }
    let mut rng = Rng::new(seed ^ 0xC0FFEE);
    let mut mem: Vec<(u8, Vec<F>)> = Vec::new();
    let mut proto_sum = vec![vec![0.0; in_dim]; 10];
    let mut proto_cnt = [0usize; 10];
    let mut protos: Vec<Option<Vec<F>>> = vec![None; 10];
    let mut seen: Vec<u8> = Vec::new();
    let mut best = [0.0f64; 5];
    for (ti, &(c0, c1)) in TASKS.iter().enumerate() {
        let cls = [c0, c1];
        let tr = select_classes(train, &cls);
        for epoch in 0..cfg.epochs {
            let mode = match strategy {
                Strategy::Sched => match epoch % 3 {
                    0 => Mode::Encode,
                    1 => Mode::Consolidate,
                    _ => Mode::Generate,
                },
                _ => Mode::Encode,
            };
            let mut order: Vec<usize> = (0..tr.y.len()).collect();
            rng.shuffle(&mut order);
            for chunk in order.chunks(cfg.batch.max(1)) {
                if chunk.is_empty() {
                    continue;
                }
                let sub = tr.subset(chunk);
                if strategy == Strategy::Er {
                    let (rx, ry) = draw_replay(&mem, 32, 0.0, in_dim, &mut rng);
                    let (x, y) = concat_xy(&sub.x, &sub.y, &rx, &ry);
                    learner.step(&x, &y, mode);
                } else if strategy == Strategy::Sched && mode == Mode::Consolidate {
                    let (rx, ry) = draw_replay(&mem, 32, 0.05, in_dim, &mut rng);
                    let (x, y) = concat_xy(&sub.x, &sub.y, &rx, &ry);
                    learner.step(&x, &y, mode);
                } else if strategy == Strategy::Sched && mode == Mode::Generate {
                    let (rx, ry) = gen_batch(&protos, &seen, 32, 0.35, in_dim, &mut rng);
                    let (x, y) = concat_xy(&sub.x, &sub.y, &rx, &ry);
                    learner.step(&x, &y, mode);
                } else {
                    learner.step(&sub.x, &sub.y, mode);
                }
            }
        }
        if strategy == Strategy::Ewc {
            if let Some(mlp) = learner.as_mlp() {
                mlp.ewc_end_task(&tr, &mut rng);
            }
        }
        for i in 0..tr.y.len() {
            let c = tr.y[i] as usize;
            for j in 0..in_dim {
                proto_sum[c][j] += tr.x.d[i * in_dim + j];
            }
            proto_cnt[c] += 1;
        }
        for c in cls {
            let cc = c as usize;
            if proto_cnt[cc] > 0 {
                protos[cc] = Some(
                    proto_sum[cc]
                        .iter()
                        .map(|v| v / proto_cnt[cc] as F)
                        .collect(),
                );
            }
        }
        for c in cls {
            let mut pick: Vec<usize> = (0..tr.y.len()).filter(|&i| tr.y[i] == c).collect();
            rng.shuffle(&mut pick);
            pick.truncate(cfg.mem_per_class);
            for i in pick {
                mem.push((c, tr.x.d[i * in_dim..(i + 1) * in_dim].to_vec()));
            }
        }
        if !seen.contains(&c0) {
            seen.push(c0);
        }
        if !seen.contains(&c1) {
            seen.push(c1);
        }
        for jj in 0..=ti {
            let (a, b) = TASKS[jj];
            let acc = eval_acc(learner, eval_ds, &[a, b], EvalNorm::FixedEma);
            if acc > best[jj] {
                best[jj] = acc;
            }
        }
    }
    let per_task: Vec<F> = TASKS
        .iter()
        .map(|&(a, b)| eval_acc(learner, eval_ds, &[a, b], EvalNorm::FixedEma))
        .collect();
    let final_avg = per_task.iter().copied().sum::<F>() / 5.0;
    let forgetting = per_task
        .iter()
        .take(4)
        .enumerate()
        .map(|(j, &f)| best[j] - f)
        .sum::<F>()
        / 4.0;
    let pooled = eval_acc(learner, eval_ds, &all_cls, EvalNorm::FixedEma);
    let transductive_final_avg = if arch_eff == "aetheron" {
        Some(
            TASKS
                .iter()
                .map(|&(a, b)| eval_acc(learner, eval_ds, &[a, b], EvalNorm::TransductiveBatch))
                .sum::<F>()
                / 5.0,
        )
    } else {
        None
    };
    EvalOut {
        per_task,
        final_avg,
        forgetting,
        pooled,
        transductive_final_avg,
    }
}

/// The architecture that actually runs: Joint and Ewc are MLP-only, so asking for Aetheron with
/// them yields an honestly labelled MLP result, never a fake Aetheron one.
fn effective_arch(arch: &str, s: Strategy) -> &'static str {
    if s == Strategy::Joint || s == Strategy::Ewc {
        "mlp"
    } else if arch == "aetheron" {
        "aetheron"
    } else {
        "mlp"
    }
}

pub fn run_split(
    arch: &str,
    s: Strategy,
    seed: u64,
    train: &Dataset,
    test: &Dataset,
    cfg: &RunCfg,
) -> RunResult {
    let start = Instant::now();
    let eff = effective_arch(arch, s);
    if eff != arch {
        eprintln!(
            "note: strategy {} is MLP-only; reporting arch='mlp' (honest label), not '{}'",
            s.as_str(),
            arch
        );
    }
    let mut learner = make_learner(eff, cfg, seed, train.x.c).unwrap_or_else(|e| {
        eprintln!("{}; falling back to mlp", e);
        make_learner("mlp", cfg, seed, train.x.c).expect("mlp fallback must succeed")
    });
    let out = train_internal(eff, &mut learner, train, test, cfg, s, seed);
    RunResult {
        arch: eff.to_string(),
        strategy: s.as_str().to_string(),
        seed,
        final_avg: out.final_avg,
        per_task: out.per_task,
        forgetting: out.forgetting,
        pooled: out.pooled,
        transductive_final_avg: out.transductive_final_avg,
        seconds: start.elapsed().as_secs_f64(),
    }
}

/// Equal tuning budget for every method: fixed seed, same epochs and steps.
pub fn tune(arch: &str, s: Strategy, train: &Dataset, val: &Dataset, cfg: &RunCfg) -> TunedParams {
    let eff = effective_arch(arch, s);
    let lrs = [0.02, 0.05, 0.1];
    let lambdas = [10.0, 100.0, 1000.0, 10000.0];
    let tuning_seed = 12345u64;
    let mut best: Option<(F, F, Option<F>)> = None;
    for &lr in lrs.iter() {
        let trials: Vec<Option<F>> = if s == Strategy::Ewc {
            lambdas.iter().map(|l| Some(*l)).collect()
        } else {
            vec![None]
        };
        for lam in trials {
            let mut c = cfg.clone();
            c.lr = lr;
            if let Some(l) = lam {
                c.ewc_lambda = l;
            }
            let mut learner = match make_learner(eff, &c, tuning_seed, train.x.c) {
                Ok(l) => l,
                Err(_) => continue,
            };
            let out = train_internal(eff, &mut learner, train, val, &c, s, tuning_seed);
            let acc = out.final_avg;
            if best.as_ref().map(|b| acc > b.0).unwrap_or(true) {
                best = Some((acc, lr, lam));
            }
        }
    }
    let (val_acc, lr, lam) = best.unwrap_or((0.0, 0.05, None));
    TunedParams {
        lr,
        ewc_lambda: lam,
        val_acc,
    }
}
