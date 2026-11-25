//! GPT-2 style byte-level BPE.

use crate::tokenizer::api::{TokenKind, Tokenizer};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::OnceLock;

/// The standard byte to printable-character table: printable Latin-1 bytes map to themselves,
/// the remaining 68 bytes to U+0100 upward in order.
fn byte_table() -> &'static ([char; 256], HashMap<char, u8>) {
    static T: OnceLock<([char; 256], HashMap<char, u8>)> = OnceLock::new();
    T.get_or_init(|| {
        let mut table = ['\0'; 256];
        let mut next = 256u32;
        for b in 0..=255u32 {
            let keep =
                (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
            let c = if keep {
                char::from_u32(b)
            } else {
                let c = char::from_u32(next);
                next += 1;
                c
            };
            table[b as usize] = c.unwrap_or('\u{fffd}');
        }
        let rev = table
            .iter()
            .enumerate()
            .map(|(b, c)| (*c, b as u8))
            .collect();
        (table, rev)
    })
}

/// Split text into words with the GPT-2 pattern
/// `'s|'t|'re|'ve|'m|'ll|'d| ?letters+| ?digits+| ?other+|spaces-not-before-a-word|spaces`.
pub(crate) fn pre_tokenize(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let at = |i: usize| chars[i].0;
    let end_of = |i: usize| if i < n { chars[i].0 } else { text.len() };
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i].1;
        if c == '\'' && i + 1 < n {
            let rest: String = chars[i + 1..n.min(i + 3)].iter().map(|(_, c)| *c).collect();
            let take = if rest.starts_with("re") || rest.starts_with("ve") || rest.starts_with("ll")
            {
                3
            } else if rest.starts_with(['s', 't', 'm', 'd']) {
                2
            } else {
                0
            };
            if take > 0 {
                out.push(&text[at(i)..end_of(i + take)]);
                i += take;
                continue;
            }
        }
        let start = i;
        let mut j = i;
        if c == ' ' && i + 1 < n && !chars[i + 1].1.is_whitespace() {
            j = i + 1;
        }
        let c2 = chars[j].1;
        let mut k = j;
        if c2.is_alphabetic() {
            while k < n && chars[k].1.is_alphabetic() {
                k += 1;
            }
        } else if c2.is_numeric() {
            while k < n && chars[k].1.is_numeric() {
                k += 1;
            }
        } else if !c2.is_whitespace() {
            while k < n
                && !chars[k].1.is_whitespace()
                && !chars[k].1.is_alphabetic()
                && !chars[k].1.is_numeric()
            {
                k += 1;
            }
        } else {
            while k < n && chars[k].1.is_whitespace() {
                k += 1;
            }
            // `\s+(?!\S)`: leave the last space for the next word when a word follows.
            if k < n && k - start > 1 {
                k -= 1;
            }
        }
        out.push(&text[at(start)..end_of(k)]);
        i = k;
    }
    out
}

struct Sym {
    start: usize,
    end: usize,
    prev: Option<usize>,
    next: Option<usize>,
    alive: bool,
}

#[derive(PartialEq, Eq)]
struct Cand {
    rank: usize,
    left: usize,
    right: usize,
    len: usize,
}

