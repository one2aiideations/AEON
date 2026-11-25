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

use mouli_code::apply::ApplyOptions;
use mouli_code::diff::{self, FilePatch};
use mouli_code::quality::{assess, brackets_balanced, PatchPolicy};
use mouli_code::workspace::{Receipt, Workspace as Files, WorkspaceError};
use mouli_interface::{Application, Call, CapabilitySpec, Error, Output, Result, Risk};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

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
    /// The diff lines.
    pub diff: Vec<String>,
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
        let report = assess(
            &current,
            &patches,
            &PatchPolicy::default(),
            &brackets_balanced,
        )
        .map_err(|e| PatchError::DoesNotApply(format!("{e:?}")))?;
        let mut checks = vec![
            ("fits the files".to_string(), true),
            ("policy".to_string(), report.acceptable),
        ];
        for f in report.findings.iter().filter(|f| f.kind.is_blocking()) {
            checks.push((format!("{:?}", f.kind), false));
        }
        let changed: Vec<ChangedFile> = patches
            .iter()
            .map(|p| {
                let lines: Vec<String> = diff::render(std::slice::from_ref(p))
                    .lines()
                    .map(str::to_string)
                    .collect();
                let count = |c: char, skip: &str| {
                    lines
                        .iter()
                        .filter(|l| l.starts_with(c) && !l.starts_with(skip))
                        .count()
                };
                ChangedFile {
                    path: p.path().to_string(),
                    added: count('+', "+++"),
                    removed: count('-', "---"),
                    diff: lines,
                }
            })
            .collect();
        let summary = format!("{} file(s), risk {}/100", patches.len(), report.risk);
        self.view = Some(View {
            summary: summary.clone(),
            files: changed,
            checks,
            stage: Stage::Proposed,
        });
        self.patches = patches;
        self.receipt = None;
        self.passed = report.acceptable;
        self.files = Some(files);
        self.version += 1;
        Ok(summary)
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
            .apply(&self.patches, ApplyOptions::default())
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
