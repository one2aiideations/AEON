//! SentencePiece-style encoding: greedy score-ordered pair merging with byte fallback.

use crate::tokenizer::{TokenKind, Tokenizer};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

const SPACE: char = '\u{2581}'; // ▁

struct Sym {
    start: usize,
    end: usize,
    prev: Option<usize>,
    next: Option<usize>,
    alive: bool,
}

struct Bigram {
    score: f32,
    left: usize,
    right: usize,
    len: usize,
}

impl PartialEq for Bigram {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Bigram {}
impl PartialOrd for Bigram {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Bigram {
    /// Max-heap: higher score first, then the leftmost pair.
    fn cmp(&self, o: &Self) -> Ordering {
        self.score
            .total_cmp(&o.score)
            .then_with(|| o.left.cmp(&self.left))
    }
}

fn mergeable(t: &Tokenizer, s: &str) -> Option<u32> {
    let id = t.lookup(s)?;
    (t.kind_of(id) == TokenKind::Normal).then_some(id)
}

/// Encode one text segment, appending ids to `out`.
pub(crate) fn encode(t: &Tokenizer, text: &str, prefix: bool, out: &mut Vec<u32>) {
    let mut norm = String::with_capacity(text.len() + 4);
    if prefix {
        norm.push(SPACE);
    }
    for c in text.chars() {
        norm.push(if c == ' ' { SPACE } else { c });
    }

    let mut syms: Vec<Sym> = Vec::new();
    for (i, c) in norm.char_indices() {
        let n = syms.len();
        syms.push(Sym {
            start: i,
            end: i + c.len_utf8(),
            prev: n.checked_sub(1),
            next: Some(n + 1),
            alive: true,
        });
    }
    if syms.is_empty() {
        return;
    }
    let last = syms.len() - 1;
    syms[last].next = None;

    let mut heap = BinaryHeap::new();
    let try_add = |syms: &Vec<Sym>,
                   heap: &mut BinaryHeap<Bigram>,
                   left: Option<usize>,
                   right: Option<usize>| {
        let (Some(l), Some(r)) = (left, right) else {
            return;
        };
        let (start, end) = (syms[l].start, syms[r].end);
        if let Some(id) = mergeable(t, &norm[start..end]) {
            heap.push(Bigram {
                score: t.score_of(id),
                left: l,
                right: r,
                len: end - start,
            });
        }
    };
    for i in 0..syms.len().saturating_sub(1) {
        try_add(&syms, &mut heap, Some(i), Some(i + 1));
    }

    while let Some(b) = heap.pop() {
        let valid = syms[b.left].alive
            && syms[b.right].alive
            && syms[b.left].next == Some(b.right)
            && syms[b.right].end - syms[b.left].start == b.len;
        if !valid {
            continue;
        }
        syms[b.left].end = syms[b.right].end;
        syms[b.right].alive = false;
        let next = syms[b.right].next;
        syms[b.left].next = next;
        if let Some(n) = next {
            syms[n].prev = Some(b.left);
        }
        let prev = syms[b.left].prev;
        try_add(&syms, &mut heap, prev, Some(b.left));
        try_add(&syms, &mut heap, Some(b.left), next);
    }

    let mut cur = Some(0usize);
    while let Some(i) = cur {
        let piece = &norm[syms[i].start..syms[i].end];
        match mergeable(t, piece).or_else(|| {
            t.lookup(piece)
                .filter(|id| t.kind_of(*id) != TokenKind::Byte)
        }) {
            Some(id) => out.push(id),
            None => {
                for b in piece.bytes() {
                    if let Some(id) = t.byte_id(b).or_else(|| t.unknown_id()) {
                        out.push(id);
                    }
                }
            }
        }
        cur = syms[i].next;
    }
}

/// Decode ids: byte tokens give their byte, normal tokens their text with `▁` as a space,
/// control/unknown/unused tokens nothing. A single leading space (the dummy prefix) is removed.
pub(crate) fn decode(t: &Tokenizer, ids: &[u32]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for id in ids {
        let Some(text) = t.raw(*id) else { continue };
        match t.kind_of(*id) {
            TokenKind::Byte => {
                if let Some(b) = crate::tokenizer::parse_byte_token(text) {
                    out.push(b);
                }
            }
            TokenKind::Normal | TokenKind::UserDefined => {
                for c in text.chars() {
                    let c = if c == SPACE { ' ' } else { c };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
            _ => {}
        }
    }
    if t.space_prefix() && out.first() == Some(&b' ') {
        out.remove(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::TokenizerConfig;

    /// Vocabulary: specials, all 256 byte tokens, then pieces with scores.
    fn tokenizer(pieces: &[(&str, f32)], add_prefix: bool) -> Tokenizer {
        let mut tokens = vec!["<unk>".to_string(), "<s>".to_string(), "</s>".to_string()];
        let mut types = vec![2, 3, 3];
        let mut scores = vec![0.0, 0.0, 0.0];
        for b in 0..=255u8 {
            tokens.push(format!("<0x{b:02X}>"));
            types.push(6);
            scores.push(0.0);
        }
        for (p, s) in pieces {
            tokens.push((*p).to_string());
            types.push(1);
            scores.push(*s);
        }
        Tokenizer::new(TokenizerConfig {
            model: "llama".into(),
            tokens,
            scores,
            types,
            merges: vec![],
            bos: Some(1),
            eos: Some(2),
            unk: Some(0),
            pad: None,
            add_bos: true,
            add_space_prefix: add_prefix,
            pre: None,
        })
        .unwrap()
    }

    fn pieces(t: &Tokenizer, ids: &[u32]) -> Vec<String> {
        ids.iter()
            .map(|i| t.token(*i).unwrap().to_string())
            .collect()
    }

    #[test]
    fn merges_by_score_not_by_position() {
        // "abc": ab (score 2) beats bc (score 1) even though both are available
        let t = tokenizer(
            &[
                ("a", 0.0),
                ("b", 0.0),
                ("c", 0.0),
                ("ab", 2.0),
                ("bc", 1.0),
                ("xyz", -1.0),
            ],
            false,
        );
        assert_eq!(pieces(&t, &t.encode("abc", false, false)), ["ab", "c"]);
        // equal scores: leftmost pair wins
        let t = tokenizer(
            &[("a", 0.0), ("b", 0.0), ("c", 0.0), ("ab", 1.0), ("bc", 1.0)],
            false,
        );
        assert_eq!(pieces(&t, &t.encode("abc", false, false)), ["ab", "c"]);
    }

    #[test]
    fn merges_continue_until_no_pair_is_in_the_vocabulary() {
        let t = tokenizer(
            &[
                ("a", 0.0),
                ("b", 0.0),
                ("c", 0.0),
                ("ab", 2.0),
                ("abc", 3.0),
            ],
            false,
        );
        assert_eq!(pieces(&t, &t.encode("abc", false, false)), ["abc"]);
        assert_eq!(pieces(&t, &t.encode("abcab", false, false)), ["abc", "ab"]);
    }

    #[test]
    fn dummy_prefix_and_space_marker() {
        let t = tokenizer(
            &[
                ("\u{2581}", 0.0),
                ("h", 0.0),
                ("i", 0.0),
                ("\u{2581}h", 1.0),
                ("\u{2581}hi", 2.0),
            ],
            true,
        );
        assert_eq!(pieces(&t, &t.encode("hi", false, false)), ["\u{2581}hi"]);
        assert_eq!(
            pieces(&t, &t.encode("hi hi", false, false)),
            ["\u{2581}hi", "\u{2581}hi"]
        );
        let t = tokenizer(
            &[
                ("\u{2581}", 0.0),
                ("h", 0.0),
                ("i", 0.0),
                ("\u{2581}h", 1.0),
                ("hi", 2.0),
            ],
            false,
        );
        assert_eq!(pieces(&t, &t.encode("hi", false, false)), ["hi"]);
    }

    #[test]
    fn bos_is_prepended_on_request() {
        let t = tokenizer(&[("a", 0.0)], false);
        assert_eq!(t.encode("a", true, false)[0], 1);
        assert_ne!(t.encode("a", false, false)[0], 1);
        assert_eq!(t.encode_default("a")[0], 1);
        assert_eq!(t.encode("", true, false), vec![1]);
        assert!(t.encode("", false, false).is_empty());
    }

    #[test]
    fn unknown_characters_fall_back_to_bytes() {
        let t = tokenizer(&[("a", 0.0)], false);
        let ids = t.encode("é", false, false); // 0xC3 0xA9
        assert_eq!(pieces(&t, &ids), ["<0xC3>", "<0xA9>"]);
        assert_eq!(t.decode(&ids), "é");
    }

    #[test]
    fn without_byte_tokens_unknown_becomes_unk() {
        let t = Tokenizer::new(TokenizerConfig {
            model: "llama".into(),
            tokens: vec!["<unk>".into(), "a".into()],
            scores: vec![],
            types: vec![2, 1],
            merges: vec![],
            bos: None,
            eos: None,
            unk: Some(0),
            pad: None,
            add_bos: false,
            add_space_prefix: false,
            pre: None,
        })
        .unwrap();
        // "é" is two bytes and there are no byte tokens: one unk per byte
        assert_eq!(t.encode("é", false, false), vec![0, 0]);
        assert_eq!(t.encode("aéa", false, false), vec![1, 0, 0, 1]);
    }

    #[test]
    fn decode_round_trips_arbitrary_text() {
        let t = tokenizer(
            &[
                ("\u{2581}", 0.0),
                ("t", 0.0),
                ("h", 0.0),
                ("e", 0.0),
                ("\u{2581}t", 1.0),
                ("th", 2.0),
                ("the", 3.0),
            ],
            true,
        );
        for text in [
            "the",
            "the the",
            " the",
            "  the  ",
            "héllo wörld",
            "日本語 テスト",
            "a\nb\tc",
            "emoji 😀 ok",
            "",
        ] {
            let ids = t.encode(text, false, false);
            assert_eq!(t.decode(&ids), text, "text {text:?}");
        }
    }

    #[test]
    fn decode_skips_control_tokens_and_handles_bad_ids() {
        let t = tokenizer(&[("a", 0.0)], false);
        let a = t.id_of("a").unwrap();
        assert_eq!(t.decode(&[1, a, 2]), "a");
        assert_eq!(t.decode(&[a, 99_999, a]), "aa");
        assert_eq!(t.decode(&[]), "");
    }

    #[test]
    fn special_tokens_are_parsed_only_on_request() {
        let mut tokens = vec![
            "<unk>".to_string(),
            "<s>".to_string(),
            "<|im|>".to_string(),
            "a".to_string(),
        ];
        let types = vec![2, 3, 4, 1];
        tokens.extend((0..=255u8).map(|b| format!("<0x{b:02X}>")));
        let mut types = types;
        types.extend(std::iter::repeat_n(6, 256));
        let t = Tokenizer::new(TokenizerConfig {
            model: "llama".into(),
            tokens,
            scores: vec![],
            types,
            merges: vec![],
            bos: Some(1),
            eos: None,
            unk: Some(0),
            pad: None,
            add_bos: false,
            add_space_prefix: false,
            pre: None,
        })
        .unwrap();
        assert_eq!(t.encode("a<|im|>a", false, true), vec![3, 2, 3]);
        // without parsing, the text is ordinary characters (via bytes here), never the special id
        assert!(!t.encode("a<|im|>a", false, false).contains(&2));
    }

    #[test]
    fn long_input_is_linearithmic_enough() {
        let t = tokenizer(&[("a", 0.0), ("aa", 1.0), ("aaaa", 2.0)], false);
        let text = "a".repeat(200_000);
        let ids = t.encode(&text, false, false);
        assert_eq!(t.decode(&ids), text);
    }
}
