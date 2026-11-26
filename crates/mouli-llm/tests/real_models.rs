//! The tokenizer and the chat formats against the real model files.
//!
//! The model files are large and are not in the repository. Set `MOULI_TEST_MODEL_DIR` to a folder
//! holding any of the files named below; models that are not there are skipped (and said so), so
//! the test passes on a machine without them.
//!
//! ```text
//! MOULI_TEST_MODEL_DIR=/path/to/models cargo test -p mouli-llm --test real_models -- --nocapture
//! ```

use mouli_llm::chat::{Message, Role, Template};
use mouli_llm::gguf::{parse_header_only, Gguf};
use mouli_llm::tokenizer::Tokenizer;
use std::io::Read;
use std::path::PathBuf;

/// (file name, the format its chat template must be detected as, a marker that starts its prompt,
/// the marker that ends an answer).
const MODELS: [(&str, Template, &str, &str); 3] = [
    (
        "qwen2.5-1.5b-instruct-q4_k_m.gguf",
        Template::ChatMl,
        "<|im_start|>",
        "<|im_end|>",
    ),
    (
        "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
        Template::Llama3,
        "<|begin_of_text|>",
        "<|eot_id|>",
    ),
    (
        "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        Template::Llama3,
        "<|begin_of_text|>",
        "<|eot_id|>",
    ),
];

/// The metadata of a model, read from the start of the file (the tokenizer is a few megabytes).
fn load(file: &str) -> Option<(Gguf, Tokenizer)> {
    let dir = PathBuf::from(std::env::var_os("MOULI_TEST_MODEL_DIR")?);
    let path = dir.join(file);
    let mut head = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(48 << 20)
        .read_to_end(&mut head)
        .ok()?;
    let g = parse_header_only(&head).expect("the header of a real model parses");
    let t = Tokenizer::from_gguf(&g).expect("a real model's tokenizer builds");
    Some((g, t))
}

fn for_each_model(mut check: impl FnMut(&str, &Gguf, &Tokenizer, Template, &str, &str)) -> usize {
    let mut ran = 0;
    for (file, template, open, close) in MODELS {
        match load(file) {
            Some((g, t)) => {
                check(file, &g, &t, template, open, close);
                ran += 1;
            }
            None => eprintln!("skipped {file}: set MOULI_TEST_MODEL_DIR to a folder that holds it"),
        }
    }
    ran
}

#[test]
fn text_round_trips_through_each_real_tokenizer() {
    let texts = [
        "",
        "Hello, world!",
        "  leading and trailing spaces  ",
        "line one\nline two\n\n\tindented\r\nwindows line",
        "fn main() { println!(\"{}\", 1 + 2); } // code",
        "Numbers 3.14159 and 1,000,000 and 0xDEADBEEF",
        "नमस्ते दुनिया",
        "你好，世界",
        "Emoji 🙂🚀 and accents: café naïve Zürich",
        "<|not a special token|> stays plain text",
        &"repeat ".repeat(200),
    ];
    let ran = for_each_model(|file, _, t, _, _, _| {
        for text in texts {
            let ids = t.encode(text, false, false);
            assert_eq!(t.decode(&ids), text, "{file}: round trip of {text:?}");
            if !text.is_empty() {
                assert!(!ids.is_empty(), "{file}: {text:?} gave no tokens");
            }
        }
    });
    eprintln!("tokenizer round trip checked on {ran} real model(s)");
}

#[test]
fn each_models_chat_format_is_detected_and_its_markers_are_single_tokens() {
    let ran = for_each_model(|file, g, t, template, open, close| {
        let chat_template = g.get_str("tokenizer.chat_template").ok();
        let arch = g.architecture().expect("architecture");
        assert_eq!(Template::detect(chat_template, arch), template, "{file}");
        // The markers the format writes are one token each when read as special tokens, and plain
        // text (several tokens) when not.
        for marker in [open, close] {
            assert_eq!(
                t.encode(marker, false, true).len(),
                1,
                "{file}: {marker} as a special token"
            );
            assert!(
                t.encode(marker, false, false).len() > 1,
                "{file}: {marker} as plain text"
            );
        }
        // A rendered conversation encodes with exactly as many special tokens as markers written.
        let conversation = [
            Message {
                role: Role::System,
                content: "Be brief.",
            },
            Message {
                role: Role::User,
                content: "What is 2+2?",
            },
            Message {
                role: Role::Assistant,
                content: "4.",
            },
            Message {
                role: Role::User,
                content: "And 3+3?",
            },
        ];
        let text = template.render(&conversation, true);
        let ids = t.encode(&text, false, true);
        let marker_id = t.encode(close, false, true)[0];
        let wrote = text.matches(close).count();
        assert_eq!(
            ids.iter().filter(|i| **i == marker_id).count(),
            wrote,
            "{file}: {close} tokens"
        );
        assert_eq!(
            ids[0],
            t.encode(open, false, true)[0],
            "{file}: starts with {open}"
        );
        // The text between the markers survives: decoding drops the markers but keeps the words.
        let words = t.decode(&ids);
        for w in ["Be brief.", "What is 2+2?", "And 3+3?"] {
            assert!(words.contains(w), "{file}: {w:?} missing from {words:?}");
        }
        // And the stop strings are the closing marker, so a reply is cut where the model ends it.
        assert!(template.stop_strings().contains(&close), "{file}");
    });
    eprintln!("chat formats checked on {ran} real model(s)");
}
