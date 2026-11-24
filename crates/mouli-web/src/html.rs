//! Tolerant HTML tokenizer.
//!
//! Produces start tags (with attributes), end tags and text. It does not
//! build a tree or run scripts; malformed markup degrades to text.

/// One token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// `<tag a="b">` (tag lowercased; attribute names lowercased).
    Start {
        /// Tag name.
        tag: String,
        /// Attributes in source order.
        attrs: Vec<(String, String)>,
        /// Ends with `/>`.
        self_closing: bool,
    },
    /// `</tag>`.
    End(String),
    /// Text with entities decoded.
    Text(String),
}

impl Token {
    /// Attribute value for a start tag.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        match self {
            Self::Start { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }
}

/// Decode the common named and numeric character references.
#[must_use]
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if start == i {
            break;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = decode_entities(&s[vs..i]);
                i = (i + 1).min(b.len());
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = decode_entities(&s[vs..i]);
            }
        }
        attrs.push((name, value));
    }
    attrs
}

/// Raw-text elements whose content is not markup.
fn raw_text(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "textarea" | "title")
}

/// Tokenize `html`.
#[must_use]
pub fn tokenize(html: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(&mut out, rest);
            break;
        };
        push_text(&mut out, &rest[..lt]);
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |e| &after[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else {
            push_text(&mut out, rest);
            break;
        };
        let inner = &rest[1..gt];
        rest = &rest[gt + 1..];
        if inner.starts_with('!') || inner.starts_with('?') {
            continue;
        }
        if let Some(name) = inner.strip_prefix('/') {
            out.push(Token::End(name.trim().to_ascii_lowercase()));
            continue;
        }
        let name_end = inner
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let tag = inner[..name_end].to_ascii_lowercase();
        if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            push_text(&mut out, &format!("<{inner}>"));
            continue;
        }
        let self_closing = inner.trim_end().ends_with('/');
        out.push(Token::Start {
            tag: tag.clone(),
            attrs: parse_attrs(&inner[name_end..]),
            self_closing,
        });
        if raw_text(&tag) && !self_closing {
            let close = format!("</{tag}");
            let lower = rest.to_ascii_lowercase();
            let end = lower.find(&close).unwrap_or(rest.len());
            let body = &rest[..end];
            if !body.is_empty() {
                let text = if tag == "script" || tag == "style" {
                    body.to_owned()
                } else {
                    decode_entities(body)
                };
                out.push(Token::Text(text));
            }
            rest = &rest[end..];
            if let Some(g) = rest.find('>') {
                out.push(Token::End(tag));
                rest = &rest[g + 1..];
            }
        }
    }
    out
}

fn push_text(out: &mut Vec<Token>, s: &str) {
    if !s.is_empty() {
        out.push(Token::Text(decode_entities(s)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_attrs() {
        let t = tokenize(
            r#"<P class='x' hidden>Fish &amp; chips<br/></p><!-- c --><a href=/go>go</a>"#,
        );
        assert_eq!(t[0].attr("class"), Some("x"));
        assert_eq!(t[0].attr("hidden"), Some(""));
        assert_eq!(t[1], Token::Text("Fish & chips".into()));
        assert!(matches!(&t[2], Token::Start { tag, self_closing: true, .. } if tag == "br"));
        assert_eq!(t[3], Token::End("p".into()));
        assert_eq!(t[4].attr("href"), Some("/go"));
    }

    #[test]
    fn script_is_raw_and_garbage_is_text() {
        let t = tokenize("<script>if (a < b) { x(); }</script>ok < not a tag");
        assert_eq!(t[1], Token::Text("if (a < b) { x(); }".into()));
        assert_eq!(t[2], Token::End("script".into()));
        let last: String = t[3..]
            .iter()
            .map(|x| match x {
                Token::Text(s) => s.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(last, "ok < not a tag");
        assert_eq!(decode_entities("&#65;&#x42;&bogus;"), "AB&bogus;");
    }
}
