//! Data washing: find secrets and personal data in text before it can reach a model that is
//! not under local control, and redact or pseudonymise it.
//!
//! Detection is pattern based and deliberately conservative about what it keeps: it can miss
//! data it has no pattern for (names, addresses, free-text secrets). It is a safety net, not a
//! data-loss-prevention system.

use crate::error::{Error, Result};
use std::collections::BTreeMap;

/// What was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    /// Credential: `password=...` style assignment or a known key format.
    Secret,
    /// Email address.
    Email,
    /// Payment card number (13 to 19 digits passing the Luhn check).
    Card,
}

impl FindingKind {
    fn label(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Email => "email",
            Self::Card => "card",
        }
    }
}

/// A span of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finding {
    /// Kind of data.
    pub kind: FindingKind,
    /// Start byte offset.
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

/// Known key format: prefix, minimum characters after it, and the allowed tail characters.
type KeyFormat = (&'static str, usize, fn(u8) -> bool);

const KEYWORDS: [&str; 7] = [
    "api_key", "api-key", "apikey", "secret", "password", "passwd", "token",
];

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_tail(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn secrets(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    let lc = text.to_ascii_lowercase();
    for kw in KEYWORDS {
        for (i, _) in lc.match_indices(kw) {
            let mut j = i + kw.len();
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if j >= b.len() || (b[j] != b':' && b[j] != b'=') {
                continue;
            }
            j += 1;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let start = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b',' && b[j] != b';' {
                j += 1;
            }
            if j > start {
                out.push(Finding {
                    kind: FindingKind::Secret,
                    start,
                    end: j,
                });
            }
        }
    }
    // Well-known key formats: (prefix, minimum characters after the prefix, tail charset).
    let formats: [KeyFormat; 4] = [
        ("sk-", 20, is_tail),
        ("ghp_", 30, is_alnum),
        ("AKIA", 16, |c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        ("AIza", 30, is_tail),
    ];
    for (prefix, min_tail, ok) in formats {
        for (i, _) in text.match_indices(prefix) {
            if i > 0 && is_alnum(b[i - 1]) {
                continue;
            }
            let mut j = i + prefix.len();
            while j < b.len() && ok(b[j]) {
                j += 1;
            }
            if j - i - prefix.len() >= min_tail {
                out.push(Finding {
                    kind: FindingKind::Secret,
                    start: i,
                    end: j,
                });
            }
        }
    }
}

fn emails(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    for (at, _) in text.match_indices('@') {
        let mut s = at;
        while s > 0 && (is_alnum(b[s - 1]) || matches!(b[s - 1], b'.' | b'_' | b'%' | b'+' | b'-'))
        {
            s -= 1;
        }
        let mut e = at + 1;
        while e < b.len() && (is_alnum(b[e]) || b[e] == b'.' || b[e] == b'-') {
            e += 1;
        }
        while e > at + 1 && matches!(b[e - 1], b'.' | b'-') {
            e -= 1;
        }
        let domain = &text[at + 1..e];
        let tld_ok = domain
            .rsplit_once('.')
            .is_some_and(|(_, t)| t.len() >= 2 && t.bytes().all(|c| c.is_ascii_alphabetic()));
        if s < at && tld_ok && domain.split('.').all(|l| !l.is_empty()) {
            out.push(Finding {
                kind: FindingKind::Email,
                start: s,
                end: e,
            });
        }
    }
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let d = u32::from(*d - b'0');
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

fn cards(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let (mut j, mut end, mut digits) = (i, i, Vec::new());
        loop {
            if j < b.len() && b[j].is_ascii_digit() {
                digits.push(b[j]);
                j += 1;
                end = j;
            } else if j + 1 < b.len() && (b[j] == b' ' || b[j] == b'-') && b[j + 1].is_ascii_digit()
            {
                j += 1;
            } else {
                break;
            }
        }
        if (13..=19).contains(&digits.len()) && luhn(&digits) {
            out.push(Finding {
                kind: FindingKind::Card,
                start: i,
                end,
            });
        }
        i = j.max(i + 1);
    }
}

/// Find sensitive spans, sorted by position and without overlaps.
#[must_use]
pub fn scan(text: &str) -> Vec<Finding> {
    let mut all = Vec::new();
    secrets(text, &mut all);
    emails(text, &mut all);
    cards(text, &mut all);
    all.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out: Vec<Finding> = Vec::new();
    for f in all {
        if out.last().is_none_or(|l| f.start >= l.end) {
            out.push(f);
        }
    }
    out
}

/// Whether anything sensitive was found.
#[must_use]
pub fn is_sensitive(text: &str) -> bool {
    !scan(text).is_empty()
}

/// How to replace findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Replace with `[REDACTED:kind]`; nothing is kept.
    Redact,
    /// Replace emails with a stable token the vault can restore; secrets and cards are still
    /// redacted irreversibly.
    Tokenize,
}

/// Result of washing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Washed {
    /// Text safe to send.
    pub text: String,
    /// What was replaced, in order.
    pub findings: Vec<Finding>,
}

/// Holds the originals of tokenised values.
#[derive(Debug, Default)]
pub struct Vault {
    by_token: BTreeMap<String, String>,
    by_value: BTreeMap<String, String>,
}

impl Vault {
    /// Empty vault.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn token_for(&mut self, kind: FindingKind, value: &str) -> String {
        if let Some(t) = self.by_value.get(value) {
            return t.clone();
        }
        let t = format!("[TOKEN:{}:{}]", kind.label(), self.by_token.len() + 1);
        self.by_token.insert(t.clone(), value.to_string());
        self.by_value.insert(value.to_string(), t.clone());
        t
    }

    /// Put the original values back.
    ///
    /// # Errors
    /// [`Error::Denied`] unless `authorised` is true: restoring is a privileged operation.
    pub fn restore(&self, text: &str, authorised: bool) -> Result<String> {
        if !authorised {
            return Err(Error::Denied(
                "restoring washed values needs explicit authorisation".into(),
            ));
        }
        let mut out = text.to_string();
        for (t, v) in &self.by_token {
            out = out.replace(t, v);
        }
        Ok(out)
    }
}

