//! Fixed-width text wrapping and terminal display utilities.
//!
//! This module provides Unicode-aware terminal width calculation, ANSI escape
//! sequence handling, greedy text wrapping, truncation, padding, and progress
//! bar generation. It uses only the Rust standard library and performs no I/O.
//!
//! An empty input passed to [`wrap`] returns an empty vector. Existing newline
//! characters delimit paragraphs, including blank paragraphs.

use std::iter::Peekable;
use std::str::Chars;

/// Represents one printable character or one complete terminal escape sequence.
#[derive(Clone, Debug)]
struct Token {
    raw: String,
    width: usize,
    whitespace: bool,
    sgr: Option<String>,
    reset: bool,
}

/// Returns whether a character belongs to a combining or zero-width range.
fn is_zero_width(c: char) -> bool {
    let n = u32::from(c);

    c.is_control()
        || (0x0300..=0x036F).contains(&n)
        || (0x200B..=0x200F).contains(&n)
        || (0xFE00..=0xFE0F).contains(&n)
}

/// Returns whether a character occupies two terminal columns.
fn is_wide(c: char) -> bool {
    let n = u32::from(c);

    (0x1100..=0x115F).contains(&n)
        || (0x2E80..=0xA4CF).contains(&n)
        || (0xAC00..=0xD7A3).contains(&n)
        || (0xF900..=0xFAFF).contains(&n)
        || (0xFE30..=0xFE6F).contains(&n)
        || (0xFF00..=0xFF60).contains(&n)
        || (0xFFE0..=0xFFE6).contains(&n)
        || (0x1F300..=0x1F64F).contains(&n)
        || (0x1F900..=0x1F9FF).contains(&n)
        || (0x20000..=0x3FFFD).contains(&n)
}

/// Returns the display width of a single character.
fn char_width(c: char) -> usize {
    if is_zero_width(c) {
        0
    } else if is_wide(c) {
        2
    } else {
        1
    }
}

/// Attempts to consume an ANSI CSI escape sequence.
///
/// An incomplete sequence is left unconsumed.
fn consume_csi(chars: &mut Peekable<Chars<'_>>) -> Option<String> {
    if chars.next()? != '[' {
        return None;
    }

    let mut sequence = String::from("\x1b[");

    while let Some(&c) = chars.peek() {
        if ('\x40'..='\x7e').contains(&c) {
            sequence.push(c);
            chars.next();
            return Some(sequence);
        }

        if !('\x20'..='\x3f').contains(&c) {
            return None;
        }

        sequence.push(c);
        chars.next();
    }

    None
}

/// Attempts to consume an ANSI OSC escape sequence.
///
/// Both BEL and ESC-backslash terminate OSC sequences.
fn consume_osc(chars: &mut Peekable<Chars<'_>>) -> Option<String> {
    if chars.next()? != ']' {
        return None;
    }

    let mut sequence = String::from("\x1b]");

    while let Some(c) = chars.next() {
        sequence.push(c);

        if c == '\x07' {
            return Some(sequence);
        }

        if c == '\x1b' && chars.peek() == Some(&'\\') {
            sequence.push(chars.next()?);
            return Some(sequence);
        }
    }

    None
}

/// Identifies an SGR reset sequence.
fn is_sgr_reset(sequence: &str) -> bool {
    let parameters = sequence
        .strip_prefix("\x1b[")
        .and_then(|s| s.strip_suffix('m'));

    matches!(parameters, Some("" | "0"))
}

/// Tokenizes text into Unicode characters and complete ANSI sequences.
///
/// Incomplete ANSI sequences are preserved as ordinary characters.
fn tokenize(text: &str) -> Vec<Token> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Probe on a copy so an incomplete sequence is left in the stream
            // and read as ordinary characters.
            let mut probe = chars.clone();
            let sequence = match chars.peek() {
                Some('[') => consume_csi(&mut probe),
                Some(']') => consume_osc(&mut probe),
                _ => None,
            };
            if sequence.is_some() {
                chars = probe;
            }

            if let Some(sequence) = sequence {
                let sgr = if sequence.starts_with("\x1b[") && sequence.ends_with('m') {
                    Some(sequence.clone())
                } else {
                    None
                };

                let reset = sgr.as_deref().is_some_and(is_sgr_reset);

                tokens.push(Token {
                    raw: sequence,
                    width: 0,
                    whitespace: false,
                    sgr,
                    reset,
                });

                continue;
            }
        }

        let whitespace = c == ' ' || c == '\t';

        tokens.push(Token {
            raw: if whitespace {
                " ".to_owned()
            } else {
                c.to_string()
            },
            width: char_width(c),
            whitespace,
            sgr: None,
            reset: false,
        });
    }

    tokens
}

