//! Code review: parse a diff, assess it against the real files, and apply or undo it atomically.

use crate::{Backend, Device};
use mouli_code::apply::ApplyOptions;
use mouli_code::diff::{self, FilePatch};
use mouli_code::quality::{assess, brackets_balanced, PatchPolicy};
use mouli_code::workspace::{Receipt, Workspace as Files, WorkspaceError};
use mouli_screen::screens::{Check, FileChange, PatchState};
use std::path::Path;

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

#[derive(Default)]
pub(crate) struct PatchSlot {
    files: Option<Files>,
    patches: Vec<FilePatch>,
    receipt: Option<Receipt>,
    passed: bool,
}

pub(crate) fn propose<B: Backend>(
    d: &mut Device<B>,
    root: &Path,
    text: &str,
) -> Result<(), PatchError> {
    let patches = diff::parse(text).map_err(|e| PatchError::BadDiff(format!("{e:?}")))?;
    if patches.is_empty() {
        return Err(PatchError::BadDiff("it changes no files".into()));
    }
    let files = Files::open(root).map_err(|e| PatchError::Files(format!("{e:?}")))?;
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
        Check {
            name: "fits the files".into(),
            passed: true,
        },
        Check {
            name: "policy".into(),
            passed: report.acceptable,
        },
    ];
    for f in report.findings.iter().filter(|f| f.kind.is_blocking()) {
        checks.push(Check {
            name: format!("{:?}", f.kind),
            passed: false,
        });
    }
    let changes: Vec<FileChange> = patches
        .iter()
        .map(|p| {
            let diff_lines: Vec<String> = diff::render(std::slice::from_ref(p))
                .lines()
                .map(str::to_string)
                .collect();
            let count = |c: char, skip: &str| {
                diff_lines
                    .iter()
                    .filter(|l| l.starts_with(c) && !l.starts_with(skip))
                    .count()
            };
            FileChange {
                path: p.path().to_string(),
                added: count('+', "+++"),
                removed: count('-', "---"),
                diff: diff_lines,
            }
        })
        .collect();
    let summary = format!("{} file(s), risk {}/100", patches.len(), report.risk);
    d.shell.review.set(&summary, changes, checks);
    d.patch = PatchSlot {
        files: Some(files),
        patches,
        receipt: None,
        passed: report.acceptable,
    };
    d.audit_event("patch.propose", &summary, "ok");
    Ok(())
}

pub(crate) fn apply<B: Backend>(d: &mut Device<B>) {
    // The screen already refuses when a check failed; the controller checks again because the
    // screen is not the authority.
    if !d.patch.passed || d.patch.receipt.is_some() {
        d.shell.review.message = "This change cannot be applied.".into();
        d.audit_event(
            "patch.apply",
            "-",
            "denied: checks did not pass or already applied",
        );
        return;
    }
    let Some(files) = d.patch.files.take() else {
        d.shell.review.message = "No change is loaded.".into();
        return;
    };
    let result = files.apply(&d.patch.patches, ApplyOptions::default());
    d.patch.files = Some(files);
    match result {
        Ok(receipt) => {
            let n = receipt.changed_paths().len();
            d.patch.receipt = Some(receipt);
            d.review_state(PatchState::Applied);
            d.audit_event("patch.apply", &format!("{n} file(s)"), "ok");
        }
        Err(e) => {
            d.shell.review.message = format!("Nothing was changed: {}", describe(&e));
            d.audit_event("patch.apply", "-", &format!("failed: {}", describe(&e)));
        }
    }
}

pub(crate) fn rollback<B: Backend>(d: &mut Device<B>) {
    let (Some(files), Some(receipt)) = (d.patch.files.take(), d.patch.receipt.take()) else {
        d.shell.review.message = "Nothing has been applied.".into();
        return;
    };
    let result = files.rollback(&receipt);
    d.patch.files = Some(files);
    match result {
        Ok(()) => {
            d.review_state(PatchState::RolledBack);
            d.audit_event("patch.rollback", "-", "ok");
        }
        Err(e) => {
            // Keep the receipt: the person may fix the file and try again.
            d.patch.receipt = Some(receipt);
            d.shell.review.message = format!("Not undone, nothing was touched: {}", describe(&e));
            d.audit_event("patch.rollback", "-", &format!("failed: {}", describe(&e)));
        }
    }
}

pub(crate) fn reject<B: Backend>(d: &mut Device<B>) {
    d.patch = PatchSlot::default();
    d.review_state(PatchState::Rejected);
    d.audit_event("patch.reject", "-", "ok");
}

fn describe(e: &WorkspaceError) -> String {
    match e {
        WorkspaceError::ChangedSince(p) => format!("{p} was edited after the change was applied"),
        other => format!("{other:?}"),
    }
}
