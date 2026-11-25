//! Checks that reading a prompt in chunks gives the same answers as one token at a time:
//! `cargo run --release -p mouli-llm --example equivalence -- MODEL.gguf`.
use mouli_llm::{Model, Session};
fn main() {
    let path = std::env::args().nth(1).expect("model");
    let model = Model::open(&path).expect("open");
    let vocab = model.config().n_vocab as u32;
    let prompt: Vec<u32> = (0..70u32).map(|i| 1 + (i * 7919) % (vocab - 1)).collect();
    let mut a = Session::new(&model, 100).unwrap();
    let la = a.prefill(&prompt).unwrap().to_vec();
    let mut b = Session::new(&model, 100).unwrap();
    let mut lb = Vec::new();
    for t in &prompt {
        lb = b.forward(*t).unwrap().to_vec();
    }
    let diff = la
        .iter()
        .zip(&lb)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    let scale = lb.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    let arg = |v: &[f32]| {
        v.iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0
    };
    println!("max diff {diff:e} (logit scale {scale}), argmax chunked {} vs sequential {}, positions {} {}", arg(&la), arg(&lb), a.position(), b.position());
    // The caches must also agree: the next token's logits come out the same way.
    let na = a.forward(5).unwrap().to_vec();
    let nb = b.forward(5).unwrap().to_vec();
    let d2 = na
        .iter()
        .zip(&nb)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    println!(
        "next-token diff {d2:e}, argmax {} vs {}",
        arg(&na),
        arg(&nb)
    );
}
