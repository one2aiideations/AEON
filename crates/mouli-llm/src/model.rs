//! Weights: tensor lookup, shape checks and quantised matrix-vector products.

use crate::config::{Config, LlmError};
use mouli_gguf::types::GgmlType;
use mouli_gguf::{parse, Gguf, MappedGguf};
use std::path::Path;

enum Backing {
    Owned(Vec<u8>),
    Mapped(MappedGguf),
}

/// A 2-D weight matrix inside the file bytes: `rows` output rows of `cols` inputs each.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mat {
    pub(crate) ty: GgmlType,
    start: usize,
    end: usize,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
}

pub(crate) struct Layer {
    pub(crate) attn_norm: Vec<f32>,
    pub(crate) wq: Mat,
    pub(crate) wk: Mat,
    pub(crate) wv: Mat,
    pub(crate) wo: Mat,
    pub(crate) bq: Option<Vec<f32>>,
    pub(crate) bk: Option<Vec<f32>>,
    pub(crate) bv: Option<Vec<f32>>,
    pub(crate) ffn_norm: Vec<f32>,
    pub(crate) w_gate: Mat,
    pub(crate) w_up: Mat,
    pub(crate) w_down: Mat,
}

/// A loaded model: configuration plus weight locations in the GGUF bytes.
pub struct Model {
    cfg: Config,
    gguf: Gguf,
    backing: Backing,
    pub(crate) layers: Vec<Layer>,
    pub(crate) token_embd: Mat,
    pub(crate) output_norm: Vec<f32>,
    pub(crate) output: Mat,
}

impl Model {
    /// Load from file bytes held in memory.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Model, LlmError> {
        let gguf = parse(&bytes)?;
        Model::build(gguf, Backing::Owned(bytes))
    }

    /// Memory-map and load the GGUF file at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Model, LlmError> {
        let mapped = MappedGguf::open(path)?;
        let gguf = mapped.gguf().clone();
        Model::build(gguf, Backing::Mapped(mapped))
    }

    /// The hyper-parameters.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// The parsed GGUF table (metadata and tensor infos), e.g. to build a tokenizer.
    pub fn gguf(&self) -> &Gguf {
        &self.gguf
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(v) => v,
            Backing::Mapped(m) => m.bytes(),
        }
    }

    fn mat(
        g: &Gguf,
        bytes_len: usize,
        name: &str,
        cols: usize,
        rows: usize,
    ) -> Result<Mat, LlmError> {
        let t = g
            .tensor(name)
            .ok_or_else(|| LlmError::MissingTensor(name.to_string()))?;
        if t.dims.len() != 2 || t.dims[0] as usize != cols || t.dims[1] as usize != rows {
            return Err(LlmError::BadTensorShape(name.to_string()));
        }
        if !mouli_quant::is_supported(t.ty) {
            return Err(LlmError::Quant(mouli_quant::QuantError::Unsupported(t.ty)));
        }
        let r = t.data_range(g.data_start)?;
        let (start, end) = (r.start as usize, r.end as usize);
        if end > bytes_len {
            return Err(LlmError::BadTensorShape(name.to_string()));
        }
        Ok(Mat {
            ty: t.ty,
            start,
            end,
            rows,
            cols,
        })
    }

    fn vector(g: &Gguf, bytes: &[u8], name: &str, len: usize) -> Result<Vec<f32>, LlmError> {
        let t = g
            .tensor(name)
            .ok_or_else(|| LlmError::MissingTensor(name.to_string()))?;
        if t.dims.len() != 1 || t.dims[0] as usize != len {
            return Err(LlmError::BadTensorShape(name.to_string()));
        }
        let data = g.tensor_data(bytes, name)?;
        Ok(mouli_quant::dequantize_vec(t.ty, data, len)?)
    }

    fn build(gguf: Gguf, backing: Backing) -> Result<Model, LlmError> {
        let cfg = Config::from_gguf(&gguf)?;
        let bytes: &[u8] = match &backing {
            Backing::Owned(v) => v,
            Backing::Mapped(m) => m.bytes(),
        };
        let g = &gguf;
        let n = bytes.len();
        let (e, kv, ff) = (cfg.n_embd, cfg.kv_dim(), cfg.n_ff);
        let q_dim = cfg.n_head * cfg.head_dim;
        let token_embd = Model::mat(g, n, "token_embd.weight", e, cfg.n_vocab)?;
        let output_norm = Model::vector(g, bytes, "output_norm.weight", e)?;
        // Tied embeddings: no separate output matrix.
        let output = if g.tensor("output.weight").is_some() {
            Model::mat(g, n, "output.weight", e, cfg.n_vocab)?
        } else {
            token_embd
        };
        let mut layers = Vec::with_capacity(cfg.n_layer);
        for l in 0..cfg.n_layer {
            let p = |s: &str| format!("blk.{l}.{s}");
            let bias = |name: &str, len: usize| -> Result<Option<Vec<f32>>, LlmError> {
                if cfg.qkv_bias {
                    Ok(Some(Model::vector(g, bytes, &p(name), len)?))
                } else {
                    Ok(None)
                }
            };
            layers.push(Layer {
                attn_norm: Model::vector(g, bytes, &p("attn_norm.weight"), e)?,
                wq: Model::mat(g, n, &p("attn_q.weight"), e, q_dim)?,
                wk: Model::mat(g, n, &p("attn_k.weight"), e, kv)?,
                wv: Model::mat(g, n, &p("attn_v.weight"), e, kv)?,
                wo: Model::mat(g, n, &p("attn_output.weight"), q_dim, e)?,
                bq: bias("attn_q.bias", q_dim)?,
                bk: bias("attn_k.bias", kv)?,
                bv: bias("attn_v.bias", kv)?,
                ffn_norm: Model::vector(g, bytes, &p("ffn_norm.weight"), e)?,
                w_gate: Model::mat(g, n, &p("ffn_gate.weight"), e, ff)?,
                w_up: Model::mat(g, n, &p("ffn_up.weight"), e, ff)?,
                w_down: Model::mat(g, n, &p("ffn_down.weight"), ff, e)?,
            });
        }
        Ok(Model {
            cfg,
            gguf,
            backing,
            layers,
            token_embd,
            output_norm,
            output,
        })
    }

    /// `out = m * x` with on-the-fly dequantisation.
    pub(crate) fn matvec(&self, m: &Mat, x: &[f32], out: &mut [f32]) -> Result<(), LlmError> {
        Ok(mouli_quant::matvec(
            m.ty,
            &self.bytes()[m.start..m.end],
            m.rows,
            m.cols,
            x,
            out,
        )?)
    }

    /// The embedding row of `token`.
    pub(crate) fn embed(&self, token: u32, out: &mut [f32]) -> Result<(), LlmError> {
        let m = &self.token_embd;
        let row = token as usize;
        if row >= m.rows {
            return Err(LlmError::BadToken(token));
        }
        let rb = mouli_quant::byte_len(m.ty, m.cols)?;
        let start = m.start + row * rb;
        Ok(mouli_quant::dequantize(
            m.ty,
            &self.bytes()[start..start + rb],
            out,
        )?)
    }
}
