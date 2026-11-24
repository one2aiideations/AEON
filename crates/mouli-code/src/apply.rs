//! Exact, in-memory application of parsed patches.
//!
//! Every context and removed line is compared with the file before anything changes, and a
//! multi-file patch set is applied all-or-nothing to a copy, so a bad hunk can never leave a
//! repository half-patched.

use crate::diff::{FilePatch, Hunk, LineKind};
use std::collections::BTreeMap;
use std::fmt;

/// How strictly hunks must sit where their headers say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ApplyOptions {
    /// How many lines a hunk may be found away from its stated position (0 = exactly there).
    /// The offset found for one hunk carries over to the next, as in `patch`.
    pub max_offset: usize,
}

/// Why a patch could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// The patch names a path that is not a safe relative path.
    UnsafePath(String),
    /// The patch needs a file that does not exist.
    MissingFile(String),
    /// The patch creates a file that already exists.
    AlreadyExists(String),
    /// A hunk's old lines were not found where expected.
    HunkMismatch {
        /// Path of the file.
        path: String,
        /// 1-based hunk number within the file.
        hunk: usize,
        /// 1-based old-file line where the first old line was expected.
        expected_line: usize,
        /// What the file has at that place (first differing line), if any.
        found: Option<String>,
        /// What the hunk wanted there.
        wanted: Option<String>,
    },
    /// Hunks overlap or are out of order.
    HunksOutOfOrder {
        /// Path of the file.
        path: String,
        /// 1-based hunk number.
        hunk: usize,
    },
    /// A deletion patch did not remove every line of the file.
    IncompleteDeletion(String),
    /// Two patches in one set name the same file.
    DuplicatePath(String),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApplyError::UnsafePath(p) => write!(f, "unsafe path '{p}'"),
            ApplyError::MissingFile(p) => write!(f, "file '{p}' does not exist"),
            ApplyError::AlreadyExists(p) => write!(f, "file '{p}' already exists"),
            ApplyError::HunkMismatch { path, hunk, expected_line, found, wanted } => write!(
                f,
                "{path}: hunk {hunk} does not match at line {expected_line} (file has {found:?}, patch wants {wanted:?})"
            ),
            ApplyError::HunksOutOfOrder { path, hunk } => write!(f, "{path}: hunk {hunk} overlaps or precedes the previous one"),
            ApplyError::IncompleteDeletion(p) => write!(f, "{p}: deletion patch leaves lines behind"),
            ApplyError::DuplicatePath(p) => write!(f, "'{p}' appears in more than one patch"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// Checks that `path` is a plain relative path inside the repository: not empty, not absolute,
/// no `..`, no backslash, no NUL or control characters, no `.git` component, bounded length.
pub fn safe_relative_path(path: &str) -> Result<&str, ApplyError> {
    let bad = || Err(ApplyError::UnsafePath(path.to_string()));
    if path.is_empty()
        || path.len() > 1024
        || path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return bad();
    }
    // Windows drive letters such as `C:`.
    if path.as_bytes().get(1) == Some(&b':') {
        return bad();
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git") {
            return bad();
        }
    }
    Ok(path)
}

/// A file split into lines, remembering whether its last line ends with a newline.
fn split_lines(text: &str) -> (Vec<&str>, bool) {
    if text.is_empty() {
        return (Vec::new(), true);
    }
    let ends = text.ends_with('\n');
    let body = if ends { &text[..text.len() - 1] } else { text };
    (body.split('\n').collect(), ends)
}

/// Does hunk `h` match `orig` when its first old line sits at 0-based index `at`?
fn matches_at(h: &Hunk, orig: &[&str], orig_ends_nl: bool, at: usize) -> bool {
    for (idx, l) in (at..).zip(h.lines.iter().filter(|l| l.kind != LineKind::Add)) {
        let Some(have) = orig.get(idx) else {
            return false;
        };
        if *have != l.text {
            return false;
        }
        let is_last = idx + 1 == orig.len();
        // A missing final newline must be mentioned by the patch, and only there.
        if l.no_eol != (is_last && !orig_ends_nl) {
            return false;
        }
    }
    true
}

