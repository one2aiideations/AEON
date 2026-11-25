//! Colouring for source text, one line at a time.
//!
//! This is deliberately simple: each line is classified on its own, so a block comment or a
//! string that runs over several lines is only coloured on the line where it starts. It never
//! fails and never reads past the line, so damaged or odd text is only ever coloured wrongly,
//! never refused.

use crate::grid::{theme, Color};

/// The language of a file, chosen from its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// Rust source.
    Rust,
    /// TOML configuration.
    Toml,
    /// Markdown text.
    Markdown,
    /// HTML pages.
    Html,
    /// CSS style sheets.
    Css,
    /// JavaScript.
    JavaScript,
    /// JSON data.
    Json,
    /// Python.
    Python,
    /// Shell scripts.
    Shell,
    /// Anything else.
    Plain,
}

impl Lang {
    /// The language for a file path.
    pub fn of(path: &str) -> Self {
        match path
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("rs") => Lang::Rust,
            Some("toml") => Lang::Toml,
            Some("md" | "markdown") => Lang::Markdown,
            Some("html" | "htm" | "xhtml") => Lang::Html,
            Some("css") => Lang::Css,
            Some("js" | "mjs" | "cjs") => Lang::JavaScript,
            Some("json") => Lang::Json,
            Some("py") => Lang::Python,
            Some("sh" | "bash") => Lang::Shell,
            _ => Lang::Plain,
        }
    }

    /// The text that starts a line comment, if the language has one.
    pub const fn line_comment(self) -> Option<&'static str> {
        match self {
            Lang::Rust | Lang::JavaScript => Some("// "),
            Lang::Toml | Lang::Python | Lang::Shell => Some("# "),
            _ => None,
        }
    }

    /// The name shown in the status line.
    pub const fn name(self) -> &'static str {
        match self {
            Lang::Rust => "Rust",
            Lang::Toml => "TOML",
            Lang::Markdown => "Markdown",
            Lang::Html => "HTML",
            Lang::Css => "CSS",
            Lang::JavaScript => "JavaScript",
            Lang::Json => "JSON",
            Lang::Python => "Python",
            Lang::Shell => "Shell",
            Lang::Plain => "Text",
        }
    }
}

/// What a character is part of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Ordinary text.
    Plain,
    /// A keyword.
    Keyword,
    /// A type name.
    Type,
    /// A string or code literal.
    Str,
    /// A comment.
    Comment,
    /// A number.
    Number,
    /// A heading or table name.
    Heading,
    /// A key in a configuration file.
    Key,
}

impl Kind {
    /// The colour used on the editor background.
    pub const fn colour(self) -> Color {
        match self {
            Kind::Plain => theme::TEXT,
            Kind::Keyword | Kind::Key => theme::ACCENT,
            Kind::Type => Color(0xCE93D8),
            Kind::Str => theme::OK,
            Kind::Comment => theme::DIM,
            Kind::Number | Kind::Heading => theme::WARN,
        }
    }
}

const RUST_KEYWORDS: [&str; 38] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Marks a string literal that starts at `i` (the opening quote). Returns the index after it.
fn string_end(chars: &[char], i: usize, quote: char) -> usize {
    let mut j = i + 1;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            c if c == quote => return j + 1,
            _ => j += 1,
        }
    }
    chars.len()
}

fn fill(kinds: &mut [Kind], from: usize, to: usize, k: Kind) {
    let to = to.min(kinds.len());
    for slot in kinds.iter_mut().take(to).skip(from) {
        *slot = k;
    }
}

