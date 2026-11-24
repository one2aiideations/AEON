//! Applications as capabilities.
//!
//! Each adapter implements the interface's `Application` trait, so the chat, the screens and any
//! agent call it the same way (`/sheet.set cell=B2 value=42`) and every call goes through the one
//! gate: policy, approval where the risk calls for it, and the audit chain. The applications keep
//! their state in a shared handle so the screens can draw it.
#![forbid(unsafe_code)]

mod sheet;
mod studio;

use mouli_interface::{Result, Workspace};
use std::cell::RefCell;
use std::rc::Rc;

pub use sheet::{range_text, SharedBook, SheetApp};
pub use studio::{SharedDoc, StudioApp};

/// The state of the applications, shared with the screens.
#[derive(Clone)]
pub struct Apps {
    /// The workbook the Sheet application edits.
    pub book: SharedBook,
    /// The document the Studio application edits.
    pub doc: SharedDoc,
}

/// Creates the applications and registers them with `ws`.
///
/// # Errors
/// The registry's error when a capability name is taken.
pub fn register(ws: &mut Workspace) -> Result<Apps> {
    let apps = Apps {
        book: Rc::new(RefCell::new(mouli_sheet::Workbook::new())),
        doc: Rc::new(RefCell::new(mouli_studio::Document::default())),
    };
    ws.register_app(Box::new(SheetApp::new(apps.book.clone())))?;
    ws.register_app(Box::new(StudioApp::new(apps.doc.clone())))?;
    Ok(apps)
}
