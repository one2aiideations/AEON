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

const KEYWORDS: [&str; 11] = [
    "api_key",
    "api-key",
    "apikey",
    "secret",
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "token",
    "private_key",
    "private-key",
];

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_tail(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Characters that can appear in a bearer or basic credential.
fn is_credential(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
}

fn push(out: &mut Vec<Finding>, start: usize, end: usize) {
    if end > start {
        out.push(Finding {
            kind: FindingKind::Secret,
            start,
            end,
        });
    }
}

/// The value after `name = value` or `"name": "value"`, starting at `j` (just after the
/// separator and any spaces). A quoted value is taken whole, spaces included, without its
/// quotes; an unquoted one ends at a space, comma or semicolon.
fn value_span(b: &[u8], mut j: usize) -> (usize, usize) {
    match b.get(j) {
        Some(&q @ (b'"' | b'\'')) => {
            j += 1;
            let start = j;
            while j < b.len() && b[j] != q && b[j] != b'\n' {
                if b[j] == b'\\' && j + 1 < b.len() && b[j + 1] != b'\n' {
                    j += 1;
                }
                j += 1;
            }
            (start, j.min(b.len()))
        }
        _ => {
            let start = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b',' && b[j] != b';' {
                j += 1;
            }
            (start, j)
        }
    }
}

fn skip_spaces(b: &[u8], mut j: usize) -> usize {
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    j
}

/// `keyword`, optionally continued by name characters (`secret_key`, `token_value`), optionally
/// closed by a quote (JSON), then `:` or `=`, then a value.
fn keyword_assignments(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    let lc = text.to_ascii_lowercase();
    for kw in KEYWORDS {
        for (i, _) in lc.match_indices(kw) {
            let mut j = i + kw.len();
            // `secret_key`, `token-value`: a name continued with _ or -, but not `tokens`.
            if matches!(b.get(j), Some(b'_' | b'-')) {
                while j < b.len() && is_tail(b[j]) {
                    j += 1;
                }
            }
            if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                j += 1;
            }
            j = skip_spaces(b, j);
            if j >= b.len() || (b[j] != b':' && b[j] != b'=') {
                continue;
            }
            j = skip_spaces(b, j + 1);
            let (start, end) = value_span(b, j);
            push(out, start, end);
        }
    }
}

/// `Authorization: Bearer <token>` and `Basic <base64>`, and a bare `Bearer <long token>`.
fn authorization(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    let lc = text.to_ascii_lowercase();
    for (i, _) in lc.match_indices("authorization") {
        let j = skip_spaces(b, i + "authorization".len());
        if j >= b.len() || (b[j] != b':' && b[j] != b'=') {
            continue;
        }
        let mut j = skip_spaces(b, j + 1);
        if matches!(b.get(j), Some(b'"' | b'\'')) {
            j += 1;
        }
        // An optional scheme word, then the credential.
        let word_end = (j..b.len())
            .find(|&k| !b[k].is_ascii_alphabetic())
            .unwrap_or(b.len());
        let scheme = lc[j..word_end].to_string();
        if matches!(scheme.as_str(), "bearer" | "basic" | "token") && b.get(word_end) == Some(&b' ')
        {
            j = skip_spaces(b, word_end);
        }
        let end = (j..b.len())
            .find(|&k| !is_credential(b[k]))
            .unwrap_or(b.len());
        push(out, j, end);
    }
    for (i, _) in lc.match_indices("bearer ") {
        if i > 0 && is_alnum(b[i - 1]) {
            continue;
        }
        let start = skip_spaces(b, i + "bearer ".len());
        let end = (start..b.len())
            .find(|&k| !is_credential(b[k]))
            .unwrap_or(b.len());
        if end - start >= 16 {
            push(out, start, end);
        }
    }
}

/// `scheme://user:password@host`: the password.
fn url_passwords(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    for (i, _) in text.match_indices("://") {
        let start = i + 3;
        let end = (start..b.len())
            .find(|&k| b[k].is_ascii_whitespace() || b[k] == b'/' || b[k] == b'"' || b[k] == b'\'')
            .unwrap_or(b.len());
        let authority = &text[start..end];
        if let Some(at) = authority.rfind('@') {
            if let Some(colon) = authority[..at].find(':') {
                push(out, start + colon + 1, start + at);
            }
        }
    }
}

