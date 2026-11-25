//! The device controller.
//!
//! The screens only show data and report what the person chose ([`Action`]). This crate carries
//! those choices out through the governed workspace, so every message, capability call,
//! approval and code change goes through one gate and lands in one tamper-evident audit chain,
//! and then refreshes the screens from that real state.
//!
//! The model itself stays behind the [`Backend`] trait: the controller picks the route and
//! washes the text; the backend only turns a prompt into an answer.
#![forbid(unsafe_code)]

mod patch;
mod security;

use mouli_apps::Apps;
use mouli_interface::routing::Route;
use mouli_interface::{Reply, Workspace};
use mouli_screen::key::Key;
use mouli_screen::screen::Action;
use mouli_screen::screens::{ApprovalRow, AuditRow, Chain, Role};
use mouli_screen::shell::Shell;

pub use patch::PatchError;

/// Something that can answer a prompt with the model the workspace chose.
pub trait Backend {
    /// Runs `prompt` on `route`.
    ///
    /// # Errors
    /// A message for the person when the model could not answer.
    fn complete(&mut self, route: &Route, prompt: &str) -> Result<String, String>;
}

/// What the caller (the program that owns the hardware) still has to do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing more; the screens are up to date.
    Handled,
    /// Power off.
    Quit,
    /// A request only the host can carry out (loading a model, Wi-Fi, hardware report...).
    Host(Action),
}

/// The screens, the workspace and the signed-in person.
pub struct Device<B: Backend> {
    /// The interface to draw and feed keys to.
    pub shell: Shell,
    ws: Workspace,
    backend: B,
    tenant: String,
    user: String,
    role: String,
    session: String,
    patch: patch::PatchSlot,
    apps: Option<Apps>,
    store: Option<AppStore>,
}

/// Where the applications are saved, and what was last written there.
struct AppStore {
    dir: std::path::PathBuf,
    saved: (String, String),
}

impl<B: Backend> Device<B> {
    /// Signs `user` in (acting as `role`) and opens a conversation.
    ///
    /// # Errors
    /// The workspace's error when the role is unknown or a name is blank.
    pub fn new(
        mut ws: Workspace,
        backend: B,
        tenant: &str,
        user: &str,
        role: &str,
    ) -> mouli_interface::Result<Self> {
        let session = ws.open_session(tenant, user, role)?;
        let mut d = Device {
            shell: Shell::new(),
            ws,
            backend,
            tenant: tenant.into(),
            user: user.into(),
            role: role.into(),
            session,
            patch: patch::PatchSlot::default(),
            apps: None,
            store: None,
        };
        d.refresh();
        Ok(d)
    }

    /// Connects the applications whose capabilities are registered in the workspace, so the Sheet
    /// and Studio screens show their real state.
    pub fn attach_apps(&mut self, apps: Apps) {
        self.apps = Some(apps);
        self.refresh();
    }

    /// Connects the Code application registered in the workspace (see `mouli_apps::register_code`).
    /// From then on applying, undoing and rejecting a change on the review screen are capability
    /// calls: applying and undoing need a second person's approval.
    pub fn attach_code(&mut self, state: mouli_apps::SharedCode) {
        self.patch = patch::PatchSlot::with(state);
        self.refresh();
    }

    /// Keeps the applications' data in `dir`: loads what is there now, then saves after every
    /// change. Call after [`Device::attach_apps`].
    ///
    /// # Errors
    /// A message when no applications are attached, or a saved file is damaged (nothing is
    /// changed and nothing will be overwritten until it is fixed or removed).
    pub fn keep_apps_in(&mut self, dir: &std::path::Path) -> Result<(), String> {
        let apps = self
            .apps
            .as_ref()
            .ok_or("no applications are attached")?
            .clone();
        apps.load(dir)?;
        let saved = (
            mouli_apps::workbook_text(&apps.book.borrow()),
            apps.doc.borrow().to_container(),
        );
        self.store = Some(AppStore {
            dir: dir.to_path_buf(),
            saved,
        });
        self.refresh();
        Ok(())
    }

    /// Writes the applications' data if it changed since the last save.
    fn save_apps(&mut self) {
        let (Some(apps), Some(store)) = (&self.apps, &mut self.store) else {
            return;
        };
        let now = (
            mouli_apps::workbook_text(&apps.book.borrow()),
            apps.doc.borrow().to_container(),
        );
        if now == store.saved {
            return;
        }
        match apps.save(&store.dir) {
            Ok(()) => store.saved = now,
            Err(e) => {
                let msg = format!("Could not save: {e}");
                self.shell.sheet.message = msg.clone();
                self.shell.studio.message = msg;
            }
        }
    }

