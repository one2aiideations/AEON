//! The Code application as capabilities: propose, apply and roll back a change to source files.
//!
//! The mechanics live in `mouli-code` (a strict unified-diff engine, a quality policy, an atomic
//! apply that keeps a receipt, and an exact rollback). This adapter puts them behind the
//! workspace gate: writing to the files (`code.apply`, `code.rollback`) is consequential, so a
//! second person approves it and the audit chain records both people. Proposing a change only
//! reads the files, so it is a plain write to the application's own state.
//!
//! The project folder is chosen by the host ([`CodeState::open`]), never by a call, so a model
//! cannot point the application at another directory.

use mouli_code::apply::safe_relative_path;
use mouli_code::apply::ApplyOptions;
use mouli_code::diff::{self, FilePatch};
use mouli_code::make::make_patch;
use mouli_code::quality::{assess, brackets_balanced, PatchPolicy};
use mouli_code::workspace::{Receipt, Workspace as Files, WorkspaceError};
use mouli_interface::{Application, Call, CapabilitySpec, Error, Output, Result, Risk};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;

/// Largest file the editor reads, in bytes.
pub const MAX_FILE: u64 = 256 * 1024;
/// Most files listed.
const MAX_FILES: usize = 5_000;
/// Deepest folder searched.
const MAX_DEPTH: usize = 12;
/// Folders never listed: generated or private.
const SKIP_DIRS: [&str; 3] = ["target", "node_modules", ".git"];

/// Largest diff accepted, in bytes.
const MAX_DIFF: usize = 1 << 20;

/// The message when a change is refused because its checks failed or it was applied already.
pub const NOT_ALLOWED: &str = "This change cannot be applied.";

/// Why a change could not be put under review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// The diff text is not a valid unified diff.
    BadDiff(String),
    /// The folder or a file in it could not be read.
    Files(String),
    /// The diff does not fit the files as they are now.
    DoesNotApply(String),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchError::BadDiff(m) => write!(f, "not a valid diff: {m}"),
            PatchError::Files(m) => write!(f, "cannot read the files: {m}"),
            PatchError::DoesNotApply(m) => write!(f, "does not fit the files: {m}"),
        }
    }
}

impl std::error::Error for PatchError {}

/// Where a proposed change stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Proposed; nothing has changed on disk.
    Proposed,
    /// Written to the files.
    Applied,
    /// Put back as it was.
    RolledBack,
    /// Thrown away.
    Rejected,
}

/// One changed file, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Path below the project folder.
    pub path: String,
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// The diff lines (every hunk, kept or skipped).
    pub diff: Vec<String>,
    /// For each hunk in order, whether the reviewer skipped it (a skipped hunk is not written).
    pub skipped: Vec<bool>,
}

/// A change under review, as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// One line: files and risk.
    pub summary: String,
    /// The files.
    pub files: Vec<ChangedFile>,
    /// Named checks and whether each passed.
    pub checks: Vec<(String, bool)>,
    /// Where it stands.
    pub stage: Stage,
}

/// The state of the Code application.
#[derive(Default)]
pub struct CodeState {
    files: Option<Files>,
    patches: Vec<FilePatch>,
    /// Hunks the reviewer skipped, as (file index, hunk index).
    skipped: BTreeSet<(usize, usize)>,
    /// The files' text as read when the change was proposed.
    current: BTreeMap<String, String>,
    receipt: Option<Receipt>,
    passed: bool,
    view: Option<View>,
    version: u64,
}

/// The Code state, shared between the adapter and the review screen.
pub type SharedCode = Rc<RefCell<CodeState>>;

fn describe(e: &WorkspaceError) -> String {
    match e {
        WorkspaceError::ChangedSince(p) => format!("{p} was edited after the change was applied"),
        other => format!("{other:?}"),
    }
}

impl CodeState {
    /// A new, empty state wrapped for sharing.
    pub fn shared() -> SharedCode {
        Rc::new(RefCell::new(Self::default()))
    }

    /// Chooses the project folder. Any change under review is dropped.
    ///
    /// # Errors
    /// [`PatchError::Files`] when the folder cannot be opened.
    pub fn open(&mut self, root: &Path) -> std::result::Result<(), PatchError> {
        let files = Files::open(root).map_err(|e| PatchError::Files(format!("{e:?}")))?;
        *self = Self {
            files: Some(files),
            version: self.version + 1,
            ..Self::default()
        };
        Ok(())
    }

