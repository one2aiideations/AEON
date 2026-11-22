//! The forward pass against an independent naive implementation, on tiny synthetic models.

use aeon_gguf::types::GgmlType;
use aeon_gguf::{GgufWriter, Value};
use aeon_llm::{LlmError, Model, Sampler, Session};

#[derive(Clone)]
struct Spec {
    arch: &'static str,
    n_embd: usize,
    n_layer: usize,
    n_head: usize,
    n_head_kv: usize,
    n_ff: usize,
    n_vocab: usize,
    n_ctx: usize,
    rope_dim: Option<usize>,
    tied: bool,
    quant: bool,
    base: f32,
    /// Metadata that lies about the tensors (for loading-error tests).
    meta_head: Option<usize>,
    meta_ff: Option<usize>,
    meta_layer: Option<usize>,
}

impl Spec {
    fn llama() -> Spec {
        Spec {
            arch: "llama",
            n_embd: 16,
            n_layer: 2,
            n_head: 4,
            n_head_kv: 2,
            n_ff: 24,
            n_vocab: 13,
            n_ctx: 16,
            rope_dim: None,
            tied: false,
            quant: false,
            base: 10_000.0,
            meta_head: None,
            meta_ff: None,
            meta_layer: None,
        }
    }
    fn head_dim(&self) -> usize {
        self.n_embd / self.n_head
    }
    fn kv_dim(&self) -> usize {
        self.n_head_kv * self.head_dim()
    }
    fn bias(&self) -> bool {
        self.arch == "qwen2"
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) - 0.5
    }
    fn vec(&mut self, n: usize, scale: f32) -> Vec<f32> {
        (0..n).map(|_| self.next() * scale).collect()
    }
}

struct LayerW {
    attn_norm: Vec<f32>,
    wq: Vec<f32>,
    wk: Vec<f32>,
    wv: Vec<f32>,
    wo: Vec<f32>,
    bq: Vec<f32>,
    bk: Vec<f32>,
    bv: Vec<f32>,
    ffn_norm: Vec<f32>,
    gate: Vec<f32>,
    up: Vec<f32>,
    down: Vec<f32>,
}

struct Weights {
    embd: Vec<f32>,
    out_norm: Vec<f32>,
    output: Vec<f32>,
    layers: Vec<LayerW>,
}

fn weights(s: &Spec, seed: u64) -> Weights {
    let mut r = Rng(seed);
    let (e, kv, ff, v) = (s.n_embd, s.kv_dim(), s.n_ff, s.n_vocab);
    let norm = |r: &mut Rng, n| {
        r.vec(n, 0.2)
            .into_iter()
            .map(|x| 1.0 + x)
            .collect::<Vec<f32>>()
    };
    let embd = r.vec(v * e, 1.0);
    let output = if s.tied {
        embd.clone()
    } else {
        r.vec(v * e, 1.0)
    };
    let layers = (0..s.n_layer)
        .map(|_| LayerW {
            attn_norm: norm(&mut r, e),
            wq: r.vec(e * e, 0.8),
            wk: r.vec(kv * e, 0.8),
            wv: r.vec(kv * e, 0.8),
            wo: r.vec(e * e, 0.8),
            bq: if s.bias() { r.vec(e, 0.5) } else { vec![] },
            bk: if s.bias() { r.vec(kv, 0.5) } else { vec![] },
            bv: if s.bias() { r.vec(kv, 0.5) } else { vec![] },
            ffn_norm: norm(&mut r, e),
            gate: r.vec(ff * e, 0.8),
            up: r.vec(ff * e, 0.8),
            down: r.vec(e * ff, 0.8),
        })
        .collect();
    Weights {
        embd,
        out_norm: norm(&mut r, e),
        output,
        layers,
    }
}

// ------------------------------------------------------------ Q8_0 test encoder

