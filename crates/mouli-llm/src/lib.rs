//! CPU forward pass for llama-family models stored as GGUF (engine milestone E2).
//!
//! Supports architectures `llama` (interleaved RoPE) and `qwen2` (split-half RoPE, QKV biases),
//! dense feed-forward only. Weights stay quantised in the file bytes and are dequantised row by
//! row inside each matrix-vector product, so memory use is the model file plus the KV cache.
//!
//! # Verification status
//!
//! Tests compare the engine with an independent naive implementation on tiny synthetic models
//! and check quantised-vs-f32 closeness. They do **not** compare logits with a reference engine
//! on a real model; milestone E2 requires that before outputs are trusted. Speed is scalar and
//! single-threaded: this is the correctness baseline for later SIMD work (E3).

pub mod gguf;
pub mod kernels;
pub mod quant;
pub mod tensor;
pub mod tokenizer;
pub mod weights;

mod config;
mod model;
mod sampler;
mod session;

pub use config::{Config, LlmError, RopeStyle};
pub use model::Model;
pub use sampler::Sampler;
pub use session::Session;
