//! Speed benchmark: `cargo run --release -p mouli-llm --example bench -- MODEL.gguf [PROMPT_TOKENS] [GEN_TOKENS] [REPEATS]`
//!
//! Measures prompt processing and greedy generation in tokens per second on synthetic token ids,
//! like `llama-bench` (`-p`, `-n`), so the two can be compared on one machine. It prints the
//! first generated tokens as a fingerprint: the same model must print the same ones before and
//! after any speed change.

use mouli_llm::{Model, Sampler, Session};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let Some(path) = a.get(1) else {
        eprintln!("usage: bench MODEL.gguf [PROMPT_TOKENS=64] [GEN_TOKENS=16] [REPEATS=2]");
        std::process::exit(2);
    };
    let num = |i: usize, d: usize| a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let (np, ng, reps) = (num(2, 64), num(3, 16), num(4, 2));
    let t = Instant::now();
    let model = Model::open(path).expect("open model");
    println!(
        "opened in {:.2}s: {:?}",
        t.elapsed().as_secs_f64(),
        model.config().n_layer
    );
    let vocab = model.config().n_vocab as u32;
    let prompt: Vec<u32> = (0..np as u32)
        .map(|i| 1 + (i * 7919) % (vocab - 1))
        .collect();
    let (mut pp, mut tg) = (Vec::new(), Vec::new());
    let mut fingerprint = Vec::new();
    for _ in 0..reps {
        let mut s = Session::new(&model, np + ng + 1).expect("session");
        let t = Instant::now();
        let mut logits = s.prefill(&prompt).expect("prefill").to_vec();
        pp.push(np as f64 / t.elapsed().as_secs_f64());
        let t = Instant::now();
        let mut out = Vec::new();
        for _ in 0..ng {
            let next = Sampler::greedy().sample(&logits);
            out.push(next);
            logits = s.forward(next).expect("forward").to_vec();
        }
        tg.push(ng as f64 / t.elapsed().as_secs_f64());
        fingerprint = out;
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    println!(
        "pp{np}: {:.2} t/s   tg{ng}: {:.2} t/s   (mean of {reps})",
        mean(&pp),
        mean(&tg)
    );
    println!(
        "fingerprint: {:?}",
        &fingerprint[..fingerprint.len().min(12)]
    );
}
