//! Recursive text chunking for retrieval.
//!
//! Splits a document into pieces of at most `max_chars` characters, preferring to break at
//! paragraph boundaries, then lines, then sentences, then words, and only as a last resort in
//! the middle of a word. Consecutive chunks can overlap by up to `overlap` characters so a
//! fact that straddles a boundary still appears whole in one chunk.
//!
//! Every chunk records its byte range in the source text, so a retrieved chunk can be traced
//! back to the exact place it came from (for citations). Chunks are deterministic.

use std::fmt;
use std::ops::Range;

/// Chunking parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkConfig {
    /// Largest chunk, in characters (at least 1).
    pub max_chars: usize,
    /// How much of the previous chunk to repeat at the start of the next, in characters.
    /// Must be smaller than `max_chars`.
    pub overlap: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        ChunkConfig {
            max_chars: 800,
            overlap: 100,
        }
    }
}

/// Invalid chunking parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkError {
    /// `max_chars` is zero.
    ZeroSize,
    /// `overlap` is not smaller than `max_chars`.
    OverlapTooLarge,
}

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChunkError::ZeroSize => f.write_str("max_chars must be at least 1"),
            ChunkError::OverlapTooLarge => f.write_str("overlap must be smaller than max_chars"),
        }
    }
}

impl std::error::Error for ChunkError {}

/// One chunk of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// Position of this chunk in the document (0-based).
    pub index: usize,
    /// Byte range in the source text.
    pub range: Range<usize>,
    /// The text, `&source[range]`.
    pub text: &'a str,
}

/// Separators from coarsest to finest. Each stays attached to the end of the piece before it.
const SEPARATORS: [&str; 5] = ["\n\n", "\n", ". ", " ", ""];

fn chars_in(text: &str, r: &Range<usize>) -> usize {
    text[r.clone()].chars().count()
}

/// Split `r` into atoms of at most `max` characters that exactly tile `r`.
fn atoms(text: &str, r: Range<usize>, level: usize, max: usize, out: &mut Vec<Range<usize>>) {
    if r.is_empty() {
        return;
    }
    if chars_in(text, &r) <= max {
        out.push(r);
        return;
    }
    if level >= SEPARATORS.len() - 1 {
        // Last resort: hard split by characters.
        let mut start = r.start;
        let mut count = 0;
        for (i, _) in text[r.clone()].char_indices() {
            if count == max {
                out.push(start..r.start + i);
                start = r.start + i;
                count = 0;
            }
            count += 1;
        }
        if start < r.end {
            out.push(start..r.end);
        }
        return;
    }
    let sep = SEPARATORS[level];
    let slice = &text[r.clone()];
    let mut start = r.start;
    let mut search = 0;
    while let Some(pos) = slice[search..].find(sep) {
        let end = r.start + search + pos + sep.len();
        atoms(text, start..end, level + 1, max, out);
        start = end;
        search = end - r.start;
    }
    if start < r.end {
        atoms(text, start..r.end, level + 1, max, out);
    }
}

fn emit<'a>(text: &'a str, parts: &[Range<usize>], cur: &[usize], out: &mut Vec<Chunk<'a>>) {
    let range = parts[cur[0]].start..parts[cur[cur.len() - 1]].end;
    out.push(Chunk {
        index: out.len(),
        text: &text[range.clone()],
        range,
    });
}

