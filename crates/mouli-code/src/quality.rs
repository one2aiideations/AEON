//! Patch quality policy: statistics, test-file detection, hard-coding heuristics and a risk score.
//!
//! The checks run on the patch AS APPLIED (see [`assess`]), so the syntax check looks at the
//! edited files rather than the originals. The hard-coding and special-casing checks are
//! heuristics that flag patterns worth a human look; they cannot prove a patch is honest.

use crate::apply::{apply_patchset, ApplyError, ApplyOptions};
use crate::diff::{FilePatch, LineKind};
use std::collections::BTreeMap;

/// Rules a patch must satisfy to be accepted without review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchPolicy {
    /// Whether the patch may modify test files.
    pub allow_test_modification: bool,
    /// Most files one patch may touch.
    pub max_files: usize,
    /// Most added plus removed lines.
    pub max_changed_lines: usize,
}

impl Default for PatchPolicy {
    fn default() -> Self {
        Self {
            allow_test_modification: false,
            max_files: 8,
            max_changed_lines: 400,
        }
    }
}

/// Counts describing a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Paths touched.
    pub files: Vec<String>,
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// Hunks in total.
    pub hunks: usize,
    /// Files created.
    pub creations: usize,
    /// Files deleted.
    pub deletions: usize,
    /// True when any touched path is a test file.
    pub touches_tests: bool,
}

/// What a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// A test file is modified and the policy forbids it. Blocking.
    TestFileModified,
    /// More files than the policy allows. Blocking.
    TooManyFiles,
    /// More changed lines than the policy allows. Blocking.
    TooManyLines,
    /// A changed file no longer has balanced delimiters. Blocking.
    SyntaxInvalid,
    /// The patch deletes a file. A warning.
    FileDeleted,
    /// An added string literal mentions "expected". A warning.
    HardcodedExpected,
    /// An added line contains a date or a long hex constant. A warning.
    SuspiciousConstant,
    /// An added line refers to the benchmark, hidden tests or secrets. A warning.
    SpecialCasing,
    /// An added line in a test file returns a literal string. A warning.
    TestOnlyReturn,
    /// The only changes are whitespace. A warning.
    WhitespaceOnly,
}

impl FindingKind {
    /// True when this finding makes the patch unacceptable without review.
    pub fn is_blocking(self) -> bool {
        matches!(
            self,
            Self::TestFileModified | Self::TooManyFiles | Self::TooManyLines | Self::SyntaxInvalid
        )
    }
}

/// One thing the assessment noticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The kind of finding.
    pub kind: FindingKind,
    /// Human-readable detail, naming the file or line.
    pub detail: String,
}

/// The result of assessing a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Counts describing the patch.
    pub stats: Stats,
    /// Everything noticed.
    pub findings: Vec<Finding>,
    /// Risk from 0 (low) to 100 (high).
    pub risk: u32,
    /// True when no blocking finding was raised.
    pub acceptable: bool,
}

/// True when `path` looks like a test file: it lies under a `test`, `tests`, `__tests__` or
/// `spec` directory, or its file name starts with `test_` or ends with `_test`, `.test`,
/// `_spec` or `.spec` before the extension.
pub fn is_test_path(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let file = parts.pop().unwrap_or("");
    if parts.iter().any(|d| {
        matches!(
            d.to_ascii_lowercase().as_str(),
            "test" | "tests" | "__tests__" | "spec" | "specs"
        )
    }) {
        return true;
    }
    let stem = file
        .rsplit_once('.')
        .map_or(file, |(s, _)| s)
        .to_ascii_lowercase();
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with(".test")
        || stem.ends_with("_spec")
        || stem.ends_with(".spec")
}

/// Statistics for a set of patches.
pub fn stats(patches: &[FilePatch]) -> Stats {
    let mut s = Stats {
        files: Vec::new(),
        added: 0,
        removed: 0,
        hunks: 0,
        creations: 0,
        deletions: 0,
        touches_tests: false,
    };
    for p in patches {
        let (a, r) = p.counts();
        s.added += a;
        s.removed += r;
        s.hunks += p.hunks.len();
        s.creations += usize::from(p.is_creation());
        s.deletions += usize::from(p.is_deletion());
        s.touches_tests |= is_test_path(p.path());
        s.files.push(p.path().to_string());
    }
    s
}

fn has_iso_date(line: &str) -> bool {
    let b = line.as_bytes();
    b.windows(10).any(|w| {
        w.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
    })
}

fn has_long_hex(line: &str) -> bool {
    let mut run = 0;
    for c in line.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
            if run >= 32 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// String literals (double or single quoted, with backslash escapes) found in a line.
fn string_literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let q = chars[i];
        if q == '"' || q == '\'' {
            let mut j = i + 1;
            let mut lit = String::new();
            while j < chars.len() && chars[j] != q {
                if chars[j] == '\\' {
                    j += 1;
                }
                if let Some(c) = chars.get(j) {
                    lit.push(*c);
                }
                j += 1;
            }
            if j < chars.len() {
                out.push(lit);
                i = j;
            }
        }
        i += 1;
    }
    out
}