fn f32_to_f16_bits(x: f32) -> u16 {
    // Enough for scales: round-to-nearest for normal range, flush tiny values to zero.
    if x == 0.0 || x.abs() < 6.2e-5 {
        return 0;
    }
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let man = b & 0x007f_ffff;
    let mut h = ((exp as u32) << 10) | (man >> 13);
    if man & 0x1000 != 0 && (man & 0x2fff != 0) {
        h += 1;
    }
    sign | h as u16
}

fn f16_bits_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let frac = u32::from(h & 0x3ff);
    if exp == 0 {
        return f32::from_bits(sign);
    }
    f32::from_bits(sign | ((exp + 112) << 23) | (frac << 13))
}

fn q8_0(data: &[f32]) -> Vec<u8> {
    assert_eq!(data.len() % 32, 0);
    let mut out = Vec::new();
    for block in data.chunks(32) {
        let amax = block.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        let hb = f32_to_f16_bits(amax / 127.0);
        let d = f16_bits_to_f32(hb);
        out.extend_from_slice(&hb.to_le_bytes());
        for v in block {
            let q = if d == 0.0 {
                0
            } else {
                (v / d).round().clamp(-127.0, 127.0) as i8
            };
            out.push(q as u8);
        }
    }
    out
}

fn f32_bytes(data: &[f32]) -> Vec<u8> {
    data.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn gguf(s: &Spec, w: &Weights) -> Vec<u8> {
    let mut g = GgufWriter::new();
    let a = s.arch;
    g.add_metadata("general.architecture", Value::String(a.into()));
    g.add_metadata(
        &format!("{a}.embedding_length"),
        Value::U32(s.n_embd as u32),
    );
    g.add_metadata(
        &format!("{a}.block_count"),
        Value::U32(s.meta_layer.unwrap_or(s.n_layer) as u32),
    );
    g.add_metadata(
        &format!("{a}.attention.head_count"),
        Value::U32(s.meta_head.unwrap_or(s.n_head) as u32),
    );
    g.add_metadata(
        &format!("{a}.attention.head_count_kv"),
        Value::U32(s.meta_head.unwrap_or(s.n_head_kv) as u32),
    );
    g.add_metadata(
        &format!("{a}.feed_forward_length"),
        Value::U32(s.meta_ff.unwrap_or(s.n_ff) as u32),
    );
    g.add_metadata(&format!("{a}.context_length"), Value::U32(s.n_ctx as u32));
    g.add_metadata(&format!("{a}.rope.freq_base"), Value::F32(s.base));
    g.add_metadata(
        &format!("{a}.attention.layer_norm_rms_epsilon"),
        Value::F32(1e-5),
    );
    if let Some(rd) = s.rope_dim {
        g.add_metadata(&format!("{a}.rope.dimension_count"), Value::U32(rd as u32));
    }
    let mat = |g: &mut GgufWriter, name: &str, cols: usize, rows: usize, data: &[f32]| {
        if s.quant {
            g.add_tensor(
                name,
                &[cols as u64, rows as u64],
                GgmlType::Q8_0,
                q8_0(data),
            )
            .unwrap();
        } else {
            g.add_tensor(
                name,
                &[cols as u64, rows as u64],
                GgmlType::F32,
                f32_bytes(data),
            )
            .unwrap();
        }
    };
    let vec1 = |g: &mut GgufWriter, name: &str, data: &[f32]| {
        g.add_tensor(name, &[data.len() as u64], GgmlType::F32, f32_bytes(data))
            .unwrap();
    };
    let (e, kv, ff, v) = (s.n_embd, s.kv_dim(), s.n_ff, s.n_vocab);
    mat(&mut g, "token_embd.weight", e, v, &w.embd);
    vec1(&mut g, "output_norm.weight", &w.out_norm);
    if !s.tied {
        mat(&mut g, "output.weight", e, v, &w.output);
    }
    for (l, lw) in w.layers.iter().enumerate() {
        let p = |n: &str| format!("blk.{l}.{n}");
        vec1(&mut g, &p("attn_norm.weight"), &lw.attn_norm);
        mat(&mut g, &p("attn_q.weight"), e, e, &lw.wq);
        mat(&mut g, &p("attn_k.weight"), e, kv, &lw.wk);
        mat(&mut g, &p("attn_v.weight"), e, kv, &lw.wv);
        mat(&mut g, &p("attn_output.weight"), e, e, &lw.wo);
        if s.bias() {
            vec1(&mut g, &p("attn_q.bias"), &lw.bq);
            vec1(&mut g, &p("attn_k.bias"), &lw.bk);
            vec1(&mut g, &p("attn_v.bias"), &lw.bv);
        }
        vec1(&mut g, &p("ffn_norm.weight"), &lw.ffn_norm);
        mat(&mut g, &p("ffn_gate.weight"), e, ff, &lw.gate);
        mat(&mut g, &p("ffn_up.weight"), e, ff, &lw.up);
        mat(&mut g, &p("ffn_down.weight"), ff, e, &lw.down);
    }
    g.finish().unwrap()
}

// ------------------------------------------------------------ naive reference (f64)

struct Reference<'a> {
    s: &'a Spec,
    w: &'a Weights,
    k: Vec<Vec<Vec<f64>>>, // [layer][pos] -> kv_dim
    v: Vec<Vec<Vec<f64>>>,
}

