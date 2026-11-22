//! Model hyper-parameters read from GGUF metadata.

use aeon_gguf::{Gguf, GgufError};
use aeon_quant::QuantError;
use std::fmt;

/// How rotary position embedding pairs up dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RopeStyle {
    /// Adjacent pairs `(2i, 2i + 1)` (llama).
    Interleaved,
    /// Split halves `(i, i + dim/2)` (qwen2, "neox").
    SplitHalf,
}

/// Failure while loading or running a model.
#[derive(Debug)]
pub enum LlmError {
    /// The GGUF file is invalid or lacks a key.
    Gguf(GgufError),
    /// Dequantisation failed (unsupported tensor type or bad sizes).
    Quant(QuantError),
    /// The architecture is not supported.
    UnsupportedArch(String),
    /// A hyper-parameter is inconsistent (the text says which).
    BadConfig(&'static str),
    /// A required tensor is missing.
    MissingTensor(String),
    /// A tensor has the wrong shape.
    BadTensorShape(String),
    /// A token id is outside the vocabulary.
    BadToken(u32),
    /// The context window is full.
    ContextFull,
    /// A computation produced a non-finite value.
    NonFinite,
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::Gguf(e) => write!(f, "gguf: {e}"),
            LlmError::Quant(e) => write!(f, "quantisation: {e}"),
            LlmError::UnsupportedArch(a) => write!(f, "unsupported architecture {a:?}"),
            LlmError::BadConfig(m) => write!(f, "bad configuration: {m}"),
            LlmError::MissingTensor(n) => write!(f, "missing tensor {n:?}"),
            LlmError::BadTensorShape(n) => write!(f, "tensor {n:?} has an unexpected shape"),
            LlmError::BadToken(t) => write!(f, "token {t} is outside the vocabulary"),
            LlmError::ContextFull => f.write_str("context window is full"),
            LlmError::NonFinite => f.write_str("non-finite value in the forward pass"),
        }
    }
}

impl std::error::Error for LlmError {}

impl From<GgufError> for LlmError {
    fn from(e: GgufError) -> Self {
        LlmError::Gguf(e)
    }
}

impl From<QuantError> for LlmError {
    fn from(e: QuantError) -> Self {
        LlmError::Quant(e)
    }
}

/// Hyper-parameters of a decoder-only transformer.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Architecture name from `general.architecture`.
    pub arch: String,
    /// Embedding width.
    pub n_embd: usize,
    /// Number of layers.
    pub n_layer: usize,
    /// Attention heads.
    pub n_head: usize,
    /// Key/value heads (grouped-query attention when smaller than `n_head`).
    pub n_head_kv: usize,
    /// Width of one head.
    pub head_dim: usize,
    /// Feed-forward inner width.
    pub n_ff: usize,
    /// Trained context length.
    pub n_ctx: usize,
    /// Vocabulary size (rows of the embedding matrix).
    pub n_vocab: usize,
    /// RoPE frequency base.
    pub rope_base: f32,
    /// Number of dimensions per head that rotate (at most `head_dim`, even).
    pub rope_dim: usize,
    /// RMSNorm epsilon.
    pub rms_eps: f32,
    /// RoPE pairing.
    pub rope_style: RopeStyle,
    /// Whether Q, K and V projections carry biases.
    pub qkv_bias: bool,
}

impl Config {
    /// Read the hyper-parameters. `n_vocab` comes from the shape of `token_embd.weight`.
    pub fn from_gguf(g: &Gguf) -> Result<Config, LlmError> {
        let arch = g.architecture()?.to_string();
        let (rope_style, qkv_bias, default_eps) = match arch.as_str() {
            "llama" => (RopeStyle::Interleaved, false, 1e-5),
            "qwen2" => (RopeStyle::SplitHalf, true, 1e-6),
            other => return Err(LlmError::UnsupportedArch(other.to_string())),
        };
        let n_embd = g.arch_u32("embedding_length")? as usize;
        let n_layer = g.arch_u32("block_count")? as usize;
        let n_head = g.arch_u32("attention.head_count")? as usize;
        let n_head_kv = g
            .arch_u32("attention.head_count_kv")
            .map_or(n_head, |v| v as usize);
        let n_ff = g.arch_u32("feed_forward_length")? as usize;
        let n_ctx = g.arch_u32("context_length")? as usize;
        let key = |s: &str| format!("{arch}.{s}");
        let rope_base = g.get_f32(&key("rope.freq_base")).unwrap_or(10_000.0);
        let rms_eps = g
            .get_f32(&key("attention.layer_norm_rms_epsilon"))
            .unwrap_or(default_eps);
        let embd = g
            .tensor("token_embd.weight")
            .ok_or_else(|| LlmError::MissingTensor("token_embd.weight".into()))?;
        if embd.dims.len() != 2 || embd.dims[0] as usize != n_embd {
            return Err(LlmError::BadTensorShape("token_embd.weight".into()));
        }
        let n_vocab = embd.dims[1] as usize;
        if n_head == 0 || n_head_kv == 0 || n_layer == 0 || n_embd == 0 {
            return Err(LlmError::BadConfig("zero-sized dimension"));
        }
        if !n_embd.is_multiple_of(n_head) {
            return Err(LlmError::BadConfig(
                "embedding length is not a multiple of the head count",
            ));
        }
        if !n_head.is_multiple_of(n_head_kv) {
            return Err(LlmError::BadConfig(
                "head count is not a multiple of the kv head count",
            ));
        }
        let head_dim = n_embd / n_head;
        let rope_dim = g
            .arch_u32("rope.dimension_count")
            .map_or(head_dim, |v| v as usize);
        if rope_dim == 0 || rope_dim > head_dim || !rope_dim.is_multiple_of(2) {
            return Err(LlmError::BadConfig(
                "rope dimension count must be even and at most the head width",
            ));
        }
        if !rope_base.is_finite() || rope_base <= 0.0 || !rms_eps.is_finite() || rms_eps < 0.0 {
            return Err(LlmError::BadConfig("rope base or epsilon out of range"));
        }
        Ok(Config {
            arch,
            n_embd,
            n_layer,
            n_head,
            n_head_kv,
            head_dim,
            n_ff,
            n_ctx,
            n_vocab,
            rope_base,
            rope_dim,
            rms_eps,
            rope_style,
            qkv_bias,
        })
    }

    /// Width of the key (and value) projection output.
    pub fn kv_dim(&self) -> usize {
        self.n_head_kv * self.head_dim
    }
}
