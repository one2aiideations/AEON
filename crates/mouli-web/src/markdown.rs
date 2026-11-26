//! A small Markdown to HTML converter for previews.
//!
//! Supported: headings (`#` and underlined), paragraphs, emphasis, strong, strike-through, inline
//! code, fenced and indented code blocks, block quotes, ordered and unordered lists (nested by
//! indentation, task items), horizontal rules, links, images, autolinks, tables, hard line breaks.
//! **Raw HTML is never passed through**: every `<`, `>` and `&` in the text is escaped, so a
//! document cannot add markup or scripts to the preview. Link and image targets that start with
//! `javascript:`, `data:` or `vbscript:` are replaced by `#`.

/// Most input characters read.
pub const MAX_INPUT: usize = 2_000_000;

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

fn safe_url(u: &str) -> String {
    let t = u.trim();
    let lower: String = t
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
    {
        "#".into()
    } else {
        esc(t)
    }
}

/// Longest stretch searched for a closing delimiter, so unmatched markers cannot make parsing
/// quadratic.
const WINDOW: usize = 4096;

fn window(s: &str, n: usize) -> &str {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Inline formatting of one run of text (already known not to contain block syntax).
fn inline(text: &str, depth: usize) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < text.len() {
        let rest = &text[i..];
        let Some(c) = rest.chars().next() else { break };
        let clen = c.len_utf8();
        let mut advance = clen;
        match c {
            '\\' if rest[1..]
                .chars()
                .next()
                .is_some_and(|n| "\\`*_{}[]()#+-.!~|<>".contains(n)) =>
            {
                let n = rest[1..].chars().next().unwrap_or(' ');
                out.push_str(&esc(&n.to_string()));
                advance = 1 + n.len_utf8();
            }
            '`' => {
                let ticks = rest.bytes().take_while(|&b| b == b'`').count();
                let close = "`".repeat(ticks);
                let after = &rest[ticks..];
                if let Some(end) = window(after, WINDOW).find(&close) {
                    out.push_str(&format!("<code>{}</code>", esc(after[..end].trim())));
                    advance = ticks + end + ticks;
                } else {
                    out.push_str(&close);
                    advance = ticks;
                }
            }
            '!' if rest.starts_with("![") => match bracket_link(&rest[1..]) {
                Some((alt, url, used)) => {
                    out.push_str(&format!(
                        "<img src=\"{}\" alt=\"{}\">",
                        safe_url(&url),
                        esc(&alt)
                    ));
                    advance = 1 + used;
                }
                None => out.push('!'),
            },
            '[' => match bracket_link(rest) {
                Some((label, url, used)) if depth < 4 => {
                    out.push_str(&format!(
                        "<a href=\"{}\">{}</a>",
                        safe_url(&url),
                        inline(&label, depth + 1)
                    ));
                    advance = used;
                }
                _ => out.push('['),
            },
            '<' => {
                let mut handled = false;
                if let Some(end) = window(rest, WINDOW).find('>') {
                    let inner = &rest[1..end];
                    if (inner.starts_with("http://")
                        || inner.starts_with("https://")
                        || inner.starts_with("mailto:"))
                        && !inner.contains(char::is_whitespace)
                    {
                        out.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            safe_url(inner),
                            esc(inner)
                        ));
                        advance = end + 1;
                        handled = true;
                    }
                }
                if !handled {
                    out.push_str("&lt;");
                }
            }
            '*' | '_' | '~' => {
                let run = rest.bytes().take_while(|&b| b == c as u8).count();
                let (tag, n) = match (c, run) {
                    ('~', n) if n >= 2 => ("del", 2),
                    ('~', _) => ("", 0),
                    (_, n) if n >= 2 => ("strong", 2),
                    _ => ("em", 1),
                };
                let mut done = false;
                if n > 0 && depth < 6 {
                    let delim = &rest[..n];
                    let after = &rest[n..];
                    // `_` inside a word is not emphasis.
                    let word_inside = c == '_' && prev.is_some_and(char::is_alphanumeric);
                    if !word_inside {
                        if let Some(end) = find_close(window(after, WINDOW), delim) {
                            let inner = &after[..end];
                            if !inner.is_empty() && !inner.starts_with(char::is_whitespace) {
                                out.push_str(&format!(
                                    "<{tag}>{}</{tag}>",
                                    inline(inner, depth + 1)
                                ));
                                advance = n + end + n;
                                done = true;
                            }
                        }
                    }
                }
                if !done {
                    out.push(c);
                }
            }
            ' ' if rest.starts_with("  \n") => {
                out.push_str("<br>");
                advance = 3;
            }
            '\n' => out.push('\n'),
            c => out.push_str(&esc(&c.to_string())),
        }
        prev = text[i..i + advance].chars().last();
        i += advance;
    }
    out
}

