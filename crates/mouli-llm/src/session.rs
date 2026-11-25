//! One generation session: KV cache, position and the per-token forward pass.

use crate::config::{LlmError, RopeStyle};
use crate::model::Model;
use crate::sampler::Sampler;
use mouli_engine::ops;

/// State of one conversation with a [`Model`]: the KV cache and the next position.
pub struct Session<'m> {
    model: &'m Model,
    limit: usize,
    pos: usize,
    /// Keys, laid out `[layer][kv_head][position][head_dim]`.
    k: Vec<f32>,
    /// Values, same layout.
    v: Vec<f32>,
    x: Vec<f32>,
    xn: Vec<f32>,
    q: Vec<f32>,
    kbuf: Vec<f32>,
    vbuf: Vec<f32>,
    attn: Vec<f32>,
    gate: Vec<f32>,
    up: Vec<f32>,
    hidden: Vec<f32>,
    logits: Vec<f32>,
    scores: Vec<f32>,
}

/// Rotate the first `rope_dim` dimensions of every head.
fn rope(
    v: &mut [f32],
    heads: usize,
    head_dim: usize,
    rope_dim: usize,
    pos: usize,
    base: f32,
    style: RopeStyle,
) {
    for h in 0..heads {
        let o = h * head_dim;
        for i in 0..rope_dim / 2 {
            let freq = base.powf(-((2 * i) as f32) / rope_dim as f32);
            let (s, c) = (pos as f32 * freq).sin_cos();
            let (a, b) = match style {
                RopeStyle::Interleaved => (o + 2 * i, o + 2 * i + 1),
                RopeStyle::SplitHalf => (o + i, o + i + rope_dim / 2),
            };
            let (x0, x1) = (v[a], v[b]);
            v[a] = x0 * c - x1 * s;
            v[b] = x0 * s + x1 * c;
        }
    }
}

fn map_op<T>(r: Result<T, mouli_engine::OpError>) -> Result<T, LlmError> {
    r.map_err(|_| LlmError::NonFinite)
}