/// `-----BEGIN ... PRIVATE KEY-----` through its END line (or the end of the text when the
/// block is cut off).
fn private_key_blocks(text: &str, out: &mut Vec<Finding>) {
    for (i, _) in text.match_indices("-----BEGIN ") {
        let header_end = text[i + 11..]
            .find("-----")
            .map_or(text.len(), |k| i + 11 + k);
        if !text[i..header_end].contains("PRIVATE KEY") {
            continue;
        }
        let end = match text[header_end..].find("-----END ") {
            Some(k) => {
                let from = header_end + k + 9;
                text[from..]
                    .find("-----")
                    .map_or(text.len(), |e| from + e + 5)
            }
            None => text.len(),
        };
        push(out, i, end);
    }
}

/// A JSON web token: `eyJ...` with two dots.
fn jwts(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    for (i, _) in text.match_indices("eyJ") {
        if i > 0 && is_alnum(b[i - 1]) {
            continue;
        }
        let mut j = i;
        let mut dots = 0;
        while j < b.len() && (is_tail(b[j]) || b[j] == b'.') {
            if b[j] == b'.' {
                dots += 1;
            }
            j += 1;
        }
        if dots >= 2 && j - i >= 30 {
            push(out, i, j);
        }
    }
}

fn secrets(text: &str, out: &mut Vec<Finding>) {
    let b = text.as_bytes();
    keyword_assignments(text, out);
    authorization(text, out);
    url_passwords(text, out);
    private_key_blocks(text, out);
    jwts(text, out);
    // Well-known key formats: (prefix, minimum characters after the prefix, tail charset).
    let formats: [KeyFormat; 14] = [
        ("sk-", 20, is_tail),
        ("ghp_", 30, is_alnum),
        ("gho_", 30, is_alnum),
        ("ghu_", 30, is_alnum),
        ("ghs_", 30, is_alnum),
        ("ghr_", 30, is_alnum),
        ("github_pat_", 30, is_tail),
        ("glpat-", 20, is_tail),
        ("xoxb-", 20, is_tail),
        ("xoxp-", 20, is_tail),
        ("xoxa-", 20, is_tail),
        ("sk_live_", 16, is_alnum),
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
                push(out, i, j);
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

    /// Text containing a secret that must not survive washing, and the part that must be gone.
    const MUST_NOT_LEAK: &[(&str, &str)] = &[
        (r#"{"password": "hunter2xyz"}"#, "hunter2xyz"),
        (r#"{"api_key":"abc123def456"}"#, "abc123def456"),
        (r#"password = "correct horse battery""#, "battery"),
        ("password: 'two words here'", "words"),
        ("secret_key=AbCdEf123456", "AbCdEf123456"),
        ("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "wJalrXUtnFEMI"),
        ("client_secret = zzTopSecret99", "zzTopSecret99"),
        ("pwd=letmein123", "letmein123"),
        ("passphrase: opensesame", "opensesame"),
        ("Authorization: Bearer abcdef1234567890abcdef", "abcdef1234567890abcdef"),
        ("curl -H 'Authorization: Basic dXNlcjpwYXNzd29yZA=='", "dXNlcjpwYXNzd29yZA"),
        ("authorization=Token abc123def456ghi789", "abc123def456ghi789"),
        ("got a Bearer abcdef1234567890abcdef today", "abcdef1234567890abcdef"),
        (concat!("xo", "xb-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx"), "AbCdEfGhIjKlMnOpQrStUvWx"),
        ("ghs_abcdefghijklmnopqrstuvwxyz0123456789", "abcdefghijklmnopqrstuvwxyz0123456789"),
        ("gho_abcdefghijklmnopqrstuvwxyz0123456789", "abcdefghijklmnopqrstuvwxyz0123456789"),
        ("github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz0123456789ABCDEFGHIJKL", "mnopqrstuvwxyz0123456789"),
        ("glpat-abcdefghij0123456789", "abcdefghij0123456789"),
        ("sk_live_abcdefghij0123456789", "abcdefghij0123456789"),
        ("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U", "dozjgNryP4J3jVmNHl0w5N"),
        ("-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----", "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC"),
        ("-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA0Z3VS5JJcds3xfn", "MIIEowIBAAKCAQEA0Z3VS5JJcds3xfn"),
        ("postgres://admin:s3cretPw@db.internal:5432/app", "s3cretPw"),
        ("redis://:onlypass@cache:6379", "onlypass"),
        ("sk-ant-api03-abcdefghijklmnopqrstuvwxyz", "abcdefghijklmnopqrstuvwxyz"),
        ("PASSWORD=Upper123", "Upper123"),
        ("token=a1b2c3d4", "a1b2c3d4"),
        ("my card is 4111 1111 1111 1111.", "4111 1111 1111 1111"),
        ("card 4111-1111-1111-1111", "4111-1111-1111-1111"),
        ("mail me: Jane.Doe+x@example.co.uk please", "Jane.Doe+x@example.co.uk"),
    ];

    #[test]
    fn known_secret_shapes_do_not_survive_washing() {
        let mut leaks = Vec::new();
        for (text, secret) in MUST_NOT_LEAK {
            let w = wash(text, Mode::Redact, &mut Vault::new());
            if w.text.contains(secret) {
                leaks.push(format!("{text:?} -> {:?}", w.text));
            }
        }
        assert!(leaks.is_empty(), "leaked:\n{}", leaks.join("\n"));
    }

    #[test]
    fn ordinary_prose_and_code_are_left_alone() {
        for text in [
            "The token economy is basic and secret santa is fun.",
            "Please reset your password at the help desk.",
            "Contact the authorization team about the bearer of bad news.",
            "let tokens = vec![1, 2, 3]; // tokenise",
            "See http://example.com/path and https://example.org/a@b",
            "Order 12345678 shipped on 2026-11-26 for 19.99",
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
            "eyJ short.no.dots",
        ] {
            let w = wash(text, Mode::Redact, &mut Vault::new());
            assert_eq!(w.text, text, "changed: {text:?}");
        }
    }

    /// A small deterministic random source (no dependencies, same run every time).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn washing_random_text_never_panics_cuts_characters_or_misplaces_spans() {
        let pieces = [
            "password",
            "=",
            ":",
            "\"",
            "'",
            " ",
            "\n",
            "secret_key",
            "Bearer ",
            "Authorization",
            "token",
            "eyJ",
            ".",
            "sk-",
            "ghp_",
            "AKIA",
            "@",
            "example.com",
            "4111111111111111",
            "-----BEGIN PRIVATE KEY-----",
            "-----END PRIVATE KEY-----",
            "é",
            "日本",
            "😀",
            "://",
            "user:pw@host",
            "abcdefghijklmnopqrstuvwxyz0123456789",
            "0123456789",
            "-",
            "_",
            "{",
            "}",
            ",",
            ";",
        ];
        let mut rng = Lcg(0x4d4f554c49);
        for round in 0..5_000 {
            let n = rng.below(24);
            let text: String = (0..n).map(|_| pieces[rng.below(pieces.len())]).collect();
            let findings = scan(&text);
            let mut last_end = 0;
            for f in &findings {
                assert!(
                    f.start >= last_end && f.start < f.end,
                    "round {round}: order {text:?}"
                );
                assert!(
                    text.is_char_boundary(f.start) && text.is_char_boundary(f.end),
                    "round {round}: cut a character in {text:?}"
                );
                last_end = f.end;
            }
            let once = wash(&text, Mode::Redact, &mut Vault::new());
            // The output is exactly the text with the found spans replaced.
            let mut expected = String::new();
            let mut pos = 0;
            for f in &findings {
                expected.push_str(&text[pos..f.start]);
                expected.push_str(&format!("[REDACTED:{}]", f.kind.label()));
                pos = f.end;
            }
            expected.push_str(&text[pos..]);
            assert_eq!(once.text, expected, "round {round}");
        }
    }

    #[test]
    fn a_planted_secret_is_removed_whatever_surrounds_it() {
        let mut rng = Lcg(0xfeed);
        let around = [
            "",
            " ",
            "\n",
            "hello ",
            "日本語 ",
            "😀 ",
            "{",
            "(",
            "- ",
            "note: ",
        ];
        let alnum = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for round in 0..3_000 {
            let value: String = (0..12 + rng.below(12))
                .map(|_| alnum[rng.below(alnum.len())] as char)
                .collect();
            let shape = rng.below(6);
            let secret = match shape {
                0 => format!("password={value}"),
                1 => format!("{{\"token\": \"{value}\"}}"),
                2 => format!("Authorization: Bearer {value}"),
                3 => format!("secret_key: '{value}'"),
                4 => format!("db://u:{value}@h/x"),
                _ => format!("api_key = {value}"),
            };
            let text = format!(
                "{}{secret}{}",
                around[rng.below(around.len())],
                around[rng.below(around.len())]
            );
            let w = wash(&text, Mode::Redact, &mut Vault::new());
            assert!(
                !w.text.contains(&value),
                "round {round}: {text:?} -> {:?}",
                w.text
            );
        }
    }
}