fn find_close(s: &str, delim: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(p) = s[from..].find(delim) {
        let at = from + p;
        if at > 0 {
            return Some(at);
        }
        from = at + delim.len();
        if from >= s.len() {
            break;
        }
    }
    None
}

/// `[label](url "title")` at the start of `s`: the label, the url and the bytes used.
fn bracket_link(s: &str) -> Option<(String, String, usize)> {
    if !s.starts_with('[') {
        return None;
    }
    let scan = window(s, 2000);
    let mut depth = 0;
    let mut close = None;
    for (i, c) in scan.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = &s[close + 1..];
    let inner_start = after.strip_prefix('(')?;
    let end = window(inner_start, 2000).find(')')?;
    let target = &inner_start[..end];
    let url = target
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c| c == '<' || c == '>');
    Some((
        s[1..close].to_string(),
        url.to_string(),
        close + 1 + 1 + end + 1,
    ))
}

struct Ctx<'a> {
    lines: Vec<&'a str>,
}

fn indent_of(l: &str) -> usize {
    l.chars().take_while(|c| *c == ' ').count() + 3 * l.chars().take_while(|c| *c == '\t').count()
}

fn is_hr(l: &str) -> bool {
    let t = l.trim();
    t.len() >= 3
        && ["-", "*", "_"].iter().any(|d| {
            t.chars().all(|c| c.to_string() == *d || c == ' ') && t.matches(d).count() >= 3
        })
}

fn heading(l: &str) -> Option<(usize, &str)> {
    let t = l.trim_start();
    let n = t.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&n) && (t.len() == n || t[n..].starts_with(' ')) {
        Some((n, t[n..].trim().trim_end_matches('#').trim()))
    } else {
        None
    }
}

fn list_marker(l: &str) -> Option<(bool, usize, &str)> {
    let ind = indent_of(l);
    let t = l.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            return Some((false, ind, r));
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let after = &t[digits..];
        if let Some(r) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return Some((true, ind, r));
        }
    }
    None
}

fn table_row(l: &str) -> Option<Vec<String>> {
    let t = l.trim();
    if !t.contains('|') {
        return None;
    }
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    Some(t.split('|').map(|c| c.trim().to_string()).collect())
}

fn is_table_sep(l: &str) -> Option<Vec<&'static str>> {
    let cells = table_row(l)?;
    let mut aligns = Vec::new();
    for c in &cells {
        let t = c.trim();
        let inner = t.trim_matches(':');
        if inner.is_empty() || !inner.chars().all(|x| x == '-') {
            return None;
        }
        aligns.push(match (t.starts_with(':'), t.ends_with(':')) {
            (true, true) => "center",
            (false, true) => "right",
            (true, false) => "left",
            _ => "",
        });
    }
    Some(aligns)
}