    /// The change under review, if any.
    pub fn view(&self) -> Option<&View> {
        self.view.as_ref()
    }

    /// Counts every change of what [`CodeState::view`] returns, so a screen can tell when to
    /// reload.
    pub fn version(&self) -> u64 {
        self.version
    }

    fn set_stage(&mut self, stage: Stage) {
        if let Some(v) = &mut self.view {
            v.stage = stage;
        }
        self.version += 1;
    }

    /// The files below the project folder as relative paths with `/`, sorted. Hidden entries,
    /// build output and symbolic links are left out; the list is capped.
    ///
    /// # Errors
    /// A message when no folder is open.
    pub fn list_files(&self) -> std::result::Result<Vec<String>, String> {
        let files = self.files.as_ref().ok_or("No project folder is open.")?;
        let mut out = Vec::new();
        let mut stack = vec![(files.root().to_path_buf(), String::new(), 0usize)];
        while let Some((dir, prefix, depth)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                // `file_type` does not follow symbolic links, so a link is neither a file nor a folder.
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let rel = format!("{prefix}{name}");
                if kind.is_dir() {
                    if depth < MAX_DEPTH && !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push((entry.path(), format!("{rel}/"), depth + 1));
                    }
                } else if kind.is_file() && safe_relative_path(&rel).is_ok() {
                    out.push(rel);
                    if out.len() >= MAX_FILES {
                        out.sort();
                        return Ok(out);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// The text of one file.
    ///
    /// # Errors
    /// A message when no folder is open, the path is unsafe, the file is missing, too large or
    /// not text.
    pub fn read(&self, path: &str) -> std::result::Result<String, String> {
        let files = self.files.as_ref().ok_or("No project folder is open.")?;
        let rel = safe_relative_path(path).map_err(|e| format!("{e:?}"))?;
        if let Ok(meta) = std::fs::metadata(files.root().join(rel)) {
            if meta.len() > MAX_FILE {
                return Err(format!("{rel} is larger than {} KB.", MAX_FILE / 1024));
            }
        }
        files
            .read(rel)
            .map_err(|e| describe(&e))?
            .ok_or_else(|| format!("{rel} does not exist."))
    }

    /// Turns "this file should now read `new_text`" into a proposal and holds it for review.
    /// A file that does not exist yet is created by the change. One change is under review at a
    /// time: a second file is refused until the first is applied or rejected, and a new edit of
    /// the same file replaces the held one.
    ///
    /// # Errors
    /// A message when the text is unchanged, another file's change is held, or the proposal is
    /// refused (see [`CodeState::propose`]).
    pub fn edit(&mut self, path: &str, new_text: &str) -> std::result::Result<String, String> {
        if new_text.len() as u64 > MAX_FILE {
            return Err(format!("The text is larger than {} KB.", MAX_FILE / 1024));
        }
        let rel = safe_relative_path(path)
            .map_err(|e| format!("{e:?}"))?
            .to_string();
        if let Some(v) = &self.view {
            let busy =
                v.stage == Stage::Proposed || (v.stage == Stage::Applied && self.receipt.is_some());
            let same = v.files.len() == 1 && v.files[0].path == rel && v.stage == Stage::Proposed;
            if busy && !same {
                let held = v.files.first().map_or("a file", |f| f.path.as_str());
                return Err(if v.stage == Stage::Applied {
                    format!(
                        "The change to {held} was applied and can still be rolled back. Roll it back or reject it before editing again."
                    )
                } else {
                    format!("A change to {held} is already under review. Apply or reject it first.")
                });
            }
        }
        let old = match self
            .files
            .as_ref()
            .ok_or("No project folder is open.")?
            .read(&rel)
        {
            Ok(t) => t,
            Err(e) => return Err(describe(&e)),
        };
        let patch = make_patch(&rel, old.as_deref(), Some(new_text), 3)
            .map_err(|e| format!("{e:?}"))?
            .ok_or("The text is the same as the file.")?;
        self.propose(&diff::render(&[patch]))
            .map_err(|e| e.to_string())
    }

    /// Reads `text` as a unified diff, checks it against the real files and holds it for review.
    ///
    /// # Errors
    /// [`PatchError`] when no folder is open, the diff is invalid, or it does not fit the files.
    pub fn propose(&mut self, text: &str) -> std::result::Result<String, PatchError> {
        if text.len() > MAX_DIFF {
            return Err(PatchError::BadDiff("the diff is too large".into()));
        }
        let files = self
            .files
            .clone()
            .ok_or_else(|| PatchError::Files("no project folder is open".into()))?;
        let patches = diff::parse(text).map_err(|e| PatchError::BadDiff(format!("{e:?}")))?;
        if patches.is_empty() {
            return Err(PatchError::BadDiff("it changes no files".into()));
        }
        let paths: Vec<&str> = patches.iter().map(FilePatch::path).collect();
        let current = files
            .read_many(paths.iter().copied())
            .map_err(|e| PatchError::Files(format!("{e:?}")))?;
        // Check the whole change first, so a diff that does not fit is refused outright.
        assess(
            &current,
            &patches,
            &PatchPolicy::default(),
            &brackets_balanced,
        )
        .map_err(|e| PatchError::DoesNotApply(format!("{e:?}")))?;
        self.patches = patches;
        self.skipped.clear();
        self.current = current;
        self.receipt = None;
        self.files = Some(files);
        let summary = self.rebuild(Stage::Proposed);
        self.version += 1;
        Ok(summary)
    }

    /// The change with the skipped hunks left out (a file with every hunk skipped is left out).
    fn kept_patches(&self) -> Vec<FilePatch> {
        self.patches
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let mut p = p.clone();
                if p.hunks.is_empty() {
                    return Some(p);
                }
                let mut h = 0;
                p.hunks.retain(|_| {
                    let keep = !self.skipped.contains(&(i, h));
                    h += 1;
                    keep
                });
                (!p.hunks.is_empty()).then_some(p)
            })
            .collect()
    }

    /// Recomputes the checks and what the screen shows from the hunks now kept. Returns the
    /// one-line summary.
    fn rebuild(&mut self, stage: Stage) -> String {
        let kept = self.kept_patches();
        let hunks_total: usize = self.patches.iter().map(|p| p.hunks.len()).sum();
        let hunks_kept = hunks_total - self.skipped.len();
        let mut checks;
        let risk;
        if kept.is_empty() {
            checks = vec![("something is kept".to_string(), false)];
            self.passed = false;
            risk = 0;
        } else {
            match assess(
                &self.current,
                &kept,
                &PatchPolicy::default(),
                &brackets_balanced,
            ) {
                Ok(report) => {
                    checks = vec![
                        ("fits the files".to_string(), true),
                        ("policy".to_string(), report.acceptable),
                    ];
                    for f in report.findings.iter().filter(|f| f.kind.is_blocking()) {
                        checks.push((format!("{:?}", f.kind), false));
                    }
                    self.passed = report.acceptable;
                    risk = report.risk;
                }
                Err(_) => {
                    // Possible when the kept hunks depend on a skipped one.
                    checks = vec![("fits the files".to_string(), false)];
                    self.passed = false;
                    risk = 0;
                }
            }
        }
        let changed: Vec<ChangedFile> = self
            .patches
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let lines_of = |p: &FilePatch| -> Vec<String> {
                    diff::render(std::slice::from_ref(p))
                        .lines()
                        .map(str::to_string)
                        .collect()
                };
                let count = |lines: &[String], c: char, skip: &str| {
                    lines
                        .iter()
                        .filter(|l| l.starts_with(c) && !l.starts_with(skip))
                        .count()
                };
                let kept_lines = kept
                    .iter()
                    .find(|k| k.path() == p.path())
                    .map(lines_of)
                    .unwrap_or_default();
                ChangedFile {
                    path: p.path().to_string(),
                    added: count(&kept_lines, '+', "+++"),
                    removed: count(&kept_lines, '-', "---"),
                    diff: lines_of(p),
                    skipped: (0..p.hunks.len())
                        .map(|h| self.skipped.contains(&(i, h)))
                        .collect(),
                }
            })
            .collect();
        let summary = if self.skipped.is_empty() {
            format!("{} file(s), risk {}/100", kept.len(), risk)
        } else {
            format!(
                "{} file(s), {hunks_kept} of {hunks_total} hunks, risk {risk}/100",
                kept.len()
            )
        };
        self.view = Some(View {
            summary: summary.clone(),
            files: changed,
            checks,
            stage,
        });
        summary
    }

    /// How many hunks are kept, and how many the change has.
    pub fn hunks(&self) -> (usize, usize) {
        let total: usize = self.patches.iter().map(|p| p.hunks.len()).sum();
        (total - self.skipped.len(), total)
    }

    /// Keeps or skips one hunk of one file of the change under review. A skipped hunk is not
    /// written when the change is applied; the checks are run again on what is kept.
    ///
    /// # Errors
    /// A message when no change is under review, it is no longer only proposed, or there is no
    /// such hunk.
    pub fn set_hunk(
        &mut self,
        file: usize,
        hunk: usize,
        keep: bool,
    ) -> std::result::Result<(), String> {
        let Some(view) = &self.view else {
            return Err("No change is loaded.".into());
        };
        if view.stage != Stage::Proposed || self.receipt.is_some() {
            return Err("Hunks can only be chosen before the change is applied.".into());
        }
        if self.patches.get(file).is_none_or(|p| hunk >= p.hunks.len()) {
            return Err("There is no such hunk.".into());
        }
        if keep {
            self.skipped.remove(&(file, hunk));
        } else {
            self.skipped.insert((file, hunk));
        }
        self.rebuild(Stage::Proposed);
        self.version += 1;
        Ok(())
    }

    /// Writes the held change to the files, all or nothing. Returns how many files changed.
    ///
    /// # Errors
    /// A message when nothing is held, the checks failed, it was applied already, or the write
    /// failed (in which case nothing was changed).
    pub fn apply(&mut self) -> std::result::Result<usize, String> {
        if self.view.is_none() {
            return Err("No change is loaded.".into());
        }
        // The screen already refuses in this case; the state does not rely on that.
        if !self.passed || self.receipt.is_some() {
            return Err(NOT_ALLOWED.into());
        }
        let files = self.files.clone().ok_or("No project folder is open.")?;
        let receipt = files
            .apply(&self.kept_patches(), ApplyOptions::default())
            .map_err(|e| format!("Nothing was changed: {}", describe(&e)))?;
        let n = receipt.changed_paths().len();
        self.receipt = Some(receipt);
        self.set_stage(Stage::Applied);
        Ok(n)
    }

    /// Puts the files back exactly as they were before [`CodeState::apply`].
    ///
    /// # Errors
    /// A message when nothing was applied, or a file was edited since (nothing is touched then
    /// and the receipt is kept so it can be tried again).
    pub fn rollback(&mut self) -> std::result::Result<(), String> {
        let (Some(files), Some(receipt)) = (self.files.clone(), self.receipt.take()) else {
            return Err("Nothing has been applied.".into());
        };
        match files.rollback(&receipt) {
            Ok(()) => {
                self.set_stage(Stage::RolledBack);
                Ok(())
            }
            Err(e) => {
                self.receipt = Some(receipt);
                Err(format!("Not undone, nothing was touched: {}", describe(&e)))
            }
        }
    }

    /// Throws the held change away. Files already written are not touched; roll back first.
    pub fn reject(&mut self) {
        let files = self.files.take();
        let version = self.version;
        *self = Self {
            files,
            view: self.view.take().map(|mut v| {
                v.stage = Stage::Rejected;
                v
            }),
            version: version + 1,
            ..Self::default()
        };
    }
}

