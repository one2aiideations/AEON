//! Build tokenizers from real (writer-produced) GGUF metadata.

use mouli_llm::gguf::{parse, Array, GgufWriter, Value};
use mouli_llm::tokenizer::{Tokenizer, TokenizerError};

fn spm_file() -> Vec<u8> {
    let mut tokens: Vec<String> = vec!["<unk>".into(), "<s>".into(), "</s>".into()];
    let mut types: Vec<i32> = vec![2, 3, 3];
    let mut scores: Vec<f32> = vec![0.0; 3];
    for b in 0..=255u8 {
        tokens.push(format!("<0x{b:02X}>"));
        types.push(6);
        scores.push(0.0);
    }
    for (p, s) in [
        ("\u{2581}", 0.0f32),
        ("h", 0.0),
        ("i", 0.0),
        ("\u{2581}h", 1.0),
        ("\u{2581}hi", 2.0),
    ] {
        tokens.push(p.into());
        types.push(1);
        scores.push(s);
    }
    let mut w = GgufWriter::new();
    w.add_metadata("general.architecture", Value::String("llama".into()));
    w.add_metadata("tokenizer.ggml.model", Value::String("llama".into()));
    w.add_metadata("tokenizer.ggml.tokens", Value::Array(Array::String(tokens)));
    w.add_metadata("tokenizer.ggml.scores", Value::Array(Array::F32(scores)));
    w.add_metadata("tokenizer.ggml.token_type", Value::Array(Array::I32(types)));
    w.add_metadata("tokenizer.ggml.bos_token_id", Value::U32(1));
    w.add_metadata("tokenizer.ggml.eos_token_id", Value::U32(2));
    w.add_metadata("tokenizer.ggml.unknown_token_id", Value::U32(0));
    w.add_metadata("tokenizer.ggml.add_bos_token", Value::Bool(true));
    w.finish().unwrap()
}

#[test]
fn llama_style_from_gguf() {
    let bytes = spm_file();
    let g = parse(&bytes).unwrap();
    let t = Tokenizer::from_gguf(&g).unwrap();
    assert_eq!((t.bos(), t.eos(), t.unk()), (Some(1), Some(2), Some(0)));
    assert!(t.adds_bos());
    let ids = t.encode_default("hi hi");
    assert_eq!(ids[0], 1);
    assert_eq!(t.decode(&ids), "hi hi");
    assert_eq!(&ids[1..], &[t.id_of("\u{2581}hi").unwrap(); 2]);
}

#[test]
fn bpe_from_gguf() {
    // 256 mapped-byte tokens are produced by the crate's own table: take them from a tokenizer
    // built through `new`, then round-trip the same vocabulary through GGUF metadata.
    let mut tokens: Vec<String> = Vec::new();
    let mut next = 256u32;
    for b in 0..=255u32 {
        let keep = (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
        let c = if keep {
            char::from_u32(b).unwrap()
        } else {
            let c = char::from_u32(next).unwrap();
            next += 1;
            c
        };
        tokens.push(c.to_string());
    }
    tokens.push("th".into());
    tokens.push("the".into());
    let mut w = GgufWriter::new();
    w.add_metadata("tokenizer.ggml.model", Value::String("gpt2".into()));
    w.add_metadata("tokenizer.ggml.pre", Value::String("gpt2".into()));
    w.add_metadata("tokenizer.ggml.tokens", Value::Array(Array::String(tokens)));
    w.add_metadata(
        "tokenizer.ggml.merges",
        Value::Array(Array::String(vec!["t h".into(), "th e".into()])),
    );
    let g = parse(&w.finish().unwrap()).unwrap();
    let t = Tokenizer::from_gguf(&g).unwrap();
    assert_eq!(t.pre_tokenizer_name(), Some("gpt2"));
    let ids = t.encode("the the", false, false);
    assert_eq!(t.decode(&ids), "the the");
    assert_eq!(ids[0], t.id_of("the").unwrap());
}

#[test]
fn missing_or_wrong_metadata_is_a_clean_error() {
    let g = parse(&GgufWriter::new().finish().unwrap()).unwrap();
    assert!(matches!(
        Tokenizer::from_gguf(&g),
        Err(TokenizerError::Metadata(_))
    ));

    let mut w = GgufWriter::new();
    w.add_metadata("tokenizer.ggml.model", Value::String("unigram-xl".into()));
    w.add_metadata(
        "tokenizer.ggml.tokens",
        Value::Array(Array::String(vec!["a".into()])),
    );
    let g = parse(&w.finish().unwrap()).unwrap();
    assert!(matches!(
        Tokenizer::from_gguf(&g),
        Err(TokenizerError::UnsupportedModel(_))
    ));

    let mut w = GgufWriter::new();
    w.add_metadata("tokenizer.ggml.model", Value::String("llama".into()));
    w.add_metadata("tokenizer.ggml.tokens", Value::Array(Array::U32(vec![1])));
    let g = parse(&w.finish().unwrap()).unwrap();
    assert!(matches!(
        Tokenizer::from_gguf(&g),
        Err(TokenizerError::Metadata(_))
    ));

    let mut w = GgufWriter::new();
    w.add_metadata("tokenizer.ggml.model", Value::String("llama".into()));
    w.add_metadata(
        "tokenizer.ggml.tokens",
        Value::Array(Array::String(vec!["a".into()])),
    );
    w.add_metadata("tokenizer.ggml.bos_token_id", Value::U32(5));
    let g = parse(&w.finish().unwrap()).unwrap();
    assert!(matches!(
        Tokenizer::from_gguf(&g),
        Err(TokenizerError::BadTokenId(5))
    ));
}

#[test]
fn defaults_when_optional_keys_are_absent() {
    let mut w = GgufWriter::new();
    w.add_metadata("tokenizer.ggml.model", Value::String("llama".into()));
    w.add_metadata(
        "tokenizer.ggml.tokens",
        Value::Array(Array::String(vec!["a".into(), "b".into()])),
    );
    let t = Tokenizer::from_gguf(&parse(&w.finish().unwrap()).unwrap()).unwrap();
    assert!(t.adds_bos()); // default when the key is absent
    assert_eq!(t.bos(), None);
    assert_eq!(t.encode_default("ab"), t.encode("ab", true, false)); // no BOS id, so nothing added
}