impl Ctx<'_> {
    fn blocks(&self, from: usize, to: usize, depth: usize) -> String {
        let mut out = String::new();
        let mut i = from;
        while i < to {
            let l = self.lines[i];
            if l.trim().is_empty() {
                i += 1;
                continue;
            }
            if depth > 12 {
                out.push_str(&format!("<p>{}</p>\n", esc(l)));
                i += 1;
                continue;
            }
            // Fenced code.
            let t = l.trim_start();
            if t.starts_with("```") || t.starts_with("~~~") {
                let fence = &t[..3];
                let lang = esc(t[3..].split_whitespace().next().unwrap_or(""));
                let mut code = String::new();
                i += 1;
                while i < to && !self.lines[i].trim_start().starts_with(fence) {
                    code.push_str(self.lines[i]);
                    code.push('\n');
                    i += 1;
                }
                i += 1;
                let class = if lang.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{lang}\"")
                };
                out.push_str(&format!("<pre><code{class}>{}</code></pre>\n", esc(&code)));
                continue;
            }
            if indent_of(l) >= 4 && !l.trim().is_empty() && list_marker(l).is_none() {
                let mut code = String::new();
                while i < to && (self.lines[i].trim().is_empty() || indent_of(self.lines[i]) >= 4) {
                    code.push_str(
                        self.lines[i]
                            .get(4.min(self.lines[i].len())..)
                            .unwrap_or(""),
                    );
                    code.push('\n');
                    i += 1;
                }
                out.push_str(&format!(
                    "<pre><code>{}</code></pre>\n",
                    esc(code.trim_end())
                ));
                continue;
            }
            if let Some((n, text)) = heading(l) {
                out.push_str(&format!("<h{n}>{}</h{n}>\n", inline(text, 0)));
                i += 1;
                continue;
            }
            if is_hr(l) {
                out.push_str("<hr>\n");
                i += 1;
                continue;
            }
            if t.starts_with('>') {
                let start = i;
                let mut inner = String::new();
                while i < to && self.lines[i].trim_start().starts_with('>') {
                    let q = self.lines[i].trim_start()[1..]
                        .strip_prefix(' ')
                        .unwrap_or(&self.lines[i].trim_start()[1..]);
                    inner.push_str(q);
                    inner.push('\n');
                    i += 1;
                }
                let sub = Ctx {
                    lines: inner.lines().collect(),
                };
                let _ = start;
                out.push_str(&format!(
                    "<blockquote>{}</blockquote>\n",
                    sub.blocks(0, sub.lines.len(), depth + 1)
                ));
                continue;
            }
            if let Some((ordered, ind, _)) = list_marker(l) {
                let (html, next) = self.list(i, to, ordered, ind);
                out.push_str(&html);
                i = next;
                continue;
            }
            // A table: a header row followed by a separator row.
            if i + 1 < to {
                if let (Some(head), Some(aligns)) = (table_row(l), is_table_sep(self.lines[i + 1]))
                {
                    if head.len() == aligns.len() {
                        let mut html = String::from("<table>\n<tr>");
                        for (c, a) in head.iter().zip(&aligns) {
                            let style = if a.is_empty() {
                                String::new()
                            } else {
                                format!(" style=\"text-align:{a}\"")
                            };
                            html.push_str(&format!("<th{style}>{}</th>", inline(c, 0)));
                        }
                        html.push_str("</tr>\n");
                        i += 2;
                        while i < to && !self.lines[i].trim().is_empty() {
                            if let Some(cells) = table_row(self.lines[i]) {
                                html.push_str("<tr>");
                                for (k, a) in aligns.iter().enumerate() {
                                    let style = if a.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" style=\"text-align:{a}\"")
                                    };
                                    html.push_str(&format!(
                                        "<td{style}>{}</td>",
                                        inline(cells.get(k).map_or("", String::as_str), 0)
                                    ));
                                }
                                html.push_str("</tr>\n");
                            }
                            i += 1;
                        }
                        html.push_str("</table>\n");
                        out.push_str(&html);
                        continue;
                    }
                }
            }
            // A paragraph, possibly an underlined heading.
            let mut para = String::new();
            while i < to {
                let p = self.lines[i];
                let underline = {
                    let u = p.trim();
                    !u.is_empty()
                        && (u.chars().all(|c| c == '=')
                            || (u.chars().all(|c| c == '-') && u.len() >= 2))
                };
                if !para.is_empty() && underline {
                    let level = if p.trim().starts_with('=') { 1 } else { 2 };
                    out.push_str(&format!(
                        "<h{level}>{}</h{level}>\n",
                        inline(para.trim(), 0)
                    ));
                    para.clear();
                    i += 1;
                    break;
                }
                if p.trim().is_empty()
                    || (i > from
                        && (heading(p).is_some()
                            || is_hr(p)
                            || p.trim_start().starts_with("```")
                            || p.trim_start().starts_with('>')
                            || list_marker(p).is_some()))
                {
                    break;
                }
                para.push_str(p);
                para.push('\n');
                i += 1;
            }
            if !para.trim().is_empty() {
                out.push_str(&format!("<p>{}</p>\n", inline(para.trim(), 0)));
            }
        }
        out
    }

    fn list(&self, from: usize, to: usize, ordered: bool, base_indent: usize) -> (String, usize) {
        let tag = if ordered { "ol" } else { "ul" };
        let mut html = format!("<{tag}>\n");
        let mut i = from;
        while i < to {
            let l = self.lines[i];
            let Some((ord, ind, text)) = list_marker(l) else {
                break;
            };
            if ind < base_indent || (ind == base_indent && ord != ordered) {
                break;
            }
            if ind > base_indent {
                break;
            }
            let mut item = String::from(text);
            let mut j = i + 1;
            // Continuation lines and nested lists.
            let mut nested = String::new();
            while j < to {
                let n = self.lines[j];
                if n.trim().is_empty() {
                    // A blank line ends the item unless the next line is indented more.
                    if j + 1 < to
                        && indent_of(self.lines[j + 1]) > base_indent
                        && !self.lines[j + 1].trim().is_empty()
                    {
                        j += 1;
                        continue;
                    }
                    break;
                }
                if let Some((o2, ind2, _)) = list_marker(n) {
                    if ind2 > base_indent {
                        let (sub, next) = self.list(j, to, o2, ind2);
                        nested.push_str(&sub);
                        j = next;
                        continue;
                    }
                    break;
                }
                if indent_of(n) > base_indent {
                    item.push(' ');
                    item.push_str(n.trim());
                    j += 1;
                } else {
                    break;
                }
            }
            let (checkbox, body) = if let Some(r) = item.strip_prefix("[ ] ") {
                ("<input type=\"checkbox\"> ", r.to_string())
            } else if let Some(r) = item
                .strip_prefix("[x] ")
                .or_else(|| item.strip_prefix("[X] "))
            {
                ("<input type=\"checkbox\" checked> ", r.to_string())
            } else {
                ("", item.clone())
            };
            html.push_str(&format!(
                "<li>{checkbox}{}{nested}</li>\n",
                inline(body.trim(), 0)
            ));
            i = j;
        }
        html.push_str(&format!("</{tag}>\n"));
        (html, i.max(from + 1))
    }
}

