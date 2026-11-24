//! Unified-diff data types, parsing, rendering and reversal.

use std::fmt;

/// Largest diff text [`parse`] accepts, in bytes.
pub const MAX_DIFF_BYTES: usize = 4 * 1024 * 1024;

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// What the line does.
    pub kind: LineKind,
    /// The line's text, without its line terminator.
    pub text: String,
    /// True when the line is followed by `\ No newline at end of file`.
    pub no_eol: bool,
}

/// What a hunk line does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Unchanged line, present in both versions.
    Context,
    /// Line added by the patch.
    Add,
    /// Line removed by the patch.
    Remove,
}

/// A contiguous change with its surrounding context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// First old-file line the hunk covers (1-based; for an empty old range, the line before).
    pub old_start: usize,
    /// Number of old-file lines (context plus removed).
    pub old_len: usize,
    /// First new-file line the hunk covers (1-based; for an empty new range, the line before).
    pub new_start: usize,
    /// Number of new-file lines (context plus added).
    pub new_len: usize,
    /// The hunk's lines in order.
    pub lines: Vec<Line>,
}

/// All hunks for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// Path before the change; `None` when the file is created.
    pub old_path: Option<String>,
    /// Path after the change; `None` when the file is deleted.
    pub new_path: Option<String>,
    /// The hunks, in file order.
    pub hunks: Vec<Hunk>,
}

impl FilePatch {
    /// The path this patch concerns (the new path, or the old path for a deletion).
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }

    /// True when the patch creates its file.
    pub fn is_creation(&self) -> bool {
        self.old_path.is_none()
    }

    /// True when the patch deletes its file.
    pub fn is_deletion(&self) -> bool {
        self.new_path.is_none()
    }

    /// Lines added and removed across all hunks.
    pub fn counts(&self) -> (usize, usize) {
        let mut added = 0;
        let mut removed = 0;
        for h in &self.hunks {
            for l in &h.lines {
                match l.kind {
                    LineKind::Add => added += 1,
                    LineKind::Remove => removed += 1,
                    LineKind::Context => {}
                }
            }
        }
        (added, removed)
    }
}

/// Why a diff could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffError {
    /// 1-based line of the diff text where the problem was found (0 for whole-input problems).
    pub line: usize,
    /// What was wrong.
    pub kind: DiffErrorKind,
}

/// The kinds of parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffErrorKind {
    /// The diff is larger than [`MAX_DIFF_BYTES`].
    TooLarge,
    /// No file patch was found.
    Empty,
    /// A `---` header had no matching `+++` header.
    MissingNewHeader,
    /// A hunk appeared before any file header.
    HunkWithoutHeader,
    /// A `@@` line could not be understood.
    BadHunkHeader,
    /// A hunk body line did not start with a space, `+`, `-` or `\`.
    BadHunkLine,
    /// The number of lines in a hunk does not match its header.
    HunkCountMismatch {
        /// Old lines the header promised.
        old_expected: usize,
        /// Old lines actually present.
        old_found: usize,
        /// New lines the header promised.
        new_expected: usize,
        /// New lines actually present.
        new_found: usize,
    },
    /// A file header named a rename or copy, which is not supported.
    RenameUnsupported,
    /// A `\ No newline at end of file` marker had no line to attach to.
    StrayNoNewlineMarker,
    /// A file patch has no hunks.
    NoHunks,
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "diff line {}: {:?}", self.line, self.kind)
    }
}

impl std::error::Error for DiffError {}

fn err<T>(line: usize, kind: DiffErrorKind) -> Result<T, DiffError> {
    Err(DiffError { line, kind })
}

/// Splits a `--- path` / `+++ path` header value into its path, dropping a trailing tab and
/// timestamp. `/dev/null` becomes `None`.
fn header_path(rest: &str) -> Option<String> {
    let path = rest.split('\t').next().unwrap_or("").trim();
    if path == "/dev/null" {
        None
    } else {
        Some(path.to_string())
    }
}

