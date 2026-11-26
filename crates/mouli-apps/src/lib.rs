//! Applications as capabilities.
//!
//! Each adapter implements the interface's `Application` trait, so the chat, the screens and any
//! agent call it the same way (`/sheet.set cell=B2 value=42`) and every call goes through the one
//! gate: policy, approval where the risk calls for it, and the audit chain. The applications keep
//! their state in a shared handle so the screens can draw it.
#![forbid(unsafe_code)]

mod code;
#[cfg(feature = "office")]
mod crm;
#[cfg(feature = "office")]
mod player;
#[cfg(feature = "office")]
mod sheet;
#[cfg(feature = "office")]
mod store;
#[cfg(feature = "office")]
mod studio;

use mouli_interface::{Result, Workspace};
#[cfg(feature = "office")]
use std::cell::RefCell;
#[cfg(feature = "office")]
use std::rc::Rc;

pub use code::{ChangedFile, CodeApp, CodeState, PatchError, SharedCode, Stage, View, NOT_ALLOWED};
#[cfg(feature = "office")]
pub use crm::{Clock, CrmApp, SharedCrm};
#[cfg(feature = "office")]
pub use player::{PlayerApp, SharedPlayer};
#[cfg(feature = "office")]
pub use sheet::{range_text, SharedBook, SheetApp};
#[cfg(feature = "office")]
pub use store::{parse_workbook, workbook_text};
#[cfg(feature = "office")]
pub use studio::{SharedDoc, StudioApp};

/// The state of the applications, shared with the screens.
#[cfg(feature = "office")]
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
#[cfg(feature = "office")]
pub fn register(ws: &mut Workspace) -> Result<Apps> {
    let apps = Apps {
        book: Rc::new(RefCell::new(mouli_sheet::Workbook::new())),
        doc: Rc::new(RefCell::new(mouli_studio::Document::default())),
    };
    ws.register_app(Box::new(SheetApp::new(apps.book.clone())))?;
    ws.register_app(Box::new(StudioApp::new(apps.doc.clone())))?;
    Ok(apps)
}

/// Creates the CRM for `tenant` and registers it with `ws`. `clock` gives the time in unix
/// seconds (the system clock on a device, a fixed value in tests).
///
/// # Errors
/// The registry's error when a capability name is taken.
#[cfg(feature = "office")]
pub fn register_crm(ws: &mut Workspace, tenant: &str, clock: Clock) -> Result<SharedCrm> {
    let crm = CrmApp::new_crm();
    ws.register_app(Box::new(CrmApp::new(crm.clone(), tenant, clock)))?;
    Ok(crm)
}

/// Creates the Code application and registers it with `ws`. Choose the project folder with
/// [`CodeState::open`] on the returned handle.
///
/// # Errors
/// The registry's error when a capability name is taken.
pub fn register_code(ws: &mut Workspace) -> Result<SharedCode> {
    let state = CodeState::shared();
    ws.register_app(Box::new(CodeApp::new(state.clone())))?;
    Ok(state)
}

/// Creates the player application over `backend` and registers it with `ws`.
///
/// # Errors
/// The registry's error when a capability name is taken.
#[cfg(feature = "office")]
pub fn register_player(
    ws: &mut Workspace,
    backend: Box<dyn mouli_player::Backend>,
) -> Result<SharedPlayer> {
    let player = PlayerApp::new_player(backend);
    ws.register_app(Box::new(PlayerApp::new(player.clone())))?;
    Ok(player)
}