/// Converts Markdown to an HTML document (a `body` of converted blocks with a small stylesheet
/// that makes it read well).
#[must_use]
pub fn to_html(md: &str) -> String {
    let md: String = md
        .chars()
        .take(MAX_INPUT)
        .collect::<String>()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let ctx = Ctx {
        lines: md.lines().collect(),
    };
    let body = ctx.blocks(0, ctx.lines.len(), 0);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>{STYLE}</style></head><body>{body}</body></html>"
    )
}

const STYLE: &str = "body{font-family:sans-serif;margin:16px 24px;color:#24292f;background:#fff}\
h1,h2{border-bottom:1px solid #d8dee4;padding-bottom:.3em}h1{font-size:2em}h2{font-size:1.5em}\
code{background:#eff1f3;padding:0 4px;border-radius:3px}pre{background:#f6f8fa;padding:12px;border-radius:6px}pre code{background:none;padding:0}\
blockquote{margin:1em 0;padding:0 1em;color:#57606a;border-left:4px solid #d0d7de}a{color:#0969da}\
table{border-collapse:collapse;margin:1em 0}th,td{border:1px solid #d0d7de;padding:6px 12px}th{background:#f6f8fa}hr{border:0;border-top:1px solid #d0d7de}img{max-width:100%}";

#[cfg(test)]
mod tests {
    use super::*;

    fn body(md: &str) -> String {
        let h = to_html(md);
        let s = h.find("<body>").unwrap() + 6;
        h[s..h.len() - "</body></html>".len()].to_string()
    }

    #[test]
    fn headings_paragraphs_and_inline_formatting() {
        let b =
            body("# Title\n\nSome **bold**, *italic*, ~~gone~~ and `code` text.\n\n## Sub ##\n");
        assert!(
            b.contains("<h1>Title</h1>") && b.contains("<h2>Sub</h2>"),
            "{b}"
        );
        assert!(b.contains("<p>Some <strong>bold</strong>, <em>italic</em>, <del>gone</del> and <code>code</code> text.</p>"), "{b}");
        let b = body("Title\n=====\n\nSub\n---\n");
        assert!(
            b.contains("<h1>Title</h1>") && b.contains("<h2>Sub</h2>"),
            "{b}"
        );
        assert!(
            body("snake_case_word and 2 * 3 * 4").contains("snake_case_word and 2 * 3 * 4"),
            "underscores inside words and spaced stars stay"
        );
        assert!(body("line one  \nline two").contains("<br>"));
    }

    #[test]
    fn links_images_and_autolinks() {
        let b = body("[site](https://example.com) and ![logo](a.png) and <https://x.org/p>");
        assert!(
            b.contains("<a href=\"https://example.com\">site</a>"),
            "{b}"
        );
        assert!(b.contains("<img src=\"a.png\" alt=\"logo\">"), "{b}");
        assert!(
            b.contains("<a href=\"https://x.org/p\">https://x.org/p</a>"),
            "{b}"
        );
        assert!(body("[bold **link**](u)").contains("<a href=\"u\">bold <strong>link</strong></a>"));
        assert!(body("[not a link] (x) and [broken](").contains("[not a link]"));
    }