/// Split `text` into chunks.
pub fn chunk(text: &str, cfg: ChunkConfig) -> Result<Vec<Chunk<'_>>, ChunkError> {
    if cfg.max_chars == 0 {
        return Err(ChunkError::ZeroSize);
    }
    if cfg.overlap >= cfg.max_chars {
        return Err(ChunkError::OverlapTooLarge);
    }
    let mut parts = Vec::new();
    atoms(text, 0..text.len(), 0, cfg.max_chars, &mut parts);
    let lens: Vec<usize> = parts.iter().map(|r| chars_in(text, r)).collect();

    let mut out = Vec::new();
    // Indices into `parts` of the chunk being built, and its length in characters.
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_len = 0usize;
    for i in 0..parts.len() {
        if !cur.is_empty() && cur_len + lens[i] > cfg.max_chars {
            emit(text, &parts, &cur, &mut out);
            // Carry over a tail of the finished chunk as overlap, as long as the next atom still fits.
            let mut keep = 0usize;
            let mut kept_len = 0usize;
            for j in (0..cur.len()).rev() {
                let l = lens[cur[j]];
                if kept_len + l > cfg.overlap || kept_len + l + lens[i] > cfg.max_chars {
                    break;
                }
                kept_len += l;
                keep += 1;
            }
            cur = cur[cur.len() - keep..].to_vec();
            cur_len = kept_len;
        }
        cur.push(i);
        cur_len += lens[i];
    }
    if !cur.is_empty() {
        emit(text, &parts, &cur, &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: usize, overlap: usize) -> ChunkConfig {
        ChunkConfig {
            max_chars: max,
            overlap,
        }
    }

    #[test]
    fn rejects_bad_parameters() {
        assert_eq!(chunk("x", cfg(0, 0)), Err(ChunkError::ZeroSize));
        assert_eq!(chunk("x", cfg(5, 5)), Err(ChunkError::OverlapTooLarge));
        assert_eq!(chunk("x", cfg(5, 9)), Err(ChunkError::OverlapTooLarge));
        assert!(chunk("x", ChunkConfig::default()).is_ok());
    }

    #[test]
    fn empty_and_short_text() {
        assert!(chunk("", cfg(10, 2)).unwrap().is_empty());
        let c = chunk("short", cfg(10, 2)).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].index, c[0].range.clone(), c[0].text),
            (0, 0..5, "short")
        );
    }

    #[test]
    fn prefers_paragraph_boundaries() {
        let text = "First paragraph here.\n\nSecond paragraph here.\n\nThird one.";
        let c = chunk(text, cfg(30, 0)).unwrap();
        let texts: Vec<&str> = c.iter().map(|c| c.text).collect();
        assert_eq!(
            texts,
            [
                "First paragraph here.\n\n",
                "Second paragraph here.\n\n",
                "Third one."
            ]
        );
    }

    #[test]
    fn falls_back_to_sentences_then_words_then_characters() {
        let text = "One two three. Four five six. Seven eight nine.";
        let c = chunk(text, cfg(16, 0)).unwrap();
        assert!(c.iter().all(|c| c.text.chars().count() <= 16));
        assert!(c[0].text.ends_with("three. "));
        let words = chunk("alpha beta gamma delta", cfg(11, 0)).unwrap();
        assert_eq!(
            words.iter().map(|c| c.text).collect::<Vec<_>>(),
            ["alpha beta ", "gamma delta"]
        );
        let long = chunk("abcdefghij", cfg(4, 0)).unwrap();
        assert_eq!(
            long.iter().map(|c| c.text).collect::<Vec<_>>(),
            ["abcd", "efgh", "ij"]
        );
    }

    #[test]
    fn chunks_without_overlap_tile_the_text() {
        let text = "Lorem ipsum dolor sit amet. Consectetur adipiscing elit.\n\nSed do eiusmod tempor.\nIncididunt ut labore et dolore magna aliqua.";
        let c = chunk(text, cfg(25, 0)).unwrap();
        assert_eq!(c.iter().map(|c| c.text).collect::<String>(), text);
        let mut end = 0;
        for ch in &c {
            assert_eq!(ch.range.start, end);
            assert_eq!(&text[ch.range.clone()], ch.text);
            end = ch.range.end;
        }
        assert_eq!(end, text.len());
    }

    #[test]
    fn overlap_repeats_the_tail_and_respects_the_limit() {
        let text = "aaa bbb ccc ddd eee fff ggg hhh iii jjj";
        let c = chunk(text, cfg(16, 8)).unwrap();
        assert!(c.len() > 2);
        for w in c.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            assert!(
                b.range.start < a.range.end,
                "no overlap between {:?} and {:?}",
                a.text,
                b.text
            );
            assert!(b.range.start > a.range.start, "no progress");
            let shared = text[b.range.start..a.range.end].chars().count();
            assert!(shared <= 8, "overlap {shared} exceeds the limit");
        }
        for ch in &c {
            assert!(ch.text.chars().count() <= 16);
        }
        // every character of the source is covered
        assert_eq!(c.first().unwrap().range.start, 0);
        assert_eq!(c.last().unwrap().range.end, text.len());
    }

    #[test]
    fn no_overlap_when_atoms_are_larger_than_the_overlap() {
        // sentences of ~14 chars with overlap 5: nothing small enough to repeat
        let text = "Hello there. General Kenobi. You are bold.";
        let c = chunk(text, cfg(20, 5)).unwrap();
        for w in c.windows(2) {
            assert!(w[1].range.start >= w[0].range.end);
        }
    }

    #[test]
    fn unicode_is_never_split_inside_a_character() {
        let text = "héllo wörld 日本語のテキスト 😀😀😀 ok. 日本語の文章です。";
        for max in 1..12 {
            let overlap = max / 3;
            let c = chunk(text, cfg(max, overlap)).unwrap();
            for ch in &c {
                assert!(
                    text.is_char_boundary(ch.range.start) && text.is_char_boundary(ch.range.end)
                );
                assert!(ch.text.chars().count() <= max, "max {max}: {:?}", ch.text);
            }
            assert_eq!(c.first().unwrap().range.start, 0);
            assert_eq!(c.last().unwrap().range.end, text.len());
        }
    }

    #[test]
    fn indices_are_sequential_and_results_deterministic() {
        let text = "word ".repeat(200);
        let a = chunk(&text, cfg(40, 10)).unwrap();
        let b = chunk(&text, cfg(40, 10)).unwrap();
        assert_eq!(a, b);
        for (i, ch) in a.iter().enumerate() {
            assert_eq!(ch.index, i);
        }
    }

    #[test]
    fn whitespace_only_and_separator_only_inputs() {
        assert_eq!(chunk("   ", cfg(10, 0)).unwrap().len(), 1);
        let c = chunk("\n\n\n\n\n\n", cfg(2, 0)).unwrap();
        assert_eq!(c.iter().map(|c| c.text).collect::<String>(), "\n\n\n\n\n\n");
        assert!(c.iter().all(|c| c.text.chars().count() <= 2));
    }

    #[test]
    fn large_input_is_handled() {
        let text = "Sentence number one. ".repeat(50_000);
        let c = chunk(&text, cfg(500, 50)).unwrap();
        assert!(c.len() > 1000);
        assert!(c.iter().all(|c| c.text.chars().count() <= 500));
    }
}