fn first_old_line(h: &Hunk) -> Option<&str> {
    h.lines
        .iter()
        .find(|l| l.kind != LineKind::Add)
        .map(|l| l.text.as_str())
}

/// Applies one file's hunks to `original` (`None` when the file does not exist yet).
/// Returns the new content, or `None` when the patch deletes the file.
pub fn apply_file(
    original: Option<&str>,
    patch: &FilePatch,
    opts: ApplyOptions,
) -> Result<Option<String>, ApplyError> {
    let path = patch.path().to_string();
    if patch.is_creation() && original.is_some() {
        return Err(ApplyError::AlreadyExists(path));
    }
    if !patch.is_creation() && original.is_none() {
        return Err(ApplyError::MissingFile(path));
    }
    let text = original.unwrap_or("");
    let (orig, orig_nl) = split_lines(text);
    let mut out: Vec<&str> = Vec::new();
    let mut consumed = 0usize;
    let mut shift: isize = 0;
    let mut final_nl = orig_nl;
    for (n, h) in patch.hunks.iter().enumerate() {
        let hunk_no = n + 1;
        // 0-based index of the first old line (for an empty old range, where text is inserted).
        let nominal = if h.old_len == 0 {
            h.old_start
        } else {
            h.old_start.saturating_sub(1)
        };
        let want = (nominal as isize + shift).max(0) as usize;
        // Try the stated position first, then widen the search one line at a time.
        let mut found = None;
        for d in 0..=opts.max_offset {
            for cand in [Some(want + d), want.checked_sub(d)].into_iter().flatten() {
                if cand + h.old_len <= orig.len()
                    && cand >= consumed
                    && matches_at(h, &orig, orig_nl, cand)
                {
                    found = Some(cand);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some(at) = found else {
            // Distinguish "overlaps an earlier hunk" from "context differs".
            let wanted = first_old_line(h).map(str::to_string);
            let found_line = orig.get(want).map(|s| (*s).to_string());
            if want < consumed {
                return Err(ApplyError::HunksOutOfOrder {
                    path,
                    hunk: hunk_no,
                });
            }
            return Err(ApplyError::HunkMismatch {
                path,
                hunk: hunk_no,
                expected_line: want + 1,
                found: found_line,
                wanted,
            });
        };
        shift = at as isize - nominal as isize;
        out.extend_from_slice(orig.get(consumed..at).unwrap_or(&[]));
        let mut last_new_no_eol = None;
        for l in h.lines.iter().filter(|l| l.kind != LineKind::Remove) {
            out.push(l.text.as_str());
            last_new_no_eol = Some(l.no_eol);
        }
        consumed = at + h.old_len;
        if consumed == orig.len() {
            // The hunk reaches the end of the old file, so it decides the new end-of-file state.
            final_nl = match last_new_no_eol {
                Some(no_eol) => !no_eol,
                None => true,
            };
        }
    }
    out.extend_from_slice(orig.get(consumed..).unwrap_or(&[]));
    if patch.is_deletion() {
        return if out.is_empty() {
            Ok(None)
        } else {
            Err(ApplyError::IncompleteDeletion(path))
        };
    }
    if out.is_empty() {
        return Ok(Some(String::new()));
    }
    let mut result = out.join("\n");
    if final_nl {
        result.push('\n');
    }
    Ok(Some(result))
}

/// Applies a whole patch set to a snapshot of files, all-or-nothing.
///
/// `files` maps repository-relative paths to their text and is not modified; the updated map is
/// returned. Paths are checked with [`safe_relative_path`], and a path may appear in only one
/// patch.
pub fn apply_patchset(
    files: &BTreeMap<String, String>,
    patches: &[FilePatch],
    opts: ApplyOptions,
) -> Result<BTreeMap<String, String>, ApplyError> {
    let mut result = files.clone();
    let mut seen = std::collections::BTreeSet::new();
    for p in patches {
        let path = safe_relative_path(p.path())?.to_string();
        if !seen.insert(path.clone()) {
            return Err(ApplyError::DuplicatePath(path));
        }
        match apply_file(files.get(&path).map(String::as_str), p, opts)? {
            Some(content) => {
                result.insert(path, content);
            }
            None => {
                result.remove(&path);
            }
        }
    }
    Ok(result)
}
