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
}

impl Default for PatchSlot {
    fn default() -> Self {
        Self {
            state: CodeState::shared(),
            gated: false,
            shown: 0,
        }
    }
}

impl PatchSlot {
    pub(crate) fn with(state: SharedCode) -> Self {
        Self {
            state,
            gated: true,
            shown: 0,
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

pub(crate) fn propose<B: Backend>(
    d: &mut Device<B>,
    root: &Path,
    text: &str,
) -> Result<(), PatchError> {
    let summary = {
        let mut st = d.patch.state.borrow_mut();
        st.open(root)?;
        st.propose(text)?
    };
    d.audit_event("patch.propose", &summary, "ok");
    Ok(())
}

pub(crate) fn apply<B: Backend>(d: &mut Device<B>) {
    if d.patch.gated {
        d.call_patch("code.apply");
        return;
    }
    let result = d.patch.state.borrow_mut().apply();
    match result {
        Ok(n) => d.audit_event("patch.apply", &format!("{n} file(s)"), "ok"),
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
