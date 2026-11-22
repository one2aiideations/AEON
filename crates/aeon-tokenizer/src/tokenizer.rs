//! Shared vocabulary, construction from GGUF, and the public encode/decode API.

use crate::{bpe, spm};
use aeon_gguf::{Array, Gguf};
use std::collections::HashMap;
use std::fmt;

/// Token categories (`tokenizer.ggml.token_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Ordinary text piece.
    Normal,
    /// The unknown token.
    Unknown,
    /// Control token such as `<s>`.
    Control,
    /// User-defined special token, matched in text when special parsing is on.
    UserDefined,
    /// Unused slot.
    Unused,
    /// Byte token `<0xXX>`.
    Byte,
}

impl TokenKind {
    fn from_i32(v: i32) -> TokenKind {
        match v {
            2 => TokenKind::Unknown,
            3 => TokenKind::Control,
            4 => TokenKind::UserDefined,
            5 => TokenKind::Unused,
            6 => TokenKind::Byte,
            _ => TokenKind::Normal,
        }
    }
}

/// Tokenizer construction failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenizerError {
    /// A required metadata key is missing or has the wrong type.
    Metadata(String),
    /// `tokenizer.ggml.model` is not one this crate implements.
    UnsupportedModel(String),
    /// The vocabulary is empty or has duplicate token strings.
    BadVocabulary(String),
    /// A special-token id is outside the vocabulary.
    BadTokenId(u32),
}

impl fmt::Display for TokenizerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenizerError::Metadata(m) => write!(f, "tokenizer metadata: {m}"),
            TokenizerError::UnsupportedModel(m) => write!(f, "unsupported tokenizer model {m:?}"),
            TokenizerError::BadVocabulary(m) => write!(f, "bad vocabulary: {m}"),
            TokenizerError::BadTokenId(i) => write!(f, "token id {i} is outside the vocabulary"),
        }
    }
}

impl std::error::Error for TokenizerError {}

/// Everything needed to build a tokenizer without a GGUF file.
#[derive(Debug, Clone)]
pub struct TokenizerConfig {
    /// `"llama"` (SentencePiece-style) or `"gpt2"` (byte-level BPE).
    pub model: String,
    /// Token strings, indexed by id.
    pub tokens: Vec<String>,
    /// Scores (SentencePiece); empty means all zero.
    pub scores: Vec<f32>,
    /// Token types as in GGUF; empty means all normal.
    pub types: Vec<i32>,
    /// BPE merges as `"left right"`, best first.
    pub merges: Vec<String>,
    /// Beginning-of-sequence id.
    pub bos: Option<u32>,
    /// End-of-sequence id.
    pub eos: Option<u32>,
    /// Unknown-token id.
    pub unk: Option<u32>,
    /// Padding id.
    pub pad: Option<u32>,
    /// Whether to prepend BOS by default.
    pub add_bos: bool,
    /// Whether SentencePiece adds a dummy `▁` prefix (default true).
    pub add_space_prefix: bool,
    /// `tokenizer.ggml.pre`, if present.
    pub pre: Option<String>,
}

enum Algo {
    Spm,
    Bpe { ranks: HashMap<String, usize> },
}

/// A ready-to-use tokenizer.
pub struct Tokenizer {
    tokens: Vec<String>,
    scores: Vec<f32>,
    kinds: Vec<TokenKind>,
    index: HashMap<String, u32>,
    /// Byte value for each byte token id (SentencePiece).
    byte_ids: [Option<u32>; 256],
    /// User-defined and control tokens, longest first, for special parsing.
    specials: Vec<(String, u32)>,
    algo: Algo,
    bos: Option<u32>,
    eos: Option<u32>,
    unk: Option<u32>,
    pad: Option<u32>,
    add_bos: bool,
    add_space_prefix: bool,
    pre: Option<String>,
}

fn strings(a: &Array, what: &str) -> Result<Vec<String>, TokenizerError> {
    a.as_strings()
        .map(<[String]>::to_vec)
        .ok_or_else(|| TokenizerError::Metadata(format!("{what} is not a string array")))
}