/// Removes git's `a/` and `b/` prefixes when BOTH headers carry them.
fn strip_prefixes(old: Option<String>, new: Option<String>) -> (Option<String>, Option<String>) {
    let old_has = old.as_deref().is_none_or(|p| p.starts_with("a/"));
    let new_has = new.as_deref().is_none_or(|p| p.starts_with("b/"));
    if old_has && new_has && (old.is_some() || new.is_some()) {
        (
            old.map(|p| p.strip_prefix("a/").unwrap_or(&p).to_string()),
            new.map(|p| p.strip_prefix("b/").unwrap_or(&p).to_string()),
        )
    } else {
        (old, new)
    }
}

fn parse_range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// Parses `@@ -a[,b] +c[,d] @@ ...` into (old_start, old_len, new_start, new_len).
fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let (os, ol) = parse_range(old)?;
    let (ns, nl) = parse_range(new)?;
    Some((os, ol, ns, nl))
}

/// Parses a unified diff (one or more files, one or more hunks each).
///
/// Strict where it matters: every hunk's line counts must match its header, which also lets the
/// parser stop at the right place when a model appends explanatory prose after the diff. Renames
/// and copies are refused.
pub fn parse(text: &str) -> Result<Vec<FilePatch>, DiffError> {
    if text.len() > MAX_DIFF_BYTES {
        return err(0, DiffErrorKind::TooLarge);
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut patches: Vec<FilePatch> = Vec::new();
    let mut i = 0;
    let mut pending_new_file = false;
    let mut pending_deleted_file = false;
    while i < lines.len() {
        let line = lines.get(i).copied().unwrap_or("");
        if line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("copy from ")
            || line.starts_with("copy to ")
        {
            return err(i + 1, DiffErrorKind::RenameUnsupported);
        }
        if line.starts_with("diff --git ") {
            pending_new_file = false;
            pending_deleted_file = false;
        } else if line.starts_with("new file mode") {
            pending_new_file = true;
        } else if line.starts_with("deleted file mode") {
            pending_deleted_file = true;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            let next = lines.get(i + 1).copied().unwrap_or("");
            let Some(new_rest) = next.strip_prefix("+++ ") else {
                return err(i + 1, DiffErrorKind::MissingNewHeader);
            };
            let (mut old_path, mut new_path) =
                strip_prefixes(header_path(rest), header_path(new_rest));
            if pending_new_file {
                old_path = None;
            }
            if pending_deleted_file {
                new_path = None;
            }
            if let (Some(o), Some(n)) = (&old_path, &new_path) {
                if o != n {
                    return err(i + 1, DiffErrorKind::RenameUnsupported);
                }
            }
            patches.push(FilePatch {
                old_path,
                new_path,
                hunks: Vec::new(),
            });
            pending_new_file = false;
            pending_deleted_file = false;
            i += 2;
            continue;
        }
        if line.starts_with("@@") {
            let Some(patch) = patches.last_mut() else {
                return err(i + 1, DiffErrorKind::HunkWithoutHeader);
            };
            let Some((old_start, old_len, new_start, new_len)) = parse_hunk_header(line) else {
                return err(i + 1, DiffErrorKind::BadHunkHeader);
            };
            let header_line = i + 1;
            let mut hunk_lines: Vec<Line> = Vec::new();
            let (mut old_found, mut new_found) = (0usize, 0usize);
            i += 1;
            while i < lines.len() && (old_found < old_len || new_found < new_len) {
                let l = lines.get(i).copied().unwrap_or("");
                let (kind, body) = match l.chars().next() {
                    Some(' ') => (LineKind::Context, l.get(1..).unwrap_or("")),
                    Some('+') => (LineKind::Add, l.get(1..).unwrap_or("")),
                    Some('-') => (LineKind::Remove, l.get(1..).unwrap_or("")),
                    // An empty line is a context line whose trailing space was stripped.
                    None => (LineKind::Context, ""),
                    Some('\\') => {
                        match hunk_lines.last_mut() {
                            Some(last) => last.no_eol = true,
                            None => return err(i + 1, DiffErrorKind::StrayNoNewlineMarker),
                        }
                        i += 1;
                        continue;
                    }
                    Some(_) => return err(i + 1, DiffErrorKind::BadHunkLine),
                };
                match kind {
                    LineKind::Context => {
                        old_found += 1;
                        new_found += 1;
                    }
                    LineKind::Add => new_found += 1,
                    LineKind::Remove => old_found += 1,
                }
                hunk_lines.push(Line {
                    kind,
                    text: body.to_string(),
                    no_eol: false,
                });
                i += 1;
            }
            // A no-newline marker may follow the final counted line.
            while lines.get(i).is_some_and(|l| l.starts_with('\\')) {
                match hunk_lines.last_mut() {
                    Some(last) => last.no_eol = true,
                    None => return err(i + 1, DiffErrorKind::StrayNoNewlineMarker),
                }
                i += 1;
            }
            if old_found != old_len || new_found != new_len {
                return err(
                    header_line,
                    DiffErrorKind::HunkCountMismatch {
                        old_expected: old_len,
                        old_found,
                        new_expected: new_len,
                        new_found,
                    },
                );
            }
            patch.hunks.push(Hunk {
                old_start,
                old_len,
                new_start,
                new_len,
                lines: hunk_lines,
            });
            continue;
        }
        // Anything else between files (git headers, `index` lines, prose) is skipped.
        i += 1;
    }
    if patches.is_empty() {
        return err(0, DiffErrorKind::Empty);
    }
    for p in &patches {
        if p.hunks.is_empty() {
            return err(0, DiffErrorKind::NoHunks);
        }
    }
    Ok(patches)
}

fn range_text(start: usize, len: usize) -> String {
    if len == 1 {
        start.to_string()
    } else {
        format!("{start},{len}")
    }
}

/// Renders patches as unified-diff text that [`parse`] reads back exactly.
pub fn render(patches: &[FilePatch]) -> String {
    let mut out = String::new();
    for p in patches {
        match &p.old_path {
            Some(o) => out.push_str(&format!("--- a/{o}\n")),
            None => out.push_str("--- /dev/null\n"),
        }
        match &p.new_path {
            Some(n) => out.push_str(&format!("+++ b/{n}\n")),
            None => out.push_str("+++ /dev/null\n"),
        }
        for h in &p.hunks {
            out.push_str(&format!(
                "@@ -{} +{} @@\n",
                range_text(h.old_start, h.old_len),
                range_text(h.new_start, h.new_len)
            ));
            for l in &h.lines {
                out.push(match l.kind {
                    LineKind::Context => ' ',
                    LineKind::Add => '+',
                    LineKind::Remove => '-',
                });
                out.push_str(&l.text);
                out.push('\n');
                if l.no_eol {
                    out.push_str("\\ No newline at end of file\n");
                }
            }
        }
    }
    out
}

/// The patch that undoes `patch`: applying it to the patched file restores the original.
pub fn reverse(patch: &FilePatch) -> FilePatch {
    FilePatch {
        old_path: patch.new_path.clone(),
        new_path: patch.old_path.clone(),
        hunks: patch
            .hunks
            .iter()
            .map(|h| Hunk {
                old_start: h.new_start,
                old_len: h.new_len,
                new_start: h.old_start,
                new_len: h.old_len,
                lines: h
                    .lines
                    .iter()
                    .map(|l| Line {
                        kind: match l.kind {
                            LineKind::Context => LineKind::Context,
                            LineKind::Add => LineKind::Remove,
                            LineKind::Remove => LineKind::Add,
                        },
                        text: l.text.clone(),
                        no_eol: l.no_eol,
                    })
                    .collect(),
            })
            .collect(),
    }
}