/// The Code application.
pub struct CodeApp {
    state: SharedCode,
}

impl CodeApp {
    /// An application over `state`.
    pub fn new(state: SharedCode) -> Self {
        Self { state }
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Handler(msg.into())
}

impl Application for CodeApp {
    fn name(&self) -> &str {
        "code"
    }

    fn capabilities(&self) -> Vec<CapabilitySpec> {
        vec![
            CapabilitySpec::new("code.status", "show the change under review", Risk::Read),
            CapabilitySpec::new(
                "code.propose",
                "check a unified diff against the files and hold it for review (diff=...)",
                Risk::Write,
            )
            .requires(&["diff"]),
            CapabilitySpec::new("code.files", "list the project's files", Risk::Read),
            CapabilitySpec::new("code.read", "read one file (path=src/lib.rs)", Risk::Read)
                .requires(&["path"]),
            CapabilitySpec::new(
                "code.edit",
                "propose the file's new whole text as a change (path=.. text=..); nothing is written until applied",
                Risk::Write,
            )
            .requires(&["path", "text"]),
            CapabilitySpec::new("code.reject", "throw the held change away", Risk::Write),
            CapabilitySpec::new(
                "code.apply",
                "write the held change to the files; needs approval",
                Risk::Consequential,
            ),
            CapabilitySpec::new(
                "code.rollback",
                "put the files back as they were before the change; needs approval",
                Risk::Consequential,
            ),
        ]
    }