fn mv(m: &[f32], rows: usize, cols: usize, x: &[f64]) -> Vec<f64> {
    (0..rows)
        .map(|r| (0..cols).map(|c| f64::from(m[r * cols + c]) * x[c]).sum())
        .collect()
}

fn rms(x: &[f64], w: &[f32]) -> Vec<f64> {
    let ms = x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64;
    let inv = 1.0 / (ms + 1e-5f32 as f64).sqrt();
    x.iter()
        .zip(w)
        .map(|(v, w)| v * inv * f64::from(*w))
        .collect()
}

impl<'a> Reference<'a> {
    fn new(s: &'a Spec, w: &'a Weights) -> Self {
        Reference {
            s,
            w,
            k: vec![vec![]; s.n_layer],
            v: vec![vec![]; s.n_layer],
        }
    }

    fn rope(&self, x: &mut [f64], heads: usize, pos: usize) {
        let hd = self.s.head_dim();
        let rd = self.s.rope_dim.unwrap_or(hd);
        for h in 0..heads {
            for i in 0..rd / 2 {
                let theta = f64::from(self.s.base).powf(-(2.0 * i as f64) / rd as f64);
                let ang = pos as f64 * theta;
                let (sn, cs) = ang.sin_cos();
                let (a, b) = if self.s.arch == "llama" {
                    (h * hd + 2 * i, h * hd + 2 * i + 1)
                } else {
                    (h * hd + i, h * hd + i + rd / 2)
                };
                let (x0, x1) = (x[a], x[b]);
                x[a] = x0 * cs - x1 * sn;
                x[b] = x0 * sn + x1 * cs;
            }
        }
    }