fn classify_rust(chars: &[char], kinds: &mut [Kind]) {
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            fill(kinds, i, chars.len(), Kind::Comment);
            return;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                .map_or(chars.len(), |j| j + 2);
            fill(kinds, i, end, Kind::Comment);
            i = end;
        } else if c == '"' {
            let end = string_end(chars, i, '"');
            fill(kinds, i, end, Kind::Str);
            i = end;
        } else if c == '\'' {
            // A character literal has its closing quote within a few characters; otherwise it
            // is a lifetime.
            let close = if chars.get(i + 1) == Some(&'\\') {
                (i + 3..(i + 12).min(chars.len())).find(|&j| chars[j] == '\'')
            } else if chars.get(i + 2) == Some(&'\'') {
                Some(i + 2)
            } else {
                None
            };
            match close {
                Some(j) => {
                    fill(kinds, i, j + 1, Kind::Str);
                    i = j + 1;
                }
                None => i += 1,
            }
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && (is_word(chars[j]) || chars[j] == '.') {
                // A dot belongs to the number only when a digit follows (not `1..2` or `x.0.len`).
                if chars[j] == '.' && !chars.get(j + 1).is_some_and(char::is_ascii_digit) {
                    break;
                }
                j += 1;
            }
            fill(kinds, i, j, Kind::Number);
            i = j;
        } else if is_word(c) {
            let mut j = i;
            while j < chars.len() && is_word(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let k = if RUST_KEYWORDS.contains(&word.as_str()) {
                Kind::Keyword
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                Kind::Type
            } else {
                Kind::Plain
            };
            fill(kinds, i, j, k);
            i = j;
        } else {
            i += 1;
        }
    }
}

fn classify_toml(chars: &[char], kinds: &mut [Kind]) {
    let first = chars.iter().position(|c| !c.is_whitespace());
    if first.is_some_and(|f| chars[f] == '[') {
        fill(kinds, 0, chars.len(), Kind::Heading);
        return;
    }
    let mut i = 0;
    let mut seen_eq = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            fill(kinds, i, chars.len(), Kind::Comment);
            return;
        } else if c == '"' || c == '\'' {
            let end = string_end(chars, i, c);
            fill(kinds, i, end, Kind::Str);
            i = end;
            continue;
        } else if c == '=' && !seen_eq {
            seen_eq = true;
            let end = chars[..i]
                .iter()
                .rposition(|c| !c.is_whitespace())
                .map_or(0, |p| p + 1);
            let start = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
            fill(kinds, start, end, Kind::Key);
        } else if seen_eq
            && (c.is_ascii_digit()
                || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)))
        {
            let mut j = i + 1;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '.' | '_' | ':' | '-'))
            {
                j += 1;
            }
            fill(kinds, i, j, Kind::Number);
            i = j;
            continue;
        } else if seen_eq && is_word(c) {
            let mut j = i;
            while j < chars.len() && is_word(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if word == "true" || word == "false" {
                fill(kinds, i, j, Kind::Keyword);
            }
            i = j;
            continue;
        }
        i += 1;
    }
}

fn classify_markdown(chars: &[char], kinds: &mut [Kind]) {
    let line: String = chars.iter().collect();
    let t = line.trim_start();
    if t.starts_with('#') || t.starts_with("```") {
        let k = if t.starts_with('#') {
            Kind::Heading
        } else {
            Kind::Str
        };
        fill(kinds, 0, chars.len(), k);
        return;
    }
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(j) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                fill(kinds, i, j + 1, Kind::Str);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
}

const JS_KEYWORDS: [&str; 36] = [
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "while",
];

const PY_KEYWORDS: [&str; 30] = [
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "if", "import", "in", "is", "lambda",
    "None", "not", "or", "pass", "return", "True", "try", "while",
];

const SH_KEYWORDS: [&str; 16] = [
    "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "local",
    "return", "then", "until", "while",
];