impl Tokenizer {
    /// Build from GGUF metadata (`tokenizer.ggml.*`).
    pub fn from_gguf(g: &Gguf) -> Result<Self, TokenizerError> {
        let model = g
            .get_str("tokenizer.ggml.model")
            .map_err(|e| TokenizerError::Metadata(e.to_string()))?
            .to_string();
        let tokens = strings(
            g.get_array("tokenizer.ggml.tokens")
                .map_err(|e| TokenizerError::Metadata(e.to_string()))?,
            "tokenizer.ggml.tokens",
        )?;
        let scores = match g.get("tokenizer.ggml.scores").and_then(|v| v.as_array()) {
            Some(a) => a
                .as_f32s()
                .map(<[f32]>::to_vec)
                .ok_or_else(|| TokenizerError::Metadata("scores is not an f32 array".into()))?,
            None => Vec::new(),
        };
        let types = match g
            .get("tokenizer.ggml.token_type")
            .and_then(|v| v.as_array())
        {
            Some(a) => a
                .as_i32s()
                .map(<[i32]>::to_vec)
                .ok_or_else(|| TokenizerError::Metadata("token_type is not an i32 array".into()))?,
            None => Vec::new(),
        };
        let merges = match g.get("tokenizer.ggml.merges").and_then(|v| v.as_array()) {
            Some(a) => strings(a, "tokenizer.ggml.merges")?,
            None => Vec::new(),
        };
        let id = |k: &str| g.get(k).and_then(|v| v.as_u32());
        let cfg = TokenizerConfig {
            model,
            tokens,
            scores,
            types,
            merges,
            bos: id("tokenizer.ggml.bos_token_id"),
            eos: id("tokenizer.ggml.eos_token_id"),
            unk: id("tokenizer.ggml.unknown_token_id"),
            pad: id("tokenizer.ggml.padding_token_id"),
            add_bos: g
                .get("tokenizer.ggml.add_bos_token")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            add_space_prefix: g
                .get("tokenizer.ggml.add_space_prefix")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            pre: g
                .get("tokenizer.ggml.pre")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        };
        Tokenizer::new(cfg)
    }