    fn step(&mut self, token: usize, pos: usize) -> Vec<f64> {
        let s = self.s;
        let (e, hd, kvd) = (s.n_embd, s.head_dim(), s.kv_dim());
        let mut x: Vec<f64> = self.w.embd[token * e..(token + 1) * e]
            .iter()
            .map(|v| f64::from(*v))
            .collect();
        for l in 0..s.n_layer {
            let lw = &self.w.layers[l];
            let xn = rms(&x, &lw.attn_norm);
            let mut q = mv(&lw.wq, e, e, &xn);
            let mut k = mv(&lw.wk, kvd, e, &xn);
            let mut v = mv(&lw.wv, kvd, e, &xn);
            if s.bias() {
                for (a, b) in q.iter_mut().zip(&lw.bq) {
                    *a += f64::from(*b);
                }
                for (a, b) in k.iter_mut().zip(&lw.bk) {
                    *a += f64::from(*b);
                }
                for (a, b) in v.iter_mut().zip(&lw.bv) {
                    *a += f64::from(*b);
                }
            }
            self.rope(&mut q, s.n_head, pos);
            self.rope(&mut k, s.n_head_kv, pos);
            self.k[l].push(k);
            self.v[l].push(v);
            let mut ctx = vec![0.0f64; e];
            for h in 0..s.n_head {
                let kvh = h * s.n_head_kv / s.n_head;
                let mut sc: Vec<f64> = (0..=pos)
                    .map(|p| {
                        (0..hd)
                            .map(|d| q[h * hd + d] * self.k[l][p][kvh * hd + d])
                            .sum::<f64>()
                            / (hd as f64).sqrt()
                    })
                    .collect();
                let m = sc.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let mut z = 0.0;
                for v in sc.iter_mut() {
                    *v = (*v - m).exp();
                    z += *v;
                }
                for p in 0..=pos {
                    for d in 0..hd {
                        ctx[h * hd + d] += sc[p] / z * self.v[l][p][kvh * hd + d];
                    }
                }
            }
            let o = mv(&lw.wo, e, e, &ctx);
            for (a, b) in x.iter_mut().zip(&o) {
                *a += b;
            }
            let xn = rms(&x, &lw.ffn_norm);
            let g = mv(&lw.gate, s.n_ff, e, &xn);
            let u = mv(&lw.up, s.n_ff, e, &xn);
            let hdn: Vec<f64> = g
                .iter()
                .zip(&u)
                .map(|(g, u)| g / (1.0 + (-g).exp()) * u)
                .collect();
            let d = mv(&lw.down, e, s.n_ff, &hdn);
            for (a, b) in x.iter_mut().zip(&d) {
                *a += b;
            }
        }
        let xn = rms(&x, &self.w.out_norm);
        mv(&self.w.output, s.n_vocab, e, &xn)
    }
}

fn max_diff(a: &[f32], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - y).abs())
        .fold(0.0, f64::max)
}

fn check_against_reference(s: &Spec, seed: u64, tol: f64) {
    let w = weights(s, seed);
    let model = Model::from_bytes(gguf(s, &w)).unwrap();
    let mut sess = Session::new(&model, s.n_ctx).unwrap();
    let mut reference = Reference::new(s, &w);
    let tokens = [3usize, 0, 7, 7, 12, 5, 1, 9];
    for (pos, t) in tokens.iter().enumerate() {
        let got = sess.forward(*t as u32).unwrap().to_vec();
        let want = reference.step(*t, pos);
        let d = max_diff(&got, &want);
        assert!(d < tol, "{} pos {pos}: max |diff| {d}", s.arch);
    }
    assert_eq!(sess.position(), tokens.len());
}

// ------------------------------------------------------------------- tests

#[test]
fn llama_matches_the_reference() {
    check_against_reference(&Spec::llama(), 1, 2e-4);
}

#[test]
fn qwen2_with_biases_and_split_half_rope_matches() {
    let s = Spec {
        arch: "qwen2",
        ..Spec::llama()
    };
    check_against_reference(&s, 2, 2e-4);
}

#[test]
fn partial_rope_dimensions() {
    let mut s = Spec::llama();
    s.rope_dim = Some(2); // only the first pair of each 4-wide head rotates
    check_against_reference(&s, 3, 2e-4);
    let q = Spec {
        arch: "qwen2",
        rope_dim: Some(2),
        ..Spec::llama()
    };
    check_against_reference(&q, 4, 2e-4);
}

#[test]
fn grouped_query_and_multi_head_variants() {
    // MHA (kv heads == heads) and MQA (one kv head)
    for kv in [4, 1] {
        let s = Spec {
            n_head_kv: kv,
            ..Spec::llama()
        };
        check_against_reference(&s, 10 + kv as u64, 2e-4);
    }
}