    fn invoke(&mut self, call: &Call) -> Result<Output> {
        let mut st = self.state.borrow_mut();
        let text = match call.capability.as_str() {
            "code.status" => match st.view() {
                None => "No change is under review.".to_string(),
                Some(v) => {
                    let mut s = format!("{} ({:?})", v.summary, v.stage);
                    for f in &v.files {
                        s.push_str(&format!("\n  {}  +{} -{}", f.path, f.added, f.removed));
                    }
                    for (name, ok) in &v.checks {
                        s.push_str(&format!(
                            "\n  [{}] {name}",
                            if *ok { "pass" } else { "FAIL" }
                        ));
                    }
                    s
                }
            },
            "code.files" => st.list_files().map_err(bad)?.join("\n"),
            "code.read" => {
                let path = call
                    .args
                    .get("path")
                    .ok_or_else(|| bad("path is required"))?;
                st.read(path).map_err(bad)?
            }
            "code.edit" => {
                let path = call
                    .args
                    .get("path")
                    .ok_or_else(|| bad("path is required"))?;
                let text = call
                    .args
                    .get("text")
                    .ok_or_else(|| bad("text is required"))?;
                let summary = st.edit(path, text).map_err(bad)?;
                format!("Held for review: {summary}.")
            }
            "code.propose" => {
                let diff = call
                    .args
                    .get("diff")
                    .filter(|d| !d.trim().is_empty())
                    .ok_or_else(|| bad("diff is required"))?;
                let summary = st.propose(diff).map_err(|e| bad(e.to_string()))?;
                format!("Held for review: {summary}.")
            }
            "code.apply" => {
                let n = st.apply().map_err(bad)?;
                format!("Applied to {n} file(s).")
            }
            "code.rollback" => {
                st.rollback().map_err(bad)?;
                "Rolled back; the files are as they were.".to_string()
            }
            "code.reject" => {
                st.reject();
                "The change was thrown away.".to_string()
            }
            other => return Err(bad(format!("unknown capability {other}"))),
        };
        Ok(Output {
            text,
            artifacts: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> (tempfile::TempDir, CodeState) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::create_dir_all(d.path().join("target/debug")).unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join("src/lib.rs"), "fn a() {}\n").unwrap();
        fs::write(d.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(d.path().join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(d.path().join("target/debug/junk"), "x").unwrap();
        fs::write(d.path().join(".git/config"), "x").unwrap();
        fs::write(d.path().join(".hidden"), "x").unwrap();
        let mut st = CodeState::default();
        st.open(d.path()).unwrap();
        (d, st)
    }

    #[test]
    fn files_are_listed_sorted_without_hidden_build_or_link_entries() {
        let (d, st) = project();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", d.path().join("link.txt")).unwrap();
            std::os::unix::fs::symlink("/etc", d.path().join("linkdir")).unwrap();
        }
        assert_eq!(
            st.list_files().unwrap(),
            vec!["Cargo.toml", "src/lib.rs", "src/main.rs"]
        );
        assert!(CodeState::default().list_files().is_err());
    }

    #[test]
    fn reading_is_refused_for_unsafe_missing_large_and_binary_files() {
        let (d, st) = project();
        assert_eq!(st.read("src/lib.rs").unwrap(), "fn a() {}\n");
        for bad in [
            "../x",
            "/etc/passwd",
            ".git/config",
            "src/../../x",
            "",
            "a\\b",
        ] {
            assert!(st.read(bad).is_err(), "{bad}");
        }
        assert!(st
            .read("src/none.rs")
            .unwrap_err()
            .contains("does not exist"));
        fs::write(d.path().join("big.txt"), vec![b'x'; MAX_FILE as usize + 1]).unwrap();
        assert!(st.read("big.txt").unwrap_err().contains("larger"));
        fs::write(d.path().join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(st.read("bin.dat").is_err());
        assert!(CodeState::default().read("a").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", d.path().join("l.txt")).unwrap();
            assert!(st.read("l.txt").is_err(), "a link is not followed");
        }
    }

    #[test]
    fn an_edit_becomes_a_held_proposal_and_writes_nothing() {
        let (d, mut st) = project();
        let summary = st.edit("src/lib.rs", "fn a() {}\nfn b() {}\n").unwrap();
        assert!(summary.contains("1 file"), "{summary}");
        let v = st.view().unwrap();
        assert_eq!((v.files[0].added, v.files[0].removed), (1, 0));
        assert_eq!(v.stage, Stage::Proposed);
        assert_eq!(
            fs::read_to_string(d.path().join("src/lib.rs")).unwrap(),
            "fn a() {}\n"
        );
        // Applying writes exactly the edit; rollback restores it.
        st.apply().unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("src/lib.rs")).unwrap(),
            "fn a() {}\nfn b() {}\n"
        );
        st.rollback().unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("src/lib.rs")).unwrap(),
            "fn a() {}\n"
        );
    }