/// Returns the number of terminal columns occupied by a string.
///
/// ANSI CSI and OSC escape sequences are ignored. Control characters,
/// combining marks, and the specified zero-width Unicode ranges occupy
/// zero columns. East Asian wide characters and supported emoji occupy
/// two columns.
///
/// # Examples
///
/// ```
/// use aeond::textwrap::display_width;
///
/// assert_eq!(display_width("Hello"), 5);
/// assert_eq!(display_width("你好"), 4);
/// assert_eq!(display_width("\x1b[31mRed\x1b[0m"), 3);
/// ```
#[must_use]
pub fn display_width(s: &str) -> usize {
    tokenize(s).iter().map(|token| token.width).sum()
}

/// Updates the active SGR color state using a token.
fn update_color(active: &mut Option<String>, token: &Token) {
    if let Some(sgr) = &token.sgr {
        if token.reset {
            *active = None;
        } else {
            *active = Some(sgr.clone());
        }
    }
}

/// Appends a token to a line and updates its color state.
fn append_token(output: &mut String, token: &Token, active: &mut Option<String>) {
    output.push_str(&token.raw);
    update_color(active, token);
}

/// Appends a reset sequence when a line ends with active color.
fn close_color(output: &mut String, active: Option<&String>) {
    if active.is_some() {
        output.push_str("\x1b[0m");
    }
}

/// Returns the display width of a token sequence.
fn tokens_width(tokens: &[Token]) -> usize {
    tokens.iter().map(|token| token.width).sum()
}

/// Wraps one paragraph while retaining its indentation.
///
/// Words are separated by whitespace tokens. Consecutive internal spaces
/// collapse into one space. Tabs are normalized to spaces.
fn wrap_paragraph(paragraph: &str, width: usize, output: &mut Vec<String>) {
    let tokens = tokenize(paragraph);

    if tokens.is_empty() {
        output.push(String::new());
        return;
    }

    let mut indent_tokens = Vec::new();
    let mut position = 0;

    while let Some(token) = tokens.get(position) {
        if !token.whitespace {
            break;
        }

        indent_tokens.push(token.clone());
        position += 1;
    }

    let indent_width = tokens_width(&indent_tokens);
    let indent = indent_tokens
        .iter()
        .map(|token| token.raw.as_str())
        .collect::<String>();

    let effective_indent_width = indent_width.min(width.saturating_sub(1));

    let mut words: Vec<Vec<Token>> = Vec::new();
    let mut word = Vec::new();

    for token in tokens.into_iter().skip(position) {
        if token.whitespace {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(token);
        }
    }

    if !word.is_empty() {
        words.push(word);
    }

    if words.is_empty() {
        output.push(indent);
        return;
    }

    let mut line = indent.clone();
    let mut line_width = effective_indent_width;
    let mut active_color: Option<String> = None;

    for word in words {
        let word_width = tokens_width(&word);

        if line_width > effective_indent_width
            && line_width.saturating_add(1).saturating_add(word_width) > width
        {
            close_color(&mut line, active_color.as_ref());
            output.push(line);

            line = indent.clone();
            line_width = effective_indent_width;

            if let Some(color) = &active_color {
                line.push_str(color);
            }
        }

        let mut pending_space = line_width > effective_indent_width;

        for token in word {
            if pending_space {
                if line_width.saturating_add(1).saturating_add(token.width) > width {
                    close_color(&mut line, active_color.as_ref());
                    output.push(line);

                    line = indent.clone();
                    line_width = effective_indent_width;

                    if let Some(color) = &active_color {
                        line.push_str(color);
                    }
                } else {
                    line.push(' ');
                    line_width += 1;
                }

                pending_space = false;
            }

            // A wide character cannot be split: when it does not fit on a line
            // that already has content it starts a new line, and on an empty
            // line it is emitted intact even if only one column is available.
            if token.width > 0
                && line_width.saturating_add(token.width) > width
                && line_width > effective_indent_width
            {
                close_color(&mut line, active_color.as_ref());
                output.push(line);

                line = indent.clone();
                line_width = effective_indent_width;

                if let Some(color) = &active_color {
                    line.push_str(color);
                }
            }

            append_token(&mut line, &token, &mut active_color);
            line_width = line_width.saturating_add(token.width);
        }
    }

    close_color(&mut line, active_color.as_ref());
    output.push(line);
}