#[test]
fn tied_embeddings() {
    let s = Spec {
        tied: true,
        ..Spec::llama()
    };
    check_against_reference(&s, 5, 2e-4);
}

#[test]
fn rope_base_matters() {
    let a = Spec::llama();
    let b = Spec {
        base: 500.0,
        ..Spec::llama()
    };
    check_against_reference(&b, 6, 2e-4);
    let w = weights(&a, 6);
    let ma = Model::from_bytes(gguf(&a, &w)).unwrap();
    let mb = Model::from_bytes(gguf(
        &Spec {
            base: 500.0,
            ..a.clone()
        },
        &w,
    ))
    .unwrap();
    let (mut sa, mut sb) = (
        Session::new(&ma, 16).unwrap(),
        Session::new(&mb, 16).unwrap(),
    );
    let mut last = (vec![], vec![]);
    for t in [1u32, 2, 3, 4] {
        last = (
            sa.forward(t).unwrap().to_vec(),
            sb.forward(t).unwrap().to_vec(),
        );
    }
    assert_ne!(last.0, last.1);
}

#[test]
fn q8_0_weights_stay_close_to_f32() {
    let base = Spec {
        n_embd: 32,
        n_head: 4,
        n_head_kv: 2,
        n_ff: 64,
        n_vocab: 16,
        ..Spec::llama()
    };
    let w = weights(&base, 7);
    let exact = Model::from_bytes(gguf(&base, &w)).unwrap();
    let quant = Model::from_bytes(gguf(
        &Spec {
            quant: true,
            ..base.clone()
        },
        &w,
    ))
    .unwrap();
    let (mut se, mut sq) = (
        Session::new(&exact, 16).unwrap(),
        Session::new(&quant, 16).unwrap(),
    );
    for t in [4u32, 9, 1, 15, 0, 7] {
        let a = se.forward(t).unwrap().to_vec();
        let b = sq.forward(t).unwrap().to_vec();
        let scale = a.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let d = a
            .iter()
            .zip(&b)
            .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
        assert!(
            d < 0.08 * scale.max(1.0),
            "token {t}: diff {d} vs scale {scale}"
        );
    }
}

#[test]
fn prefill_equals_stepwise_and_reset_replays() {
    let s = Spec::llama();
    let w = weights(&s, 8);
    let model = Model::from_bytes(gguf(&s, &w)).unwrap();
    let toks = [2u32, 5, 11, 0, 4];
    let mut a = Session::new(&model, 16).unwrap();
    let mut last = vec![];
    for t in toks {
        last = a.forward(t).unwrap().to_vec();
    }
    let mut b = Session::new(&model, 16).unwrap();
    assert_eq!(b.prefill(&toks).unwrap(), &last[..]);
    assert_eq!(b.position(), 5);
    assert_eq!(b.remaining(), 11);
    b.reset();
    assert_eq!(b.position(), 0);
    assert_eq!(b.prefill(&toks).unwrap(), &last[..]);
}

#[test]
fn context_limit_and_input_errors() {
    let s = Spec::llama();
    let model = Model::from_bytes(gguf(&s, &weights(&s, 9))).unwrap();
    let mut sess = Session::new(&model, 3).unwrap();
    for t in 0..3 {
        sess.forward(t).unwrap();
    }
    assert!(matches!(sess.forward(1), Err(LlmError::ContextFull)));
    let mut sess = Session::new(&model, 3).unwrap();
    assert!(matches!(
        sess.prefill(&[1, 2, 3, 4]),
        Err(LlmError::ContextFull)
    ));
    assert!(matches!(sess.prefill(&[]), Err(LlmError::BadConfig(_))));
    assert!(matches!(sess.forward(13), Err(LlmError::BadToken(13))));
    assert_eq!(sess.position(), 0, "a failed forward must not advance");
    assert!(matches!(
        Session::new(&model, 0),
        Err(LlmError::BadConfig(_))
    ));
    assert!(matches!(
        Session::new(&model, 17),
        Err(LlmError::BadConfig(_))
    ));
}