    /// The workspace (read-only), for status lines and tests.
    pub fn workspace(&self) -> &Workspace {
        &self.ws
    }

    /// The signed-in user.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Feeds one key to the interface and carries out whatever it asked for.
    pub fn on_key(&mut self, key: Key) -> Outcome {
        let action = self.shell.on_key(key);
        self.run(action)
    }

    /// Carries out one action directly, as if a screen had asked for it. The controller makes
    /// its own checks (for example before applying a patch), so it does not rely on the screen
    /// having refused first.
    pub fn run(&mut self, action: Action) -> Outcome {
        let outcome = self.perform(action);
        self.refresh();
        outcome
    }

    fn perform(&mut self, action: Action) -> Outcome {
        match action {
            Action::None | Action::Show(_) => Outcome::Handled,
            Action::Quit => Outcome::Quit,
            Action::Send(text) => {
                self.send(&text);
                Outcome::Handled
            }
            Action::NewConversation => {
                match self.ws.open_session(&self.tenant, &self.user, &self.role) {
                    Ok(id) => {
                        self.session = id;
                        self.shell.chat.clear();
                    }
                    Err(e) => self.notice(&e.to_string()),
                }
                Outcome::Handled
            }
            Action::Decide { id, approve } => {
                self.decide(&id, approve);
                Outcome::Handled
            }
            Action::RefreshSecurity => Outcome::Handled, // refresh() below recomputes it
            Action::Call { capability, args } => {
                self.call(&capability, args);
                Outcome::Handled
            }
            Action::ApplyPatch => {
                self.apply_patch();
                Outcome::Handled
            }
            Action::RollbackPatch => {
                self.rollback_patch();
                Outcome::Handled
            }
            Action::RejectPatch => {
                self.reject_patch();
                Outcome::Handled
            }
            other @ (Action::ChooseModel(_)
            | Action::RescanModels
            | Action::Wifi
            | Action::Hosted
            | Action::HardwareInfo
            | Action::SwitchUser) => Outcome::Host(other),
        }
    }

    fn notice(&mut self, text: &str) {
        self.shell.chat.push(Role::Notice, text);
    }

    fn send(&mut self, text: &str) {
        self.shell.chat.push(Role::User, text);
        match self.ws.submit(&self.tenant, &self.session, text) {
            Ok(Reply::Chat {
                route,
                prompt,
                washed,
            }) => match self.backend.complete(&route, &prompt) {
                Ok(answer) => {
                    if let Err(e) = self.ws.record_reply(&self.tenant, &self.session, &answer) {
                        self.notice(&format!("The answer could not be kept: {e}"));
                    }
                    self.shell.chat.push(Role::Assistant, &answer);
                    if washed {
                        self.notice("Private details were removed before this left the machine.");
                    }
                }
                Err(e) => {
                    self.ws
                        .record_event(&self.user, "chat", &route.model, &format!("failed: {e}"));
                    self.notice(&format!("The model could not answer: {e}"));
                }
            },
            Ok(Reply::Done(out)) => self.shell.chat.push(Role::Tool, &out.text),
            Ok(Reply::AwaitingApproval { id }) => self.notice(&format!(
                "Held for approval ({id}). Someone else must approve it under F3."
            )),
            Ok(Reply::Refused { by }) => self.notice(&format!("Refused by {by}.")),
            Err(e) => self.notice(&e.to_string()),
        }
    }

    /// Shows `text` on the screen of the application that owns `capability` (or in the chat).
    fn app_message(&mut self, capability: &str, text: &str) {
        match capability.split('.').next() {
            Some("code") => self.shell.review.message = text.to_string(),
            Some("sheet") => self.shell.sheet.message = text.to_string(),
            Some("studio") => self.shell.studio.message = text.to_string(),
            _ => self.notice(text),
        }
    }

    /// Runs a capability a screen asked for, through the same gate as the chat.
    fn call(&mut self, capability: &str, args: Vec<(String, String)>) {
        let args = args.into_iter().collect();
        match self.ws.call(&self.tenant, &self.session, capability, args) {
            Ok(Reply::Done(_)) => self.app_message(capability, ""),
            Ok(Reply::AwaitingApproval { id }) => self.app_message(
                capability,
                &format!("Held for approval ({id}). Someone else must approve it under Approvals."),
            ),
            Ok(Reply::Refused { by }) => self.app_message(capability, &format!("Refused by {by}.")),
            Ok(Reply::Chat { .. }) => {}
            Err(e) => self.app_message(capability, &e.to_string()),
        }
    }