    #[test]
    fn lists_nest_and_tasks_have_checkboxes() {
        let b = body("- one\n- two\n  - inner a\n  - inner b\n- three\n\n1. first\n2. second\n\n- [x] done\n- [ ] todo\n");
        assert!(b.contains("<ul>\n<li>one</li>\n<li>two<ul>\n<li>inner a</li>\n<li>inner b</li>\n</ul>\n</li>\n<li>three</li>\n</ul>"), "{b}");
        assert!(
            b.contains("<ol>\n<li>first</li>\n<li>second</li>\n</ol>"),
            "{b}"
        );
        assert!(
            b.contains("<input type=\"checkbox\" checked> done")
                && b.contains("<input type=\"checkbox\"> todo"),
            "{b}"
        );
    }

    #[test]
    fn code_quotes_rules_and_tables() {
        let b = body("```rust\nfn main() { <x> }\n```\n\n    indented code\n\n> quoted *text*\n> more\n\n---\n\n| A | B |\n|:--|--:|\n| 1 | 2 |\n");
        assert!(
            b.contains("<pre><code class=\"language-rust\">fn main() { &lt;x&gt; }\n</code></pre>"),
            "{b}"
        );
        assert!(b.contains("<pre><code>indented code</code></pre>"), "{b}");
        assert!(
            b.contains("<blockquote><p>quoted <em>text</em>\nmore</p>\n</blockquote>"),
            "{b}"
        );
        assert!(b.contains("<hr>"));
        assert!(
            b.contains("<th style=\"text-align:left\">A</th><th style=\"text-align:right\">B</th>"),
            "{b}"
        );
        assert!(
            b.contains("<td style=\"text-align:left\">1</td><td style=\"text-align:right\">2</td>"),
            "{b}"
        );
    }

    #[test]
    fn raw_html_and_dangerous_links_never_get_through() {
        let b = body("<script>alert(1)</script> <b onclick=x>hi</b> & <img src=x onerror=y>\n\n[a](javascript:alert(1)) [b](  JaVaScRiPt:x) ![c](data:text/html,x) [d](vbscript:x)");
        assert!(
            !b.contains("<script") && !b.contains("<b onclick") && !b.contains("<img src=x"),
            "{b}"
        );
        assert!(b.contains("&lt;script&gt;") && b.contains("&amp;"), "{b}");
        assert!(
            !b.to_lowercase().contains("javascript:")
                && !b.contains("data:text")
                && !b.contains("vbscript"),
            "{b}"
        );
        assert_eq!(b.matches("href=\"#\"").count(), 3);
        // Quotes in attributes are escaped.
        assert!(body("[x](\"onmouseover=\"y)").contains("&quot;"));
    }

    #[test]
    fn the_result_renders_with_the_engine() {
        use crate::dom::Dom;
        let d = Dom::parse(&to_html("# Hello\n\ntext **bold**\n\n- a\n- b\n"));
        assert_eq!(d.find_all("h1").len(), 1);
        assert_eq!(d.find_all("li").len(), 2);
        assert_eq!(d.find_all("strong").len(), 1);
        assert!(!d.style_sheets().is_empty());
    }

    #[test]
    fn hostile_input_is_bounded_and_never_panics() {
        for s in [
            "",
            "#",
            "# ",
            "```",
            "`",
            "[",
            "[]()",
            "![",
            "**",
            "*",
            "_",
            "~~",
            ">",
            "> >",
            "- ",
            "1. ",
            "|",
            "|-|",
            "\\",
            "<",
            "<>",
            "\u{0}",
            "é**é*é",
            "[\u{1F600}](\u{1F600})",
        ] {
            let _ = to_html(s);
        }
        let deep = format!("{}x", "> ".repeat(500));
        let _ = to_html(&deep);
        let nested = (0..200)
            .map(|i| format!("{}- item", "  ".repeat(i)))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = to_html(&nested);
        let many = "**a** ".repeat(50_000);
        let t = std::time::Instant::now();
        let _ = to_html(&many);
        assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
        let _ = to_html(&"*".repeat(5000));
        let _ = to_html(&"[".repeat(5000));
    }
}
