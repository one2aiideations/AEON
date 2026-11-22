//! Readable page text.

use crate::html::{tokenize, Token};

/// Title and main text of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readable {
    /// `<title>` text.
    pub title: String,
    /// Main text, one block per line.
    pub text: String,
}

fn skipped(tag: &str) -> bool {
    matches!(
        tag,
        "script"
            | "style"
            | "noscript"
            | "nav"
            | "header"
            | "footer"
            | "aside"
            | "form"
            | "svg"
            | "template"
            | "iframe"
    )
}

fn block(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "br"
            | "li"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "tr"
            | "section"
            | "article"
            | "main"
            | "pre"
            | "blockquote"
    )
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract(tokens: &[Token]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut skip_depth = 0usize;
    for t in tokens {
        match t {
            Token::Start {
                tag, self_closing, ..
            } => {
                if skipped(tag) && !self_closing {
                    skip_depth += 1;
                } else if block(tag) && skip_depth == 0 {
                    let line = collapse(&cur);
                    if !line.is_empty() {
                        lines.push(line);
                    }
                    cur.clear();
                }
            }
            Token::End(tag) => {
                if skipped(tag) {
                    skip_depth = skip_depth.saturating_sub(1);
                } else if block(tag) && skip_depth == 0 {
                    let line = collapse(&cur);
                    if !line.is_empty() {
                        lines.push(line);
                    }
                    cur.clear();
                }
            }
            Token::Text(s) if skip_depth == 0 => {
                cur.push(' ');
                cur.push_str(s);
            }
            Token::Text(_) => {}
        }
    }
    let line = collapse(&cur);
    if !line.is_empty() {
        lines.push(line);
    }
    lines.join("\n")
}

/// Slice of tokens inside the first `<main>` or `<article>`, if any.
fn main_region(tokens: &[Token]) -> Option<&[Token]> {
    for want in ["main", "article"] {
        let Some(start) = tokens
            .iter()
            .position(|t| matches!(t, Token::Start { tag, .. } if tag == want))
        else {
            continue;
        };
        let mut depth = 0usize;
        for (i, t) in tokens.iter().enumerate().skip(start) {
            match t {
                Token::Start { tag, .. } if tag == want => depth += 1,
                Token::End(tag) if tag == want => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&tokens[start..=i]);
                    }
                }
                _ => {}
            }
        }
        return Some(&tokens[start..]);
    }
    None
}

/// Extract the title and readable main text of `html`.
#[must_use]
pub fn readable(html: &str) -> Readable {
    let tokens = tokenize(html);
    let title = tokens
        .windows(2)
        .find_map(|w| match (&w[0], &w[1]) {
            (Token::Start { tag, .. }, Token::Text(s)) if tag == "title" => Some(collapse(s)),
            _ => None,
        })
        .unwrap_or_default();
    let body: Vec<Token> = tokens
        .iter()
        .skip_while(|t| !matches!(t, Token::End(tag) if tag == "head"))
        .cloned()
        .collect();
    let scope: &[Token] = if body.is_empty() { &tokens } else { &body };
    let text = match main_region(scope) {
        Some(m) => extract(m),
        None => extract(scope),
    };
    Readable { title, text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_main_drops_chrome() {
        let html = "<html><head><title> AEON  docs </title><style>p{}</style></head><body>\
            <nav>Home | About</nav><main><h1>Install</h1><p>Run   the <b>installer</b>.</p>\
            <script>track()</script><p>Reboot.</p></main><footer>(c)</footer></body></html>";
        let r = readable(html);
        assert_eq!(r.title, "AEON docs");
        assert_eq!(r.text, "Install\nRun the installer .\nReboot.");
    }

    #[test]
    fn falls_back_to_body() {
        let r = readable("<body><div>One</div><aside>ad</aside><div>Two</div></body>");
        assert_eq!(r.text, "One\nTwo");
        assert_eq!(r.title, "");
    }
}