    /// Runs a Code capability for the review screen and shows the outcome there.
    pub(crate) fn call_patch(&mut self, capability: &str) {
        self.call(capability, Vec::new());
    }

    fn decide(&mut self, id: &str, approve: bool) {
        match self
            .ws
            .decide(&self.tenant, id, &self.user, &self.role, approve)
        {
            Ok(Reply::Done(out)) => {
                self.shell.chat.push(Role::Tool, &out.text);
                self.shell.approvals.message = format!("{id} approved and run.");
            }
            Ok(Reply::Refused { .. }) => self.shell.approvals.message = format!("{id} refused."),
            Ok(_) => self.shell.approvals.message.clear(),
            Err(e) => self.shell.approvals.message = e.to_string(),
        }
    }

    /// Rebuilds the approvals, audit and security screens from the workspace.
    pub fn refresh(&mut self) {
        let can_approve = self.ws.role_can(&self.role, "approve");
        let rows: Vec<ApprovalRow> = self
            .ws
            .pending_approvals()
            .into_iter()
            .map(|(id, action, detail)| {
                let requester = self.ws.approval_requester(id).unwrap_or("?").to_string();
                let own = requester == self.user;
                let (can_decide, why_not) = if own {
                    (
                        false,
                        "You asked for this yourself; someone else must decide.".to_string(),
                    )
                } else if !can_approve {
                    (
                        false,
                        format!("Your role ({}) cannot approve requests.", self.role),
                    )
                } else {
                    (true, String::new())
                };
                ApprovalRow {
                    id: id.to_string(),
                    capability: action.to_string(),
                    requester,
                    detail: detail.to_string(),
                    can_decide,
                    why_not,
                }
            })
            .collect();
        self.shell.approvals.set_rows(rows);

        let audit = self.ws.audit();
        let chain = match audit.verify() {
            Ok(()) => Chain::Verified,
            Err(i) => Chain::Broken(i as u64 + 1),
        };
        let records: Vec<AuditRow> = audit
            .records()
            .iter()
            .map(|r| AuditRow {
                seq: r.seq + 1,
                time: format!("t{}", r.tick),
                actor: r.actor.clone(),
                action: r.action.clone(),
                target: r.subject.clone(),
                outcome: r.outcome.clone(),
                hash: r.hash.clone(),
            })
            .collect();
        self.shell.audit.set(records, chain);

        let (score, findings) = security::assess(&self.ws);
        self.shell.security.set(score, findings);
        self.save_apps();
        self.refresh_apps();
        patch::sync(self);
    }

    /// Copies the applications' state into their screens.
    fn refresh_apps(&mut self) {
        use mouli_screen::screens::sheet::{MAX_COLS, MAX_ROWS};
        use mouli_screen::screens::{SheetCell, StudioBlock};
        let Some(apps) = &self.apps else { return };
        {
            let book = apps.book.borrow();
            if let Some(sheet) = book.sheets.get(book.active) {
                let cells = sheet
                    .cells
                    .iter()
                    .filter(|((r, c), _)| *r < MAX_ROWS && *c < MAX_COLS)
                    .take(20_000)
                    .map(|(k, cell)| {
                        (
                            *k,
                            SheetCell {
                                shown: cell.value.display(),
                                raw: cell.raw.clone(),
                            },
                        )
                    })
                    .collect();
                self.shell.sheet.set_cells(&sheet.name, cells);
            }
        }
        let doc = apps.doc.borrow();
        self.shell.studio.blocks = doc
            .blocks
            .iter()
            .map(|b| match b {
                mouli_studio::Block::Heading(t) => StudioBlock {
                    heading: true,
                    text: t.clone(),
                },
                mouli_studio::Block::Paragraph(t) => StudioBlock {
                    heading: false,
                    text: t.clone(),
                },
            })
            .collect();
    }

    /// Puts a proposed change (unified diff text) under review for the files below `root`.
    ///
    /// # Errors
    /// [`PatchError`] when the diff cannot be read or does not fit the files.
    pub fn propose_patch(&mut self, root: &std::path::Path, diff: &str) -> Result<(), PatchError> {
        patch::propose(self, root, diff)
    }

    fn apply_patch(&mut self) {
        patch::apply(self);
    }

    fn rollback_patch(&mut self) {
        patch::rollback(self);
    }

    fn reject_patch(&mut self) {
        patch::reject(self);
    }

    pub(crate) fn audit_event(&mut self, action: &str, subject: &str, outcome: &str) {
        let user = self.user.clone();
        self.ws.record_event(&user, action, subject, outcome);
    }
}