/// Greedily wraps text to a fixed terminal width.
///
/// Existing newlines are preserved as paragraph boundaries, including blank
/// lines. Leading indentation is retained and applied to continuation lines.
/// Internal runs of spaces and tabs collapse to one space. Long words are
/// hard-split at Unicode character boundaries; ANSI escape sequences remain
/// intact. Width zero is treated as width one.
///
/// An empty input returns an empty vector.
///
/// # Examples
///
/// ```
/// use aeond::textwrap::wrap;
///
/// assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
/// assert_eq!(wrap("", 10), Vec::<String>::new());
/// assert_eq!(wrap("hello", 0), vec!["h", "e", "l", "l", "o"]);
/// ```
#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let width = width.max(1);
    let mut output = Vec::new();

    for paragraph in text.split('\n') {
        wrap_paragraph(paragraph, width, &mut output);
    }

    output
}

/// Truncates a string to the specified terminal width using an ellipsis.
///
/// The ellipsis consists of three ASCII dots. Widths smaller than three
/// return only as many dots as fit. ANSI sequences are preserved when
/// they occur before the truncation boundary.
///
/// # Examples
///
/// ```
/// use aeond::textwrap::truncate_with_ellipsis;
///
/// assert_eq!(truncate_with_ellipsis("Hello world", 8), "Hello...");
/// assert_eq!(truncate_with_ellipsis("Hello", 2), "..");
/// ```
#[must_use]
pub fn truncate_with_ellipsis(s: &str, width: usize) -> String {
    if width < 3 {
        return ".".repeat(width);
    }

    if display_width(s) <= width {
        return s.to_owned();
    }

    let target = width - 3;
    let tokens = tokenize(s);
    let mut output = String::new();
    let mut used = 0;

    for token in tokens {
        if token.width > 0 && used + token.width > target {
            break;
        }

        used += token.width;
        output.push_str(&token.raw);
    }

    output.push_str("...");
    output
}

/// Pads a string on the right to the requested terminal width.
///
/// Strings already at or above the requested width are unchanged.
///
/// # Examples
///
/// ```
/// use aeond::textwrap::pad_right;
///
/// assert_eq!(pad_right("Hi", 5), "Hi   ");
/// ```
#[must_use]
pub fn pad_right(s: &str, width: usize) -> String {
    let current = display_width(s);

    if current >= width {
        return s.to_owned();
    }

    let mut output = s.to_owned();
    output.push_str(&" ".repeat(width - current));
    output
}

/// Pads a string on the left to the requested terminal width.
///
/// Strings already at or above the requested width are unchanged.
///
/// # Examples
///
/// ```
/// use aeond::textwrap::pad_left;
///
/// assert_eq!(pad_left("Hi", 5), "   Hi");
/// ```
#[must_use]
pub fn pad_left(s: &str, width: usize) -> String {
    let current = display_width(s);

    if current >= width {
        return s.to_owned();
    }

    format!("{}{}", " ".repeat(width - current), s)
}