    /// Build from an explicit configuration.
    pub fn new(cfg: TokenizerConfig) -> Result<Self, TokenizerError> {
        if cfg.tokens.is_empty() {
            return Err(TokenizerError::BadVocabulary("empty vocabulary".into()));
        }
        let n = u32::try_from(cfg.tokens.len())
            .map_err(|_| TokenizerError::BadVocabulary("too many tokens".into()))?;
        for id in [cfg.bos, cfg.eos, cfg.unk, cfg.pad].into_iter().flatten() {
            if id >= n {
                return Err(TokenizerError::BadTokenId(id));
            }
        }
        let mut index = HashMap::with_capacity(cfg.tokens.len());
        for (i, t) in cfg.tokens.iter().enumerate() {
            // Duplicates exist in some real vocabularies; the first id wins for encoding.
            index.entry(t.clone()).or_insert(i as u32);
        }
        let kinds: Vec<TokenKind> = (0..cfg.tokens.len())
            .map(|i| {
                cfg.types
                    .get(i)
                    .copied()
                    .map_or(TokenKind::Normal, TokenKind::from_i32)
            })
            .collect();
        let scores: Vec<f32> = (0..cfg.tokens.len())
            .map(|i| cfg.scores.get(i).copied().unwrap_or(0.0))
            .collect();

        let mut byte_ids = [None; 256];
        for (i, t) in cfg.tokens.iter().enumerate() {
            if let Some(b) = parse_byte_token(t) {
                if byte_ids[usize::from(b)].is_none() {
                    byte_ids[usize::from(b)] = Some(i as u32);
                }
            }
        }
        let mut specials: Vec<(String, u32)> = cfg
            .tokens
            .iter()
            .enumerate()
            .filter(|(i, t)| {
                matches!(kinds[*i], TokenKind::Control | TokenKind::UserDefined) && !t.is_empty()
            })
            .map(|(i, t)| (t.clone(), i as u32))
            .collect();
        specials.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.1.cmp(&b.1)));

        let algo = match cfg.model.as_str() {
            "llama" | "spm" => Algo::Spm,
            "gpt2" | "bpe" => {
                let mut ranks = HashMap::with_capacity(cfg.merges.len());
                for (rank, m) in cfg.merges.iter().enumerate() {
                    if !m.contains(' ') {
                        return Err(TokenizerError::BadVocabulary(format!(
                            "merge {rank} has no space"
                        )));
                    }
                    // Keyed by the merge line itself ("left right"); first occurrence wins.
                    ranks.entry(m.clone()).or_insert(rank);
                }
                Algo::Bpe { ranks }
            }
            other => return Err(TokenizerError::UnsupportedModel(other.to_string())),
        };
        Ok(Tokenizer {
            tokens: cfg.tokens,
            scores,
            kinds,
            index,
            byte_ids,
            specials,
            algo,
            bos: cfg.bos,
            eos: cfg.eos,
            unk: cfg.unk,
            pad: cfg.pad,
            add_bos: cfg.add_bos,
            add_space_prefix: cfg.add_space_prefix,
            pre: cfg.pre,
        })
    }

    /// Vocabulary size.
    pub fn vocab_size(&self) -> usize {
        self.tokens.len()
    }

    /// The text of token `id`.
    pub fn token(&self, id: u32) -> Option<&str> {
        self.tokens.get(id as usize).map(String::as_str)
    }

    /// The id of an exact token string.
    pub fn id_of(&self, token: &str) -> Option<u32> {
        self.index.get(token).copied()
    }

    /// The category of token `id`.
    pub fn kind(&self, id: u32) -> Option<TokenKind> {
        self.kinds.get(id as usize).copied()
    }

    /// Beginning-of-sequence id.
    pub fn bos(&self) -> Option<u32> {
        self.bos
    }

    /// End-of-sequence id.
    pub fn eos(&self) -> Option<u32> {
        self.eos
    }

    /// Unknown-token id.
    pub fn unk(&self) -> Option<u32> {
        self.unk
    }

    /// Padding id.
    pub fn pad(&self) -> Option<u32> {
        self.pad
    }

    /// Whether BOS is prepended by default.
    pub fn adds_bos(&self) -> bool {
        self.add_bos
    }

    /// `tokenizer.ggml.pre` from the file, if any. BPE here always uses the GPT-2 pattern, so a
    /// value other than `None` or `"gpt2"` means ids may differ from the reference tokenizer.
    pub fn pre_tokenizer_name(&self) -> Option<&str> {
        self.pre.as_deref()
    }

    /// Encode `text`. Prepends BOS if `add_bos` and the model has one. With `parse_special`,
    /// control and user-defined token strings inside the text become their single ids.
    pub fn encode(&self, text: &str, add_bos: bool, parse_special: bool) -> Vec<u32> {
        let mut out = Vec::new();
        if add_bos {
            if let Some(b) = self.bos {
                out.push(b);
            }
        }
        let mut first = true;
        for (is_special, piece) in self.split_specials(text, parse_special) {
            if is_special {
                if let Some(id) = self.index.get(piece).copied() {
                    out.push(id);
                }
            } else if !piece.is_empty() {
                match &self.algo {
                    Algo::Spm => spm::encode(self, piece, first && self.add_space_prefix, &mut out),
                    Algo::Bpe { ranks } => bpe::encode(self, ranks, piece, &mut out),
                }
            }
            first = false;
        }
        out
    }

    /// Encode with the model's default BOS behaviour and no special-token parsing.
    pub fn encode_default(&self, text: &str) -> Vec<u32> {
        self.encode(text, self.add_bos, false)
    }

    fn split_specials<'a>(&'a self, text: &'a str, parse: bool) -> Vec<(bool, &'a str)> {
        if !parse || self.specials.is_empty() {
            return vec![(false, text)];
        }
        let mut parts = Vec::new();
        let mut rest_start = 0;
        let mut i = 0;
        while i < text.len() {
            if !text.is_char_boundary(i) {
                i += 1;
                continue;
            }
            let hit = self
                .specials
                .iter()
                .find(|(s, _)| text[i..].starts_with(s.as_str()));
            if let Some((s, _)) = hit {
                if i > rest_start {
                    parts.push((false, &text[rest_start..i]));
                }
                parts.push((true, &text[i..i + s.len()]));
                i += s.len();
                rest_start = i;
            } else {
                i += 1;
            }
        }
        if rest_start < text.len() {
            parts.push((false, &text[rest_start..]));
        }
        parts
    }

    /// Decode token ids to text. Control and unused tokens produce nothing; invalid UTF-8
    /// (possible when a multi-byte character is split across tokens) is replaced with U+FFFD.
    pub fn decode(&self, ids: &[u32]) -> String {
        let bytes = self.decode_bytes(ids);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Decode token ids to raw bytes.
    pub fn decode_bytes(&self, ids: &[u32]) -> Vec<u8> {
        match &self.algo {
            Algo::Spm => spm::decode(self, ids),
            Algo::Bpe { .. } => bpe::decode(self, ids),
        }
    }

    // --- crate-internal access for the algorithm modules ---
    pub(crate) fn score_of(&self, id: u32) -> f32 {
        self.scores.get(id as usize).copied().unwrap_or(0.0)
    }

    pub(crate) fn lookup(&self, s: &str) -> Option<u32> {
        self.index.get(s).copied()
    }

    pub(crate) fn byte_id(&self, b: u8) -> Option<u32> {
        self.byte_ids[usize::from(b)]
    }

    pub(crate) fn raw(&self, id: u32) -> Option<&str> {
        self.token(id)
    }

    pub(crate) fn kind_of(&self, id: u32) -> TokenKind {
        self.kind(id).unwrap_or(TokenKind::Unused)
    }

    pub(crate) fn unknown_id(&self) -> Option<u32> {
        self.unk
    }

    pub(crate) fn space_prefix(&self) -> bool {
        self.add_space_prefix
    }
}