#[test]
fn greedy_generation_is_deterministic_and_stops() {
    let s = Spec::llama();
    let model = Model::from_bytes(gguf(&s, &weights(&s, 12))).unwrap();
    let run = |stop: &[u32]| {
        let mut sess = Session::new(&model, 16).unwrap();
        sess.generate(&[1, 2], 8, &mut Sampler::greedy(), stop)
            .unwrap()
    };
    let full = run(&[]);
    assert_eq!(full.len(), 8);
    assert_eq!(full, run(&[]));
    // stopping on the third generated token cuts the output there and excludes the stop token
    let stop = full[2];
    let cut = run(&[stop]);
    let first_hit = full.iter().position(|t| *t == stop).unwrap();
    assert_eq!(cut, full[..first_hit].to_vec());
    // limited by the context window
    let mut tiny = Session::new(&model, 4).unwrap();
    let out = tiny
        .generate(&[1, 2], 100, &mut Sampler::greedy(), &[])
        .unwrap();
    // 4-token window, 2-token prompt: tokens at positions 2 and 3 are fed back, and the
    // logits after the last fed token still yield one more sample.
    assert_eq!(out.len(), 3);
}

#[test]
fn seeded_sampling_is_reproducible() {
    let s = Spec::llama();
    let model = Model::from_bytes(gguf(&s, &weights(&s, 13))).unwrap();
    let run = |seed| {
        let mut sess = Session::new(&model, 16).unwrap();
        sess.generate(&[1], 10, &mut Sampler::new(1.0, 0, 1.0, seed), &[])
            .unwrap()
    };
    assert_eq!(run(3), run(3));
}

#[test]
fn loading_errors_are_clear() {
    let s = Spec::llama();
    let w = weights(&s, 14);
    // unsupported architecture
    let mut g = GgufWriter::new();
    g.add_metadata("general.architecture", Value::String("mamba".into()));
    assert!(matches!(
        Model::from_bytes(g.finish().unwrap()),
        Err(LlmError::UnsupportedArch(_))
    ));
    // metadata promises a third layer that the file does not contain
    let layers = Spec {
        meta_layer: Some(3),
        ..s.clone()
    };
    assert!(matches!(
        Model::from_bytes(gguf(&layers, &w)),
        Err(LlmError::MissingTensor(_))
    ));
    // head count that does not divide the embedding width
    let heads = Spec {
        meta_head: Some(5),
        ..s.clone()
    };
    assert!(matches!(
        Model::from_bytes(gguf(&heads, &w)),
        Err(LlmError::BadConfig(_))
    ));
    // feed-forward width that disagrees with the tensors
    let ff = Spec {
        meta_ff: Some(25),
        ..s.clone()
    };
    assert!(matches!(
        Model::from_bytes(gguf(&ff, &w)),
        Err(LlmError::BadTensorShape(_))
    ));
    // garbage
    assert!(matches!(
        Model::from_bytes(vec![1, 2, 3]),
        Err(LlmError::Gguf(_))
    ));
}

#[test]
fn open_from_a_file_matches_from_bytes() {
    let s = Spec::llama();
    let bytes = gguf(&s, &weights(&s, 15));
    let path = std::env::temp_dir().join(format!("aeon-llm-test-{}.gguf", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let a = Model::from_bytes(bytes).unwrap();
    let b = Model::open(&path).unwrap();
    let (mut sa, mut sb) = (Session::new(&a, 16).unwrap(), Session::new(&b, 16).unwrap());
    for t in [1u32, 8, 3] {
        assert_eq!(sa.forward(t).unwrap(), sb.forward(t).unwrap());
    }
    assert_eq!(a.config(), b.config());
    drop(b);
    std::fs::remove_file(path).unwrap();
}