/// Creates a fixed-width textual progress bar.
///
/// The result is exactly `width` columns wide: `[`, the bar, `]`, a space and
/// the percentage. The minimum supported width is eight columns; smaller widths
/// return an empty string. A zero total produces zero percent, and progress
/// exceeding the total is clamped to 100 percent.
///
/// # Examples
///
/// ```
/// use aeond::textwrap::progress_bar;
///
/// assert_eq!(progress_bar(5, 10, 16), "[#####-----] 50%");
/// assert_eq!(progress_bar(0, 0, 7), "");
/// ```
#[must_use]
pub fn progress_bar(done: u64, total: u64, width: usize) -> String {
    if width < 8 {
        return String::new();
    }

    let percentage = (u128::from(done.min(total)) * 100)
        .checked_div(u128::from(total))
        .unwrap_or(0)
        .min(100);
    let label = format!(" {percentage}%");
    let bar_width = width.saturating_sub(label.len() + 2);

    let filled = if total == 0 {
        0
    } else {
        let value = done.min(total);
        let scaled = u128::from(value) * u128::try_from(bar_width).unwrap_or(0);
        usize::try_from(scaled / u128::from(total)).unwrap_or(0)
    };

    let filled = filled.min(bar_width);
    let empty = bar_width.saturating_sub(filled);

    format!("[{}{}]{}", "#".repeat(filled), "-".repeat(empty), label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
        assert_eq!(display_width("12345"), 5);
    }

    #[test]
    fn display_width_wide_characters() {
        assert_eq!(display_width("你好"), 4);
        assert_eq!(display_width("Ａ"), 2);
        assert_eq!(display_width("한"), 2);
    }

    #[test]
    fn display_width_emoji() {
        assert_eq!(display_width("😀"), 2);
        assert_eq!(display_width("😀😀"), 4);
    }

    #[test]
    fn display_width_combining_marks() {
        assert_eq!(display_width("e\u{0301}"), 1);
        assert_eq!(display_width("\u{0301}"), 0);
    }

    #[test]
    fn display_width_control_characters() {
        assert_eq!(display_width("\t\n\r"), 0);
        assert_eq!(display_width("\u{007f}\u{009f}"), 0);
    }

    #[test]
    fn display_width_ansi() {
        assert_eq!(display_width("\x1b[31mRed\x1b[0m"), 3);
        assert_eq!(display_width("\x1b]0;Title\x07Hello"), 5);
        assert_eq!(display_width("\x1b]0;Title\x1b\\Hello"), 5);
    }

    #[test]
    fn malformed_escape_sequences() {
        assert_eq!(display_width("\x1b"), 0);
        assert_eq!(display_width("\x1b["), 1);
        assert_eq!(display_width("\x1b]unfinished"), 11);
        assert_eq!(display_width("\x1b[31"), 3);
    }

    #[test]
    fn basic_wrap() {
        assert_eq!(
            wrap("one two three four", 8),
            vec!["one two", "three", "four"]
        );
    }

    #[test]
    fn empty_input() {
        assert!(wrap("", 10).is_empty());
    }

    #[test]
    fn preserves_blank_lines() {
        assert_eq!(wrap("first\n\nthird", 20), vec!["first", "", "third"]);
    }

    #[test]
    fn preserves_trailing_newline() {
        assert_eq!(wrap("hello\n", 10), vec!["hello", ""]);
    }

    #[test]
    fn preserves_indentation() {
        assert_eq!(
            wrap("  one two three", 8),
            vec!["  one", "  two", "  three"]
        );
    }

    #[test]
    fn collapses_spaces_and_tabs() {
        assert_eq!(wrap("one   two\t\tthree", 20), vec!["one two three"]);
    }

    #[test]
    fn hard_split_long_word() {
        assert_eq!(wrap("abcdefghij", 3), vec!["abc", "def", "ghi", "j"]);
    }

    #[test]
    fn wide_character_at_boundary() {
        assert_eq!(wrap("ab你好", 3), vec!["ab", "你", "好"]);
        assert_eq!(wrap("a你好", 4), vec!["a你", "好"]);
    }

    #[test]
    fn emoji_wrap() {
        assert_eq!(wrap("😀😀😀", 4), vec!["😀😀", "😀"]);
    }

    #[test]
    fn combining_marks_wrap() {
        assert_eq!(
            wrap("e\u{0301}e\u{0301}", 1),
            vec!["e\u{0301}", "e\u{0301}"]
        );
    }

    #[test]
    fn zero_and_one_width() {
        assert_eq!(wrap("abc", 0), vec!["a", "b", "c"]);
        assert_eq!(wrap("abc", 1), vec!["a", "b", "c"]);
    }

    #[test]
    fn ansi_color_width() {
        assert_eq!(display_width("\x1b[31mred\x1b[0m"), 3);
    }

    #[test]
    fn color_carries_across_lines() {
        let result = wrap("\x1b[31mred green blue\x1b[0m", 5);

        assert_eq!(result.len(), 3);
        assert!(result.first().is_some_and(|line| line.ends_with("\x1b[0m")));
        assert!(result
            .get(1)
            .is_some_and(|line| line.starts_with("\x1b[31m")));
        assert!(result.get(1).is_some_and(|line| line.ends_with("\x1b[0m")));
        assert!(result
            .get(2)
            .is_some_and(|line| line.starts_with("\x1b[31m")));
        assert!(result.get(2).is_some_and(|line| line.contains("\x1b[0m")));
    }

    #[test]
    fn ansi_reset_clears_color() {
        let result = wrap("\x1b[31mred\x1b[0m blue green", 5);

        assert!(result.len() > 1);
        assert!(result
            .get(1)
            .is_some_and(|line| !line.starts_with("\x1b[31m")));
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_with_ellipsis("Hello world", 8), "Hello...");
        assert_eq!(truncate_with_ellipsis("Hello", 5), "Hello");
        assert_eq!(truncate_with_ellipsis("Hello", 3), "...");
        assert_eq!(truncate_with_ellipsis("Hello", 2), "..");
        assert_eq!(truncate_with_ellipsis("Hello", 1), ".");
        assert_eq!(truncate_with_ellipsis("Hello", 0), "");
        assert_eq!(truncate_with_ellipsis("", 5), "");
    }

    #[test]
    fn truncation_wide_characters() {
        assert_eq!(truncate_with_ellipsis("你好世界", 5), "你...");
        assert_eq!(truncate_with_ellipsis("你好世界", 7), "你好...");
    }

    #[test]
    fn truncation_ansi() {
        assert_eq!(
            truncate_with_ellipsis("\x1b[31mHello\x1b[0m world", 8),
            "\x1b[31mHello\x1b[0m..."
        );
    }

    #[test]
    fn padding() {
        assert_eq!(pad_right("Hi", 5), "Hi   ");
        assert_eq!(pad_left("Hi", 5), "   Hi");
        assert_eq!(pad_right("Hello", 3), "Hello");
        assert_eq!(pad_left("Hello", 3), "Hello");
    }

    #[test]
    fn padding_wide_characters() {
        assert_eq!(pad_right("你", 4), "你  ");
        assert_eq!(pad_left("你", 4), "  你");
    }

    #[test]
    fn progress_bar_fills_exactly_the_width() {
        assert_eq!(progress_bar(5, 10, 16), "[#####-----] 50%");
        assert_eq!(progress_bar(10, 10, 16), "[#########] 100%");
        assert_eq!(progress_bar(0, 10, 16), "[-----------] 0%");
        for done in 0..=10 {
            assert_eq!(display_width(&progress_bar(done, 10, 24)), 24);
        }
    }

    #[test]
    fn progress_bar_boundaries() {
        assert_eq!(progress_bar(0, 0, 16), "[-----------] 0%");
        assert_eq!(progress_bar(20, 10, 16), "[#########] 100%");
        assert_eq!(progress_bar(1, 10, 7), "");
        assert_eq!(progress_bar(1, 10, 8), "[--] 10%");
    }

    #[test]
    fn progress_bar_large_values() {
        assert_eq!(progress_bar(u64::MAX, u64::MAX, 20), "[#############] 100%");
    }

    #[test]
    fn malformed_ansi_is_preserved() {
        let result = wrap("\x1b[31", 10);
        assert_eq!(result.len(), 1);
        assert_eq!(result.first().map(String::as_str), Some("\x1b[31"));
    }

    #[test]
    fn long_word_with_wide_characters() {
        assert_eq!(
            wrap("你好你好你好", 3),
            vec!["你", "好", "你", "好", "你", "好"]
        );
    }
}