/// Balanced `()`, `[]` and `{}` outside strings and comments. Comment syntax follows the file
/// extension (`#` for py/sh/rb/toml/yaml, `//` and `/* */` otherwise). A rough guard against
/// truncated or mangled edits; it is not a parser.
pub fn brackets_balanced(path: &str, text: &str) -> bool {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    let hash_comments = matches!(ext, "py" | "sh" | "rb" | "toml" | "yaml" | "yml");
    let rust_like = matches!(ext, "rs");
    let c: Vec<char> = text.chars().collect();
    let mut stack: Vec<char> = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '#' if hash_comments => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if !hash_comments && c.get(i + 1) == Some(&'/') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if !hash_comments && c.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                    i += 1;
                }
                i += 1;
            }
            '\'' if rust_like => {
                // A character literal ('x' or '\n') is skipped; a lifetime ('a) is not a quote.
                if c.get(i + 1) == Some(&'\\') {
                    i += 2;
                    while i < c.len() && c[i] != '\'' {
                        i += 1;
                    }
                } else if c.get(i + 2) == Some(&'\'') {
                    i += 2;
                }
            }
            q @ ('"' | '\'') => {
                i += 1;
                while i < c.len() && c[i] != q {
                    if c[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            o @ ('(' | '[' | '{') => stack.push(o),
            cl @ (')' | ']' | '}') => {
                let want = match cl {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if stack.pop() != Some(want) {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    stack.is_empty()
}

fn strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Assesses `patches` against `files` under `policy`.
///
/// The patch set is first APPLIED to a copy of `files` (exactly, with no fuzz), so a patch that
/// does not fit is an error, not a risk score. `syntax_ok(path, content)` decides whether each
/// changed file is still well formed; pass [`brackets_balanced`] for a language-agnostic check.
pub fn assess(
    files: &BTreeMap<String, String>,
    patches: &[FilePatch],
    policy: &PatchPolicy,
    syntax_ok: &dyn Fn(&str, &str) -> bool,
) -> Result<Report, ApplyError> {
    let edited = apply_patchset(files, patches, ApplyOptions::default())?;
    let st = stats(patches);
    let mut findings: Vec<Finding> = Vec::new();
    let mut push = |kind, detail: String| findings.push(Finding { kind, detail });

    for p in patches {
        let path = p.path();
        if is_test_path(path) && !policy.allow_test_modification {
            push(
                FindingKind::TestFileModified,
                format!("{path} is a test file"),
            );
        }
        if p.is_deletion() {
            push(FindingKind::FileDeleted, format!("{path} is deleted"));
        } else if let Some(content) = edited.get(path) {
            if !syntax_ok(path, content) {
                push(
                    FindingKind::SyntaxInvalid,
                    format!("{path} has unbalanced delimiters after the change"),
                );
            }
        }
        // Heuristics look only at lines the patch ADDS.
        for h in &p.hunks {
            for l in h.lines.iter().filter(|l| l.kind == LineKind::Add) {
                let t = l.text.as_str();
                if string_literals(t)
                    .iter()
                    .any(|s| s.to_ascii_lowercase().contains("expected"))
                {
                    push(
                        FindingKind::HardcodedExpected,
                        format!("{path}: {}", t.trim()),
                    );
                }
                if has_iso_date(t) || has_long_hex(t) {
                    push(
                        FindingKind::SuspiciousConstant,
                        format!("{path}: {}", t.trim()),
                    );
                }
                let lower = t.to_ascii_lowercase();
                if [
                    "benchmark",
                    "hidden test",
                    "hidden_test",
                    "competition",
                    "secret",
                ]
                .iter()
                .any(|w| lower.contains(w))
                {
                    push(FindingKind::SpecialCasing, format!("{path}: {}", t.trim()));
                }
                if is_test_path(path)
                    && t.trim_start().starts_with("return ")
                    && !string_literals(t).is_empty()
                {
                    push(FindingKind::TestOnlyReturn, format!("{path}: {}", t.trim()));
                }
            }
        }
    }
    let removed_all: String = patches
        .iter()
        .flat_map(|p| &p.hunks)
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Remove)
        .map(|l| strip_ws(&l.text))
        .collect();
    let added_all: String = patches
        .iter()
        .flat_map(|p| &p.hunks)
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Add)
        .map(|l| strip_ws(&l.text))
        .collect();
    if st.added + st.removed > 0 && removed_all == added_all {
        push(
            FindingKind::WhitespaceOnly,
            "the patch only changes whitespace".to_string(),
        );
    }
    if st.files.len() > policy.max_files {
        push(
            FindingKind::TooManyFiles,
            format!("{} files (limit {})", st.files.len(), policy.max_files),
        );
    }
    let changed = st.added + st.removed;
    if changed > policy.max_changed_lines {
        push(
            FindingKind::TooManyLines,
            format!(
                "{changed} changed lines (limit {})",
                policy.max_changed_lines
            ),
        );
    }

    // Risk: touching tests dominates, size adds up to 30 (2.5 per changed line), an invalid
    // file maxes it out, and each heuristic warning adds 10.
    let mut risk: u32 = 0;
    if findings
        .iter()
        .any(|f| f.kind == FindingKind::TestFileModified)
    {
        risk += 60;
    }
    risk += ((changed as u32).saturating_mul(5) / 2).min(30);
    if findings
        .iter()
        .any(|f| f.kind == FindingKind::SyntaxInvalid)
    {
        risk += 100;
    }
    risk += 10
        * findings
            .iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    FindingKind::HardcodedExpected
                        | FindingKind::SuspiciousConstant
                        | FindingKind::SpecialCasing
                        | FindingKind::TestOnlyReturn
                )
            })
            .count() as u32;
    let acceptable = !findings.iter().any(|f| f.kind.is_blocking());
    Ok(Report {
        stats: st,
        findings,
        risk: risk.min(100),
        acceptable,
    })
}