    #[test]
    fn an_edit_can_create_a_new_file_and_an_unchanged_text_is_refused() {
        let (d, mut st) = project();
        assert!(st
            .edit("src/lib.rs", "fn a() {}\n")
            .unwrap_err()
            .contains("same"));
        st.edit("src/new.rs", "pub fn n() {}\n").unwrap();
        st.apply().unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("src/new.rs")).unwrap(),
            "pub fn n() {}\n"
        );
    }

    #[test]
    fn only_one_change_is_held_and_a_new_edit_of_the_same_file_replaces_it() {
        let (_d, mut st) = project();
        st.edit("src/lib.rs", "one\n").unwrap();
        st.edit("src/lib.rs", "two\n").unwrap();
        assert_eq!(st.view().unwrap().files.len(), 1);
        let e = st.edit("src/main.rs", "x\n").unwrap_err();
        assert!(
            e.contains("src/lib.rs") && e.contains("under review"),
            "{e}"
        );
        st.reject();
        st.edit("src/main.rs", "x\n").unwrap();
    }

    #[test]
    fn after_an_apply_another_edit_waits_until_the_change_is_rolled_back_or_rejected() {
        let (_d, mut st) = project();
        st.edit("src/lib.rs", "one\n").unwrap();
        st.apply().unwrap();
        let e = st.edit("src/main.rs", "x\n").unwrap_err();
        assert!(e.contains("rolled back"), "{e}");
        st.rollback().unwrap();
        st.edit("src/main.rs", "x\n").unwrap();
    }

    #[test]
    fn an_oversized_edit_is_refused_before_any_work_is_done() {
        let (_d, mut st) = project();
        let big = "x\n".repeat(MAX_FILE as usize);
        assert!(st.edit("src/lib.rs", &big).unwrap_err().contains("larger"));
        assert!(st.view().is_none());
    }

    #[test]
    fn unsafe_edit_paths_and_no_folder_are_refused() {
        let (_d, mut st) = project();
        assert!(st.edit("../x", "a\n").is_err());
        assert!(st.edit(".git/config", "a\n").is_err());
        assert!(CodeState::default().edit("a", "b\n").is_err());
    }

    /// A 30-line function; the two edits below are far enough apart to be separate hunks.
    fn long_file() -> String {
        let mut t = String::from("fn f() {\n");
        for i in 1..=28 {
            t.push_str(&format!("    let v{i} = {i};\n"));
        }
        t.push_str("}\n");
        t
    }

    fn two_hunks(second: &str) -> (tempfile::TempDir, CodeState, String, String) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        let old = long_file();
        fs::write(d.path().join("src/long.rs"), &old).unwrap();
        let new = old
            .replace("let v3 = 3;", "let v3 = 33;")
            .replace("    let v25 = 25;", second);
        let patch = make_patch("src/long.rs", Some(&old), Some(&new), 3)
            .unwrap()
            .unwrap();
        assert_eq!(patch.hunks.len(), 2, "the edits must be two hunks");
        let mut st = CodeState::default();
        st.open(d.path()).unwrap();
        st.propose(&diff::render(&[patch])).unwrap();
        (d, st, old, new)
    }

    fn read_long(d: &tempfile::TempDir) -> String {
        fs::read_to_string(d.path().join("src/long.rs")).unwrap()
    }

    #[test]
    fn a_skipped_hunk_is_not_written_and_rollback_restores_the_original() {
        let (d, mut st, old, _new) = two_hunks("    let v25 = 250;");
        assert_eq!(st.hunks(), (2, 2));
        st.set_hunk(0, 1, false).unwrap();
        assert_eq!(st.hunks(), (1, 2));
        let v = st.view().unwrap();
        assert_eq!(v.files[0].skipped, [false, true]);
        assert!(v.summary.contains("1 of 2 hunks"), "{}", v.summary);
        assert_eq!(
            (v.files[0].added, v.files[0].removed),
            (1, 1),
            "counts are of what is kept"
        );
        assert_eq!(st.apply(), Ok(1));
        let written = read_long(&d);
        assert!(written.contains("let v3 = 33;"), "{written}");
        assert!(
            written.contains("let v25 = 25;"),
            "the skipped hunk must not be written"
        );
        assert!(!written.contains("250"));
        st.rollback().unwrap();
        assert_eq!(read_long(&d), old);
    }

    #[test]
    fn the_checks_are_run_again_on_what_is_kept() {
        // The second hunk opens a brace that is never closed: the whole change fails the policy.
        let (d, mut st, _old, _new) = two_hunks("    if true {");
        let failing = |st: &CodeState| st.view().unwrap().checks.iter().any(|(_, ok)| !ok);
        assert!(failing(&st));
        assert_eq!(st.apply(), Err(NOT_ALLOWED.to_string()));
        assert!(!read_long(&d).contains("v3 = 33"), "nothing written");
        // Skipping that hunk leaves a change that passes, and it can be applied.
        st.set_hunk(0, 1, false).unwrap();
        assert!(!failing(&st), "{:?}", st.view().unwrap().checks);
        assert_eq!(st.apply(), Ok(1));
        assert!(read_long(&d).contains("v3 = 33"));
        assert!(!read_long(&d).contains("if true"));
    }

    #[test]
    fn skipping_everything_cannot_be_applied_and_keeping_one_again_can() {
        let (d, mut st, old, _new) = two_hunks("    let v25 = 250;");
        st.set_hunk(0, 0, false).unwrap();
        st.set_hunk(0, 1, false).unwrap();
        assert_eq!(st.hunks(), (0, 2));
        assert_eq!(
            st.view().unwrap().checks,
            [("something is kept".to_string(), false)]
        );
        assert_eq!(st.apply(), Err(NOT_ALLOWED.to_string()));
        assert_eq!(read_long(&d), old);
        st.set_hunk(0, 0, true).unwrap();
        assert_eq!(st.apply(), Ok(1));
        assert!(read_long(&d).contains("v3 = 33"));
    }

    #[test]
    fn choosing_hunks_is_refused_with_no_change_after_applying_and_for_hunks_that_do_not_exist() {
        let mut st = CodeState::default();
        assert!(st.set_hunk(0, 0, false).is_err(), "no change is loaded");
        let (_d, mut st, _old, _new) = two_hunks("    let v25 = 250;");
        assert!(st.set_hunk(0, 2, false).is_err());
        assert!(st.set_hunk(1, 0, false).is_err());
        assert_eq!(st.hunks(), (2, 2), "refused choices change nothing");
        st.apply().unwrap();
        assert!(st
            .set_hunk(0, 0, false)
            .unwrap_err()
            .contains("before the change is applied"));
        // A fresh proposal starts with every hunk kept again.
        st.reject();
        assert!(st.set_hunk(0, 0, false).is_err());
    }
}
