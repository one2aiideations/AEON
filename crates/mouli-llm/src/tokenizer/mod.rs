//! Tokenizers built from GGUF metadata.
//!
//! Two families, selected by `tokenizer.ggml.model`:
//!
//! * `"llama"`: SentencePiece-style. Text gets a dummy prefix and spaces become `▁`; adjacent
//!   symbols merge greedily by vocabulary score; characters missing from the vocabulary fall back
//!   to byte tokens `<0xXX>` (or the unknown token when those are absent too).
//! * `"gpt2"`: byte-level BPE. A GPT-2 style pre-tokenizer splits text into words, bytes map to
//!   printable characters, and symbols merge by merge rank from `tokenizer.ggml.merges`.
//!
//! Written from the algorithm descriptions and the GGUF key list, not from other implementations.
//!
//! # Limits (read before relying on it)
//!
//! * The BPE pre-tokenizer is the GPT-2 pattern. Models that define a different pre-tokenizer
//!   (`tokenizer.ggml.pre`, e.g. llama-3 or qwen2 families) will split some inputs differently,
//!   so token ids can differ from the reference tokenizer for those models. The key is exposed
//!   as [`Tokenizer::pre_tokenizer_name`] so callers can detect that case.
//! * Unicode classes use Rust's `char::is_alphabetic` / `is_numeric`, not full `\p{L}` / `\p{N}`.
//! * No comparison against a reference tokenizer on real models has been done yet.

mod api;
mod bpe;
mod spm;

pub use api::{TokenKind, Tokenizer, TokenizerConfig, TokenizerError};