/// Colours a line of a C-like or scripting language: comments, strings, numbers and keywords.
fn classify_generic(
    chars: &[char],
    kinds: &mut [Kind],
    keywords: &[&str],
    comment: &str,
    quotes: &[char],
) {
    let cm: Vec<char> = comment.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !cm.is_empty() && chars[i..].starts_with(&cm) {
            fill(kinds, i, chars.len(), Kind::Comment);
            return;
        }
        if comment == "//" && c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                .map_or(chars.len(), |j| j + 2);
            fill(kinds, i, end, Kind::Comment);
            i = end;
        } else if quotes.contains(&c) {
            let end = string_end(chars, i, c);
            fill(kinds, i, end, Kind::Str);
            i = end;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len()
                && (is_word(chars[j])
                    || (chars[j] == '.' && chars.get(j + 1).is_some_and(char::is_ascii_digit)))
            {
                j += 1;
            }
            fill(kinds, i, j, Kind::Number);
            i = j;
        } else if is_word(c) || c == '$' {
            let mut j = i + 1;
            while j < chars.len() && (is_word(chars[j]) || chars[j] == '$') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let k = if keywords.contains(&word.as_str()) {
                Kind::Keyword
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                Kind::Type
            } else {
                Kind::Plain
            };
            fill(kinds, i, j, k);
            i = j;
        } else {
            i += 1;
        }
    }
}

fn classify_json(chars: &[char], kinds: &mut [Kind]) {
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            let end = string_end(chars, i, '"');
            // A string followed by a colon is a key.
            let after = chars[end.min(chars.len())..]
                .iter()
                .find(|c| !c.is_whitespace());
            fill(
                kinds,
                i,
                end,
                if after == Some(&':') {
                    Kind::Key
                } else {
                    Kind::Str
                },
            );
            i = end;
        } else if c.is_ascii_digit()
            || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let mut j = i + 1;
            while j < chars.len()
                && (chars[j].is_ascii_digit() || matches!(chars[j], '.' | 'e' | 'E' | '+' | '-'))
            {
                j += 1;
            }
            fill(kinds, i, j, Kind::Number);
            i = j;
        } else if is_word(c) {
            let mut j = i;
            while j < chars.len() && is_word(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if matches!(word.as_str(), "true" | "false" | "null") {
                fill(kinds, i, j, Kind::Keyword);
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

fn classify_css(chars: &[char], kinds: &mut [Kind]) {
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                .map_or(chars.len(), |j| j + 2);
            fill(kinds, i, end, Kind::Comment);
            i = end;
        } else if c == '"' || c == '\'' {
            let end = string_end(chars, i, c);
            fill(kinds, i, end, Kind::Str);
            i = end;
        } else if c.is_ascii_digit()
            || (c == '#' && chars[..i].contains(&':') && !chars[i..].contains(&'{'))
        {
            let mut j = i + 1;
            while j < chars.len() && (is_word(chars[j]) || chars[j] == '.' || chars[j] == '%') {
                j += 1;
            }
            fill(kinds, i, j, Kind::Number);
            i = j;
        } else if c == '.' || c == '#' || c == '@' {
            let mut j = i + 1;
            while j < chars.len() && (is_word(chars[j]) || chars[j] == '-') {
                j += 1;
            }
            fill(
                kinds,
                i,
                j,
                if c == '@' { Kind::Keyword } else { Kind::Type },
            );
            i = j;
        } else {
            i += 1;
        }
    }
    // `property: value` lines inside a rule: colour the property name.
    let first = chars.iter().position(|c| !c.is_whitespace());
    if let (Some(f), Some(colon)) = (first, chars.iter().position(|c| *c == ':')) {
        let name: String = chars[f..colon].iter().collect();
        if colon > f
            && name
                .trim()
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == '-')
            && chars.contains(&';')
        {
            fill(kinds, f, f + name.trim_end().chars().count(), Kind::Key);
        }
    }
}