impl Ord for Cand {
    /// Min rank first (BinaryHeap is a max-heap, so wrap with `Reverse` at the call site).
    fn cmp(&self, o: &Self) -> Ordering {
        self.rank.cmp(&o.rank).then_with(|| self.left.cmp(&o.left))
    }
}
impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Merge the symbols of one mapped word and append token ids.
fn merge_word(t: &Tokenizer, ranks: &HashMap<String, usize>, word: &str, out: &mut Vec<u32>) {
    let mut syms: Vec<Sym> = Vec::new();
    for (i, c) in word.char_indices() {
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

    let mut heap: BinaryHeap<Reverse<Cand>> = BinaryHeap::new();
    let try_add = |syms: &Vec<Sym>,
                   heap: &mut BinaryHeap<Reverse<Cand>>,
                   l: Option<usize>,
                   r: Option<usize>| {
        let (Some(l), Some(r)) = (l, r) else { return };
        let key = format!(
            "{} {}",
            &word[syms[l].start..syms[l].end],
            &word[syms[r].start..syms[r].end]
        );
        if let Some(rank) = ranks.get(&key) {
            heap.push(Reverse(Cand {
                rank: *rank,
                left: l,
                right: r,
                len: syms[r].end - syms[l].start,
            }));
        }
    };
    for i in 0..syms.len().saturating_sub(1) {
        try_add(&syms, &mut heap, Some(i), Some(i + 1));
    }
    while let Some(Reverse(c)) = heap.pop() {
        let valid = syms[c.left].alive
            && syms[c.right].alive
            && syms[c.left].next == Some(c.right)
            && syms[c.right].end - syms[c.left].start == c.len;
        if !valid {
            continue;
        }
        syms[c.left].end = syms[c.right].end;
        syms[c.right].alive = false;
        let next = syms[c.right].next;
        syms[c.left].next = next;
        if let Some(n) = next {
            syms[n].prev = Some(c.left);
        }
        let prev = syms[c.left].prev;
        try_add(&syms, &mut heap, prev, Some(c.left));
        try_add(&syms, &mut heap, Some(c.left), next);
    }

    let mut cur = Some(0usize);
    while let Some(i) = cur {
        let piece = &word[syms[i].start..syms[i].end];
        match t.lookup(piece) {
            Some(id) => out.push(id),
            None => {
                // Fall back to single mapped characters, then to the unknown token.
                for (ci, ch) in piece.char_indices() {
                    let s = &piece[ci..ci + ch.len_utf8()];
                    if let Some(id) = t.lookup(s).or_else(|| t.unknown_id()) {
                        out.push(id);
                    }
                }
            }
        }
        cur = syms[i].next;
    }
}

/// Encode one text segment (no special tokens inside).
pub(crate) fn encode(
    t: &Tokenizer,
    ranks: &HashMap<String, usize>,
    text: &str,
    out: &mut Vec<u32>,
) {
    let (table, _) = byte_table();
    for word in pre_tokenize(text) {
        let mapped: String = word.bytes().map(|b| table[usize::from(b)]).collect();
        merge_word(t, ranks, &mapped, out);
    }
}

/// Decode ids: mapped characters go back to bytes; special tokens contribute their text.
pub(crate) fn decode(t: &Tokenizer, ids: &[u32]) -> Vec<u8> {
    let (_, rev) = byte_table();
    let mut out = Vec::new();
    for id in ids {
        let Some(text) = t.raw(*id) else { continue };
        match t.kind_of(*id) {
            TokenKind::Normal | TokenKind::Byte => {
                for c in text.chars() {
                    match rev.get(&c) {
                        Some(b) => out.push(*b),
                        None => {
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                }
            }
            TokenKind::UserDefined => out.extend_from_slice(text.as_bytes()),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::api::TokenizerConfig;

    #[test]
    fn byte_table_is_the_standard_gpt2_one() {
        let (table, rev) = byte_table();
        assert_eq!(table[b'A' as usize], 'A');
        assert_eq!(table[b'!' as usize], '!');
        assert_eq!(table[b' ' as usize], '\u{120}'); // Ġ
        assert_eq!(table[b'\n' as usize], '\u{10a}'); // Ċ
        assert_eq!(table[0], '\u{100}');
        assert_eq!(table[0xad], '\u{143}'); // soft hyphen is remapped
        assert_eq!(table[0xe9], 'é');
        assert_eq!(rev.len(), 256);
        for b in 0..=255u8 {
            assert_eq!(rev[&table[usize::from(b)]], b);
        }
    }

    #[test]
    fn pre_tokenizer_follows_the_gpt2_pattern() {
        assert_eq!(pre_tokenize("Hello world"), ["Hello", " world"]);
        assert_eq!(pre_tokenize("Hello, world!"), ["Hello", ",", " world", "!"]);
        assert_eq!(
            pre_tokenize("I'm sure we'll go"),
            ["I", "'m", " sure", " we", "'ll", " go"]
        );
        assert_eq!(pre_tokenize("don't"), ["don", "'t"]);
        assert_eq!(pre_tokenize("it's they've"), ["it", "'s", " they", "'ve"]);
        assert_eq!(pre_tokenize("abc123 def"), ["abc", "123", " def"]);
        assert_eq!(pre_tokenize("x  y"), ["x", " ", " y"]);
        assert_eq!(pre_tokenize("x   "), ["x", "   "]);
        assert_eq!(pre_tokenize("a\n\nb"), ["a", "\n", "\n", "b"]);
        assert_eq!(pre_tokenize("héllo wörld"), ["héllo", " wörld"]);
        assert_eq!(pre_tokenize(" 42"), [" 42"]);
        assert_eq!(pre_tokenize("$$$ ok"), ["$$$", " ok"]);
        assert_eq!(pre_tokenize("'"), ["'"]);
        assert_eq!(pre_tokenize("'x"), ["'", "x"]);
        assert!(pre_tokenize("").is_empty());
    }

    #[test]
    fn pre_tokenizer_never_loses_or_reorders_text() {
        for s in [
            "Hello,  world!!  \n\t x'y 'z\u{3000}日本語 12.5% — done",
            "   ",
            "a  b  c  ",
            "\n\n\n",
            "''''",
            "x'",
            "😀😀 😀",
        ] {
            assert_eq!(pre_tokenize(s).concat(), s, "{s:?}");
        }
    }

    /// Byte-level vocabulary: all 256 mapped bytes, then merged pieces.
    fn tokenizer(merges: &[&str], extra: &[&str]) -> Tokenizer {
        let (table, _) = byte_table();
        let mut tokens: Vec<String> = table.iter().map(|c| c.to_string()).collect();
        tokens.extend(extra.iter().map(|s| (*s).to_string()));
        Tokenizer::new(TokenizerConfig {
            model: "gpt2".into(),
            tokens,
            scores: vec![],
            types: vec![],
            merges: merges.iter().map(|s| (*s).to_string()).collect(),
            bos: None,
            eos: None,
            unk: None,
            pad: None,
            add_bos: false,
            add_space_prefix: false,
            pre: Some("gpt2".into()),
        })
        .unwrap()
    }

    fn pieces(t: &Tokenizer, ids: &[u32]) -> Vec<String> {
        ids.iter()
            .map(|i| t.token(*i).unwrap().to_string())
            .collect()
    }

    #[test]
    fn lowest_rank_merges_first() {
        // "abcd": rank 0 = "b c", rank 1 = "a bc", rank 2 = "abc d"
        let t = tokenizer(&["b c", "a bc", "abc d"], &["bc", "abc", "abcd"]);
        let ids = t.encode("abcd", false, false);
        assert_eq!(pieces(&t, &ids), ["abcd"]);
        // without the last merge, stops at "abc" + "d"
        let t = tokenizer(&["b c", "a bc"], &["bc", "abc"]);
        assert_eq!(pieces(&t, &t.encode("abcd", false, false)), ["abc", "d"]);
    }

    #[test]
    fn merge_order_decides_ambiguous_pairs() {
        // "aaa": merges "a a" -> "aa"; then only one "aa" + "a" remain
        let t = tokenizer(&["a a"], &["aa"]);
        assert_eq!(pieces(&t, &t.encode("aaa", false, false)), ["aa", "a"]);
        assert_eq!(pieces(&t, &t.encode("aaaa", false, false)), ["aa", "aa"]);
    }

    #[test]
    fn spaces_use_the_mapped_character() {
        let t = tokenizer(&["\u{120} t", "\u{120}t h"], &["\u{120}t", "\u{120}th"]);
        let ids = t.encode("a th", false, false);
        assert_eq!(pieces(&t, &ids), ["a", "\u{120}th"]);
        assert_eq!(t.decode(&ids), "a th");
    }

    #[test]
    fn round_trips_any_utf8_with_a_full_byte_vocabulary() {
        let t = tokenizer(&["t h", "th e"], &["th", "the"]);
        for text in [
            "the cat",
            "The  CAT\n",
            "héllo wörld",
            "日本語のテスト",
            "emoji 😀 ok",
            "a\u{0}b",
            "tabs\tand\r\nnewlines",
            "",
            "  ",
            "x'y 're",
        ] {
            let ids = t.encode(text, false, false);
            assert_eq!(t.decode(&ids), text, "text {text:?}");
        }
    }

    #[test]
    fn missing_vocabulary_entries_fall_back_to_characters() {
        // vocabulary lacks "ab" although a merge produces it
        let t = tokenizer(&["a b"], &[]);
        assert_eq!(pieces(&t, &t.encode("ab", false, false)), ["a", "b"]);
    }

    #[test]
    fn special_tokens_in_text() {
        let (table, _) = byte_table();
        let mut tokens: Vec<String> = table.iter().map(|c| c.to_string()).collect();
        tokens.push("<|end|>".into());
        let mut types = vec![1; 256];
        types.push(4);
        let t = Tokenizer::new(TokenizerConfig {
            model: "gpt2".into(),
            tokens,
            scores: vec![],
            types,
            merges: vec![],
            bos: None,
            eos: Some(256),
            unk: None,
            pad: None,
            add_bos: false,
            add_space_prefix: false,
            pre: None,
        })
        .unwrap();
        let ids = t.encode("hi<|end|>yo", false, true);
        assert_eq!(ids.iter().filter(|i| **i == 256).count(), 1);
        assert_eq!(t.decode(&ids), "hi<|end|>yo");
        let plain = t.encode("hi<|end|>yo", false, false);
        assert!(!plain.contains(&256));
        assert_eq!(t.decode(&plain), "hi<|end|>yo");
    }

    #[test]
    fn long_single_word_is_fast() {
        let t = tokenizer(&["a a", "aa aa"], &["aa", "aaaa"]);
        let text = "a".repeat(100_000);
        let ids = t.encode(&text, false, false);
        assert_eq!(t.decode(&ids), text);
        assert_eq!(ids.len(), 25_000);
    }

    #[test]
    fn decode_ignores_unknown_ids() {
        let t = tokenizer(&[], &[]);
        // ids 0..256 are the mapped bytes in byte order: 97 is "a"
        assert_eq!(t.decode(&[97, 99_999, 98]), "ab");
        assert_eq!(t.decode(&[]), "");
    }
}
