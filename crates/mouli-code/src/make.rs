//! Generating a unified diff from two versions of a file.

use crate::diff::{FilePatch, Hunk, Line, LineKind};
use std::fmt;

/// Largest middle region (old lines times new lines, after trimming common ends) that is diffed.
pub const MAX_DIFF_CELLS: usize = 4_000_000;

/// Why a diff could not be generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MakeError {
    /// The changed region is too large to diff exactly.
    TooLarge,
    /// Creating or deleting an empty file cannot be expressed as hunks.
    EmptyFile,
}

impl fmt::Display for MakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MakeError::TooLarge => write!(f, "the changed region is too large to diff"),
            MakeError::EmptyFile => {
                write!(f, "an empty file cannot be created or deleted by a diff")
            }
        }
    }
}

impl std::error::Error for MakeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq(usize, usize),
    Del(usize),
    Add(usize),
}

/// Lines of `text` with a flag per line: true when it is the last line and has no newline.
fn lines_of(text: &str) -> Vec<(&str, bool)> {
    if text.is_empty() {
        return Vec::new();
    }
    let ends = text.ends_with('\n');
    let body = if ends { &text[..text.len() - 1] } else { text };
    let parts: Vec<&str> = body.split('\n').collect();
    let last = parts.len() - 1;
    parts
        .into_iter()
        .enumerate()
        .map(|(i, l)| (l, i == last && !ends))
        .collect()
}

fn edit_script(old: &[(&str, bool)], new: &[(&str, bool)]) -> Result<Vec<Op>, MakeError> {
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old.get(prefix) == new.get(prefix) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old.get(old.len() - 1 - suffix) == new.get(new.len() - 1 - suffix)
    {
        suffix += 1;
    }
    let (om, nm) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );
    let (n, m) = (om.len(), nm.len());
    if n.saturating_mul(m) > MAX_DIFF_CELLS {
        return Err(MakeError::TooLarge);
    }
    // dp[i][j] = length of the longest common subsequence of om[i..] and nm[j..].
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if om[i] == nm[j] {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let mut ops: Vec<Op> = (0..prefix).map(|k| Op::Eq(k, k)).collect();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && om[i] == nm[j] {
            ops.push(Op::Eq(prefix + i, prefix + j));
            i += 1;
            j += 1;
        } else if i < n && (j == m || dp[(i + 1) * w + j] >= dp[i * w + j + 1]) {
            // On ties, removals come before additions, as in git's output.
            ops.push(Op::Del(prefix + i));
            i += 1;
        } else {
            ops.push(Op::Add(prefix + j));
            j += 1;
        }
    }
    for k in 0..suffix {
        ops.push(Op::Eq(old.len() - suffix + k, new.len() - suffix + k));
    }
    Ok(ops)
}

/// Builds the patch that turns `old` into `new` for `path`, with `context` lines around each
/// change. `None` for `old` means the file is created; `None` for `new` means it is deleted.
/// Returns `Ok(None)` when nothing changes.
pub fn make_patch(
    path: &str,
    old: Option<&str>,
    new: Option<&str>,
    context: usize,
) -> Result<Option<FilePatch>, MakeError> {
    let old_lines = lines_of(old.unwrap_or(""));
    let new_lines = lines_of(new.unwrap_or(""));
    if (old.is_none() && new_lines.is_empty()) || (new.is_none() && old_lines.is_empty()) {
        return Err(MakeError::EmptyFile);
    }
    if old.is_some() && new.is_some() && old == new {
        return Ok(None);
    }
    let ops = edit_script(&old_lines, &new_lines)?;
    // Groups of operations that belong in one hunk: changes separated by at most 2*context
    // unchanged lines are merged.
    let change_idx: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Eq(..)))
        .map(|(i, _)| i)
        .collect();
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &ci in &change_idx {
        match groups.last_mut() {
            Some((_, end)) if ci - *end <= 2 * context + 1 => *end = ci,
            _ => groups.push((ci, ci)),
        }
    }
    let mut hunks = Vec::new();
    for (first, last) in groups {
        let start = first.saturating_sub(context);
        let end = (last + context).min(ops.len() - 1);
        let (mut old_before, mut new_before) = (0usize, 0usize);
        for op in &ops[..start] {
            match op {
                Op::Eq(..) => {
                    old_before += 1;
                    new_before += 1;
                }
                Op::Del(_) => old_before += 1,
                Op::Add(_) => new_before += 1,
            }
        }
        let mut lines = Vec::new();
        let (mut old_len, mut new_len) = (0usize, 0usize);
        for op in &ops[start..=end] {
            match *op {
                Op::Eq(oi, _) => {
                    let (t, no_eol) = old_lines[oi];
                    lines.push(Line {
                        kind: LineKind::Context,
                        text: t.to_string(),
                        no_eol,
                    });
                    old_len += 1;
                    new_len += 1;
                }
                Op::Del(oi) => {
                    let (t, no_eol) = old_lines[oi];
                    lines.push(Line {
                        kind: LineKind::Remove,
                        text: t.to_string(),
                        no_eol,
                    });
                    old_len += 1;
                }
                Op::Add(ni) => {
                    let (t, no_eol) = new_lines[ni];
                    lines.push(Line {
                        kind: LineKind::Add,
                        text: t.to_string(),
                        no_eol,
                    });
                    new_len += 1;
                }
            }
        }
        hunks.push(Hunk {
            old_start: if old_len > 0 {
                old_before + 1
            } else {
                old_before
            },
            old_len,
            new_start: if new_len > 0 {
                new_before + 1
            } else {
                new_before
            },
            new_len,
            lines,
        });
    }
    Ok(Some(FilePatch {
        old_path: old.map(|_| path.to_string()),
        new_path: new.map(|_| path.to_string()),
        hunks,
    }))
}
