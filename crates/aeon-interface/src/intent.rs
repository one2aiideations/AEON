//! Normalise a line of input into an [`Intent`]: a slash command or plain chat.

use crate::error::{Error, Result};
use std::collections::BTreeMap;

/// Name used for ordinary conversation.
pub const CHAT: &str = "chat";

/// A normalised request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// `chat` or a capability name such as `mail.send`.
    pub name: String,
    /// Arguments. Chat carries the full text under `text`.
    pub params: BTreeMap<String, String>,
}

impl Intent {
    /// Parse `/capability key=value "key two"="a value"` or plain text.
    ///
    /// # Errors
    /// Returns [`Error::Invalid`] for an empty line, an empty command name, an argument without
    /// `=`, or an unterminated quote.
    pub fn parse(line: &str) -> Result<Self> {
        let line = line.trim();
        if line.is_empty() {
            return Err(Error::Invalid("empty input".into()));
        }
        let Some(rest) = line.strip_prefix('/') else {
            let mut params = BTreeMap::new();
            params.insert("text".to_string(), line.to_string());
            return Ok(Self {
                name: CHAT.into(),
                params,
            });
        };
        let words = split_words(rest)?;
        let mut it = words.into_iter();
        let name = it
            .next()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| Error::Invalid("empty command".into()))?;
        let mut params = BTreeMap::new();
        for w in it {
            let (k, v) = w
                .split_once('=')
                .ok_or_else(|| Error::Invalid(format!("argument without '=': {w}")))?;
            params.insert(k.to_string(), v.to_string());
        }
        Ok(Self { name, params })
    }
}

fn split_words(s: &str) -> Result<Vec<String>> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quoted {
        return Err(Error::Invalid("unterminated quote".into()));
    }
    if any {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_chat() {
        let i = Intent::parse("  what is on my calendar? ").unwrap();
        assert_eq!(i.name, CHAT);
        assert_eq!(i.params["text"], "what is on my calendar?");
    }

    #[test]
    fn commands_with_quotes() {
        let i = Intent::parse("/mail.send to=a@b.co subject=\"hello there\"").unwrap();
        assert_eq!(i.name, "mail.send");
        assert_eq!(i.params["subject"], "hello there");
        assert_eq!(i.params["to"], "a@b.co");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Intent::parse("").is_err());
        assert!(Intent::parse("/").is_err());
        assert!(Intent::parse("/x bare").is_err());
        assert!(Intent::parse("/x a=\"open").is_err());
        assert_eq!(Intent::parse("/x a=\"\"").unwrap().params["a"], "");
    }
}