impl<'m> Session<'m> {
    /// A fresh session that can hold up to `max_ctx` tokens (`1..=` the trained context).
    pub fn new(model: &'m Model, max_ctx: usize) -> Result<Session<'m>, LlmError> {
        let c = model.config();
        if max_ctx == 0 || max_ctx > c.n_ctx {
            return Err(LlmError::BadConfig(
                "max_ctx must be between 1 and the trained context length",
            ));
        }
        let cache = c.n_layer * c.n_head_kv * max_ctx * c.head_dim;
        let q_dim = c.n_head * c.head_dim;
        Ok(Session {
            model,
            limit: max_ctx,
            pos: 0,
            k: vec![0.0; cache],
            v: vec![0.0; cache],
            x: vec![0.0; c.n_embd],
            xn: vec![0.0; c.n_embd],
            q: vec![0.0; q_dim],
            kbuf: vec![0.0; c.kv_dim()],
            vbuf: vec![0.0; c.kv_dim()],
            attn: vec![0.0; q_dim],
            gate: vec![0.0; c.n_ff],
            up: vec![0.0; c.n_ff],
            hidden: vec![0.0; c.n_ff],
            logits: vec![0.0; c.n_vocab],
            scores: vec![0.0; max_ctx],
        })
    }

    /// Tokens processed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Tokens this session can still take.
    pub fn remaining(&self) -> usize {
        self.limit - self.pos
    }

    /// Forget everything and start again from position 0.
    pub fn reset(&mut self) {
        self.pos = 0;
    }

    fn cache_base(&self, layer: usize, kv_head: usize) -> usize {
        let c = self.model.config();
        ((layer * c.n_head_kv + kv_head) * self.limit) * c.head_dim
    }

    /// Process one token and return the logits for the next one.
    pub fn forward(&mut self, token: u32) -> Result<&[f32], LlmError> {
        let m = self.model;
        let c = m.config();
        if self.pos >= self.limit {
            return Err(LlmError::ContextFull);
        }
        let pos = self.pos;
        m.embed(token, &mut self.x)?;
        let group = c.n_head / c.n_head_kv;
        let scale = 1.0 / (c.head_dim as f32).sqrt();

        for (l, layer) in m.layers.iter().enumerate() {
            map_op(ops::rmsnorm(
                &self.x,
                &layer.attn_norm,
                c.rms_eps,
                &mut self.xn,
            ))?;
            m.matvec(&layer.wq, &self.xn, &mut self.q)?;
            m.matvec(&layer.wk, &self.xn, &mut self.kbuf)?;
            m.matvec(&layer.wv, &self.xn, &mut self.vbuf)?;
            for (buf, bias) in [
                (&mut self.q, &layer.bq),
                (&mut self.kbuf, &layer.bk),
                (&mut self.vbuf, &layer.bv),
            ] {
                if let Some(b) = bias {
                    for (v, b) in buf.iter_mut().zip(b) {
                        *v += b;
                    }
                }
            }
            rope(
                &mut self.q,
                c.n_head,
                c.head_dim,
                c.rope_dim,
                pos,
                c.rope_base,
                c.rope_style,
            );
            rope(
                &mut self.kbuf,
                c.n_head_kv,
                c.head_dim,
                c.rope_dim,
                pos,
                c.rope_base,
                c.rope_style,
            );

            for h in 0..c.n_head_kv {
                let at = self.cache_base(l, h) + pos * c.head_dim;
                self.k[at..at + c.head_dim]
                    .copy_from_slice(&self.kbuf[h * c.head_dim..(h + 1) * c.head_dim]);
                self.v[at..at + c.head_dim]
                    .copy_from_slice(&self.vbuf[h * c.head_dim..(h + 1) * c.head_dim]);
            }
            for h in 0..c.n_head {
                let base = self.cache_base(l, h / group);
                let len = (pos + 1) * c.head_dim;
                map_op(ops::attention(
                    &self.q[h * c.head_dim..(h + 1) * c.head_dim],
                    &self.k[base..base + len],
                    &self.v[base..base + len],
                    pos + 1,
                    scale,
                    &mut self.scores,
                    &mut self.attn[h * c.head_dim..(h + 1) * c.head_dim],
                ))?;
            }
            m.matvec(&layer.wo, &self.attn, &mut self.xn)?;
            for (x, o) in self.x.iter_mut().zip(&self.xn) {
                *x += o;
            }

            map_op(ops::rmsnorm(
                &self.x,
                &layer.ffn_norm,
                c.rms_eps,
                &mut self.xn,
            ))?;
            m.matvec(&layer.w_gate, &self.xn, &mut self.gate)?;
            m.matvec(&layer.w_up, &self.xn, &mut self.up)?;
            map_op(ops::swiglu(&self.gate, &self.up, &mut self.hidden))?;
            m.matvec(&layer.w_down, &self.hidden, &mut self.xn)?;
            for (x, o) in self.x.iter_mut().zip(&self.xn) {
                *x += o;
            }
        }

        map_op(ops::rmsnorm(
            &self.x,
            &m.output_norm,
            c.rms_eps,
            &mut self.xn,
        ))?;
        m.matvec(&m.output, &self.xn, &mut self.logits)?;
        if self.logits.iter().any(|v| !v.is_finite()) {
            return Err(LlmError::NonFinite);
        }
        self.pos += 1;
        Ok(&self.logits)
    }

    /// Process several tokens and return the logits after the last one.
    pub fn prefill(&mut self, tokens: &[u32]) -> Result<&[f32], LlmError> {
        if tokens.is_empty() {
            return Err(LlmError::BadConfig("prefill needs at least one token"));
        }
        if tokens.len() > self.remaining() {
            return Err(LlmError::ContextFull);
        }
        for t in &tokens[..tokens.len() - 1] {
            self.forward(*t)?;
        }
        self.forward(tokens[tokens.len() - 1])
    }

    /// Feed `prompt`, then sample up to `max_new` tokens, stopping early on a token in `stop`
    /// (the stop token is not included). Stops at the end of the context window.
    pub fn generate(
        &mut self,
        prompt: &[u32],
        max_new: usize,
        sampler: &mut Sampler,
        stop: &[u32],
    ) -> Result<Vec<u32>, LlmError> {
        let mut logits = self.prefill(prompt)?.to_vec();
        let mut out = Vec::new();
        for _ in 0..max_new {
            let next = sampler.sample(&logits);
            if stop.contains(&next) {
                break;
            }
            out.push(next);
            if self.remaining() == 0 {
                break;
            }
            logits = self.forward(next)?.to_vec();
        }
        Ok(out)
    }
}