/// Wash `text`.
pub fn wash(text: &str, mode: Mode, vault: &mut Vault) -> Washed {
    let findings = scan(text);
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for f in &findings {
        out.push_str(&text[pos..f.start]);
        if mode == Mode::Tokenize && f.kind == FindingKind::Email {
            out.push_str(&vault.token_for(f.kind, &text[f.start..f.end]));
        } else {
            out.push_str(&format!("[REDACTED:{}]", f.kind.label()));
        }
        pos = f.end;
    }
    out.push_str(&text[pos..]);
    Washed {
        text: out,
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(t: &str) -> Vec<FindingKind> {
        scan(t).iter().map(|f| f.kind).collect()
    }

    #[test]
    fn secret_assignments() {
        let t = "use API_KEY = abc123, and password:hunter2; done";
        let w = wash(t, Mode::Redact, &mut Vault::new());
        assert_eq!(
            w.text,
            "use API_KEY = [REDACTED:secret], and password:[REDACTED:secret]; done"
        );
        assert_eq!(w.findings.len(), 2);
        assert!(scan("the tokenizer is fast").is_empty());
        assert!(scan("password=").is_empty());
    }

    #[test]
    fn key_formats() {
        assert_eq!(
            kinds("key sk-abcdefghijklmnopqrstuv end"),
            [FindingKind::Secret]
        );
        assert_eq!(kinds("AKIAABCDEFGHIJKLMNOP"), [FindingKind::Secret]);
        assert!(scan("sk-short").is_empty());
        assert!(scan("task-abcdefghijklmnopqrstuvwxyz").is_empty());
        let gh = format!("ghp_{}", "a1".repeat(18));
        assert_eq!(kinds(&gh), [FindingKind::Secret]);
    }

    #[test]
    fn emails_and_cards() {
        assert_eq!(
            kinds("mail ann.lee+x@example.co.uk now"),
            [FindingKind::Email]
        );
        assert!(
            scan("a@b").is_empty()
                && scan("@example.com").is_empty()
                && scan("a@b..com").is_empty()
        );
        assert_eq!(kinds("card 4111 1111 1111 1111 ok"), [FindingKind::Card]);
        assert_eq!(kinds("4111-1111-1111-1111"), [FindingKind::Card]);
        assert!(scan("4111 1111 1111 1112").is_empty(), "fails Luhn");
        assert!(scan("order 1234567").is_empty());
    }

    #[test]
    fn tokenize_is_stable_and_restorable_only_when_authorised() {
        let mut v = Vault::new();
        let w = wash(
            "ann@x.io wrote to bob@y.io and ann@x.io, pw: 4111111111111111",
            Mode::Tokenize,
            &mut v,
        );
        assert_eq!(
            w.text,
            "[TOKEN:email:1] wrote to [TOKEN:email:2] and [TOKEN:email:1], pw: [REDACTED:card]"
        );
        assert!(v.restore(&w.text, false).is_err());
        let back = v.restore(&w.text, true).unwrap();
        assert!(back.starts_with("ann@x.io wrote to bob@y.io"));
        assert!(
            back.contains("[REDACTED:card]"),
            "cards are never restorable"
        );
    }

    #[test]
    fn overlapping_findings_keep_the_first() {
        let t = "password=ann@x.io";
        let f = scan(t);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::Secret);
    }

    #[test]
    fn non_ascii_text_is_safe() {
        let t = "café ü password=ñandú ok ann@x.io ✓";
        let w = wash(t, Mode::Redact, &mut Vault::new());
        assert!(w.text.contains("[REDACTED:secret]") && w.text.contains("[REDACTED:email]"));
        assert!(w.text.starts_with("café ü"));
    }
}
