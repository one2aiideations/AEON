//! Code review: the screen side of the Code application.
//!
//! The change itself (parse, check, apply, undo) lives in `mouli_apps::CodeState`. Without the
//! Code application attached to the workspace the device calls that state directly; with it
//! attached the same steps run as capabilities, so applying and undoing need a second person's
//! approval and are recorded by the gate.

use crate::{Backend, Device};
use mouli_apps::{CodeState, SharedCode, Stage, NOT_ALLOWED};
use mouli_screen::screens::{Check, FileChange, PatchState};
use std::path::Path;

pub use mouli_apps::PatchError;

/// The shared state and what the screen last showed of it.
pub(crate) struct PatchSlot {
    pub(crate) state: SharedCode,
    /// Whether the capabilities are registered, so steps go through the gate.
    pub(crate) gated: bool,
    shown: u64,
    /// Who proposed the current change (a model name, or `user`), as given when it was proposed.
    proposer: String,
}

impl Default for PatchSlot {
    fn default() -> Self {
        Self {
            state: CodeState::shared(),
            gated: false,
            shown: 0,
            proposer: String::new(),
        }
    }
}

impl PatchSlot {
    pub(crate) fn with(state: SharedCode) -> Self {
        Self {
            state,
            gated: true,
            shown: 0,
            proposer: String::new(),
        }
    }
}

const fn screen_state(s: Stage) -> PatchState {
    match s {
        Stage::Proposed => PatchState::Proposed,
        Stage::Applied => PatchState::Applied,
        Stage::RolledBack => PatchState::RolledBack,
        Stage::Rejected => PatchState::Rejected,
    }
}

/// Copies the state into the review screen when it has changed since the last copy.
pub(crate) fn sync<B: Backend>(d: &mut Device<B>) {
    let st = d.patch.state.borrow();
    if st.version() == d.patch.shown {
        return;
    }
    let version = st.version();
    match st.view() {
        Some(v) => {
            let files = v
                .files
                .iter()
                .map(|f| FileChange {
                    path: f.path.clone(),
                    added: f.added,
                    removed: f.removed,
                    diff: f.diff.clone(),
                })
                .collect();
            let checks = v
                .checks
                .iter()
                .map(|(name, passed)| Check {
                    name: name.clone(),
                    passed: *passed,
                })
                .collect();
            d.shell.review.set(&v.summary, files, checks);
            d.shell.review.state = screen_state(v.stage);
        }
        None => d.shell.review.set("", vec![], vec![]),
    }
    drop(st);
    d.patch.shown = version;
}

/// A label that is safe to put in the audit trail: visible characters only, at most 64.
pub(crate) fn clean_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned: String = cleaned.chars().take(64).collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

pub(crate) fn propose<B: Backend>(
    d: &mut Device<B>,
    root: &Path,
    text: &str,
    proposer: &str,
) -> Result<(), PatchError> {
    let summary = {
        let mut st = d.patch.state.borrow_mut();
        st.open(root)?;
        st.propose(text)?
    };
    d.patch.proposer = clean_label(proposer);
    let subject = format!("{summary} [proposed by {}]", d.patch.proposer);
    d.audit_event("patch.propose", &subject, "ok");
    Ok(())
}

/// After a step that may have applied the change, records who proposed what was applied and who
/// applied it, as one line in the audit trail (the gate or the direct path records the apply
/// itself; this ties it to its source).
pub(crate) fn note_origin<B: Backend>(d: &mut Device<B>, was_applied: bool, how: &str) {
    let applied_now = d
        .patch
        .state
        .borrow()
        .view()
        .is_some_and(|v| v.stage == Stage::Applied);
    if applied_now && !was_applied {
        let subject = format!("proposed by {}, {how} {}", d.patch.proposer, d.user);
        d.audit_event("patch.origin", &subject, "ok");
    }
}

pub(crate) fn is_applied<B: Backend>(d: &Device<B>) -> bool {
    d.patch
        .state
        .borrow()
        .view()
        .is_some_and(|v| v.stage == Stage::Applied)
}

pub(crate) fn apply<B: Backend>(d: &mut Device<B>) {
    let was_applied = is_applied(d);
    if d.patch.gated {
        d.call_patch("code.apply");
        note_origin(d, was_applied, "applied by");
        return;
    }
    let result = d.patch.state.borrow_mut().apply();
    match result {
        Ok(n) => {
            d.audit_event("patch.apply", &format!("{n} file(s)"), "ok");
            note_origin(d, was_applied, "applied by");
        }
        Err(m) => {
            let kind = if m == NOT_ALLOWED { "denied" } else { "failed" };
            d.audit_event("patch.apply", "-", &format!("{kind}: {m}"));
            d.shell.review.message = m;
        }
    }
}

pub(crate) fn rollback<B: Backend>(d: &mut Device<B>) {
    if d.patch.gated {
        d.call_patch("code.rollback");
        return;
    }
    let result = d.patch.state.borrow_mut().rollback();
    match result {
        Ok(()) => d.audit_event("patch.rollback", "-", "ok"),
        Err(m) => {
            d.audit_event("patch.rollback", "-", &format!("failed: {m}"));
            d.shell.review.message = m;
        }
    }
}

pub(crate) fn reject<B: Backend>(d: &mut Device<B>) {
    if d.patch.gated {
        d.call_patch("code.reject");
        return;
    }
    d.patch.state.borrow_mut().reject();
    d.audit_event("patch.reject", "-", "ok");
}