fn classify_html(chars: &[char], kinds: &mut [Kind]) {
    let mut i = 0;
    while i < chars.len() {
        if chars[i..].starts_with(&['<', '!', '-', '-']) {
            let end = (i + 4..chars.len().saturating_sub(2))
                .find(|&j| chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>')
                .map_or(chars.len(), |j| j + 3);
            fill(kinds, i, end, Kind::Comment);
            i = end;
        } else if chars[i] == '<'
            && chars
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == '/' || *c == '!')
        {
            let mut j = i + 1;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '/' | '!' | '-'))
            {
                j += 1;
            }
            fill(kinds, i, j, Kind::Keyword);
            // Attributes up to the closing `>`.
            while j < chars.len() && chars[j] != '>' {
                if chars[j] == '"' || chars[j] == '\'' {
                    let end = string_end(chars, j, chars[j]);
                    fill(kinds, j, end, Kind::Str);
                    j = end;
                } else if is_word(chars[j]) || chars[j] == '-' {
                    let st = j;
                    while j < chars.len() && (is_word(chars[j]) || chars[j] == '-') {
                        j += 1;
                    }
                    fill(kinds, st, j, Kind::Key);
                } else {
                    j += 1;
                }
            }
            if j < chars.len() {
                fill(kinds, j, j + 1, Kind::Keyword);
                j += 1;
            }
            i = j;
        } else if chars[i] == '&' {
            let end = (i + 1..(i + 10).min(chars.len()))
                .find(|&j| chars[j] == ';')
                .map_or(i + 1, |j| j + 1);
            fill(kinds, i, end, Kind::Number);
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
}