/// `<0xXX>` (uppercase hex) to its byte.
pub(crate) fn parse_byte_token(t: &str) -> Option<u8> {
    let h = t.strip_prefix("<0x")?.strip_suffix('>')?;
    if h.len() != 2
        || !h
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c))
    {
        return None;
    }
    u8::from_str_radix(h, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_tokens_parse_strictly() {
        assert_eq!(parse_byte_token("<0x00>"), Some(0));
        assert_eq!(parse_byte_token("<0xFF>"), Some(255));
        assert_eq!(parse_byte_token("<0x0a>"), None); // lowercase is not a byte token
        assert_eq!(parse_byte_token("<0x1>"), None);
        assert_eq!(parse_byte_token("<0x123>"), None);
        assert_eq!(parse_byte_token("0x41"), None);
        assert_eq!(parse_byte_token("<0xZZ>"), None);
    }

    fn cfg(model: &str, tokens: &[&str]) -> TokenizerConfig {
        TokenizerConfig {
            model: model.into(),
            tokens: tokens.iter().map(|s| s.to_string()).collect(),
            scores: vec![],
            types: vec![],
            merges: vec![],
            bos: None,
            eos: None,
            unk: None,
            pad: None,
            add_bos: false,
            add_space_prefix: true,
            pre: None,
        }
    }

    #[test]
    fn construction_validates() {
        assert!(matches!(
            Tokenizer::new(cfg("llama", &[])),
            Err(TokenizerError::BadVocabulary(_))
        ));
        assert!(matches!(
            Tokenizer::new(cfg("wordpiece", &["a"])),
            Err(TokenizerError::UnsupportedModel(_))
        ));
        let mut c = cfg("llama", &["a", "b"]);
        c.bos = Some(2);
        assert!(matches!(
            Tokenizer::new(c),
            Err(TokenizerError::BadTokenId(2))
        ));
        let mut c = cfg("gpt2", &["a"]);
        c.merges = vec!["nospace".into()];
        assert!(matches!(
            Tokenizer::new(c),
            Err(TokenizerError::BadVocabulary(_))
        ));
    }

    #[test]
    fn accessors_and_kinds() {
        let mut c = cfg("llama", &["<unk>", "<s>", "a"]);
        c.types = vec![2, 3, 1];
        c.unk = Some(0);
        c.bos = Some(1);
        let t = Tokenizer::new(c).unwrap();
        assert_eq!(t.vocab_size(), 3);
        assert_eq!(t.token(2), Some("a"));
        assert_eq!(t.token(9), None);
        assert_eq!(t.id_of("<s>"), Some(1));
        assert_eq!(t.kind(0), Some(TokenKind::Unknown));
        assert_eq!(t.kind(1), Some(TokenKind::Control));
        assert_eq!(t.kind(2), Some(TokenKind::Normal));
        assert_eq!(
            (t.bos(), t.unk(), t.eos(), t.pad()),
            (Some(1), Some(0), None, None)
        );
        assert!(!t.adds_bos());
        assert_eq!(t.pre_tokenizer_name(), None);
    }

    #[test]
    fn error_display() {
        assert!(TokenizerError::BadTokenId(7).to_string().contains('7'));
        assert!(TokenizerError::UnsupportedModel("x".into())
            .to_string()
            .contains("\"x\""));
    }
}