/// The kind of every character of `line`.
pub fn classify(lang: Lang, line: &[char]) -> Vec<Kind> {
    let mut kinds = vec![Kind::Plain; line.len()];
    match lang {
        Lang::Rust => classify_rust(line, &mut kinds),
        Lang::Toml => classify_toml(line, &mut kinds),
        Lang::Markdown => classify_markdown(line, &mut kinds),
        Lang::Html => classify_html(line, &mut kinds),
        Lang::Css => classify_css(line, &mut kinds),
        Lang::Json => classify_json(line, &mut kinds),
        Lang::JavaScript => {
            classify_generic(line, &mut kinds, &JS_KEYWORDS, "//", &['"', '\'', '`'])
        }
        Lang::Python => classify_generic(line, &mut kinds, &PY_KEYWORDS, "#", &['"', '\'']),
        Lang::Shell => classify_generic(line, &mut kinds, &SH_KEYWORDS, "#", &['"', '\'']),
        Lang::Plain => {}
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: Lang, s: &str) -> Vec<Kind> {
        classify(lang, &s.chars().collect::<Vec<_>>())
    }

    fn at(lang: Lang, s: &str, needle: &str) -> Kind {
        let i = s.find(needle).unwrap();
        kinds(lang, s)[s[..i].chars().count()]
    }

    #[test]
    fn languages_come_from_the_file_name() {
        assert_eq!(Lang::of("src/lib.rs"), Lang::Rust);
        assert_eq!(Lang::of("Cargo.toml"), Lang::Toml);
        assert_eq!(Lang::of("README.MD"), Lang::Markdown);
        assert_eq!(Lang::of("Makefile"), Lang::Plain);
        assert_eq!(Lang::of(""), Lang::Plain);
    }

    #[test]
    fn rust_keywords_types_strings_numbers_and_comments_are_told_apart() {
        let s = "pub fn tax(x: Money) -> i64 { let n = 42; \"hi\" } // note";
        assert_eq!(at(Lang::Rust, s, "pub"), Kind::Keyword);
        assert_eq!(at(Lang::Rust, s, "fn"), Kind::Keyword);
        assert_eq!(at(Lang::Rust, s, "tax"), Kind::Plain);
        assert_eq!(at(Lang::Rust, s, "Money"), Kind::Type);
        assert_eq!(at(Lang::Rust, s, "42"), Kind::Number);
        assert_eq!(at(Lang::Rust, s, "\"hi\""), Kind::Str);
        assert_eq!(at(Lang::Rust, s, "// note"), Kind::Comment);
        assert_eq!(at(Lang::Rust, s, "note"), Kind::Comment);
    }

    #[test]
    fn a_slash_slash_inside_a_string_is_not_a_comment() {
        let s = "let u = \"http://x\"; let z = 1;";
        assert_eq!(at(Lang::Rust, s, "//x"), Kind::Str);
        assert_eq!(at(Lang::Rust, s, "z"), Kind::Plain);
        assert_eq!(at(Lang::Rust, s, "1;"), Kind::Number);
    }

    #[test]
    fn lifetimes_are_not_character_literals_and_chars_are() {
        let s = "fn f<'a>(x: &'a str) -> char { 'x' }";
        assert_eq!(at(Lang::Rust, s, "'a>"), Kind::Plain);
        assert_eq!(at(Lang::Rust, s, "'x'"), Kind::Str);
        let e = "let c = '\\n'; let d = 2;";
        assert_eq!(at(Lang::Rust, e, "'\\n'"), Kind::Str);
        assert_eq!(at(Lang::Rust, e, "2"), Kind::Number);
    }

    #[test]
    fn ranges_and_field_numbers_do_not_swallow_the_dots() {
        let s = "for i in 0..10 { t.0.len() }";
        assert_eq!(at(Lang::Rust, s, "0.."), Kind::Number);
        assert_eq!(at(Lang::Rust, s, ".."), Kind::Plain);
        assert_eq!(at(Lang::Rust, s, "10"), Kind::Number);
        assert_eq!(at(Lang::Rust, s, "len"), Kind::Plain);
    }

    #[test]
    fn a_block_comment_on_one_line_ends_where_it_closes() {
        let s = "let a /* why */ = 3;";
        assert_eq!(at(Lang::Rust, s, "why"), Kind::Comment);
        assert_eq!(at(Lang::Rust, s, "= 3"), Kind::Plain);
        assert_eq!(at(Lang::Rust, s, "3"), Kind::Number);
    }

    #[test]
    fn toml_tables_keys_strings_numbers_and_comments() {
        assert_eq!(at(Lang::Toml, "[dependencies]", "depend"), Kind::Heading);
        let s = "name = \"mouli\" # the name";
        assert_eq!(at(Lang::Toml, s, "name"), Kind::Key);
        assert_eq!(at(Lang::Toml, s, "\"mouli\""), Kind::Str);
        assert_eq!(at(Lang::Toml, s, "# the"), Kind::Comment);
        assert_eq!(at(Lang::Toml, "n = 12", "12"), Kind::Number);
        assert_eq!(at(Lang::Toml, "on = true", "true"), Kind::Keyword);
        assert_eq!(at(Lang::Toml, "x = \"a#b\"", "#"), Kind::Str);
    }

    #[test]
    fn markdown_headings_and_inline_code() {
        assert_eq!(at(Lang::Markdown, "# Title", "Title"), Kind::Heading);
        assert_eq!(
            at(Lang::Markdown, "use `cargo test` now", "cargo"),
            Kind::Str
        );
        assert_eq!(
            at(Lang::Markdown, "use `cargo test` now", "now"),
            Kind::Plain
        );
        assert_eq!(at(Lang::Markdown, "```rust", "rust"), Kind::Str);
        assert_eq!(at(Lang::Markdown, "an `unclosed", "unclosed"), Kind::Plain);
    }

    #[test]
    fn odd_text_never_panics_and_always_gives_one_kind_per_character() {
        for s in [
            "",
            "\"",
            "'",
            "'\\",
            "/*",
            "/",
            "0.",
            "[",
            "=",
            "= =",
            "\u{1F600} \"\u{1F600}",
            "`",
            "\\\\\\\\",
            "'\\u{1F600}'",
        ] {
            for lang in [Lang::Rust, Lang::Toml, Lang::Markdown, Lang::Plain] {
                assert_eq!(kinds(lang, s).len(), s.chars().count(), "{lang:?} {s:?}");
            }
        }
    }

    #[test]
    fn every_colour_is_readable_on_the_editor_background() {
        fn lum(Color(c): Color) -> f64 {
            let f = |v: u32| {
                let v = f64::from(v as u8) / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c >> 16) + 0.7152 * f(c >> 8) + 0.0722 * f(c)
        }
        for k in [
            Kind::Plain,
            Kind::Keyword,
            Kind::Type,
            Kind::Str,
            Kind::Comment,
            Kind::Number,
            Kind::Heading,
            Kind::Key,
        ] {
            let (a, b) = (lum(k.colour()), lum(theme::BG));
            let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(ratio >= 4.5, "{k:?} {ratio}");
        }
    }

    #[test]
    fn web_and_scripting_languages_are_coloured() {
        assert_eq!(Lang::of("a/index.html"), Lang::Html);
        assert_eq!(Lang::of("x.CSS"), Lang::Css);
        assert_eq!(Lang::of("app.js"), Lang::JavaScript);
        assert_eq!(Lang::of("data.json"), Lang::Json);
        assert_eq!(Lang::of("tool.py"), Lang::Python);
        assert_eq!(Lang::of("run.sh"), Lang::Shell);
        let js = "const n = 42; // note";
        assert_eq!(at(Lang::JavaScript, js, "const"), Kind::Keyword);
        assert_eq!(at(Lang::JavaScript, js, "42"), Kind::Number);
        assert_eq!(at(Lang::JavaScript, js, "note"), Kind::Comment);
        assert_eq!(at(Lang::JavaScript, "let s = `a//b`;", "//b"), Kind::Str);
        assert_eq!(at(Lang::Python, "def f(): # c", "def"), Kind::Keyword);
        assert_eq!(at(Lang::Python, "def f(): # c", "# c"), Kind::Comment);
        assert_eq!(at(Lang::Shell, "if [ -f x ]; then", "then"), Kind::Keyword);
        let h = "<a href=\"/x\" id=y>t &amp; u</a><!-- c -->";
        assert_eq!(at(Lang::Html, h, "<a"), Kind::Keyword);
        assert_eq!(at(Lang::Html, h, "href"), Kind::Key);
        assert_eq!(at(Lang::Html, h, "\"/x\""), Kind::Str);
        assert_eq!(at(Lang::Html, h, "&amp;"), Kind::Number);
        assert_eq!(at(Lang::Html, h, "<!-- c -->"), Kind::Comment);
        assert_eq!(at(Lang::Html, h, "t &"), Kind::Plain);
        let j = "  \"name\": \"x\", \"n\": -1.5, \"ok\": true";
        assert_eq!(at(Lang::Json, j, "\"name\""), Kind::Key);
        assert_eq!(at(Lang::Json, j, "\"x\""), Kind::Str);
        assert_eq!(at(Lang::Json, j, "-1.5"), Kind::Number);
        assert_eq!(at(Lang::Json, j, "true"), Kind::Keyword);
        let c = "  color: #fff; /* c */";
        assert_eq!(at(Lang::Css, c, "color"), Kind::Key);
        assert_eq!(at(Lang::Css, c, "#fff"), Kind::Number);
        assert_eq!(at(Lang::Css, c, "/* c */"), Kind::Comment);
        assert_eq!(at(Lang::Css, ".box { margin: 0 }", ".box"), Kind::Type);
        assert_eq!(Lang::Rust.line_comment(), Some("// "));
        assert_eq!(Lang::Html.line_comment(), None);
        // Half-written lines never panic.
        for l in [
            "<",
            "<a href=\"",
            "&",
            "\"",
            "/*",
            "`",
            "#",
            ":",
            "{",
            "\\",
            "<!--",
        ] {
            for lang in [
                Lang::Html,
                Lang::Css,
                Lang::JavaScript,
                Lang::Json,
                Lang::Python,
                Lang::Shell,
            ] {
                let _ = kinds(lang, l);
            }
        }
    }
}
