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

mod keep;
mod patch;
mod roles;
mod security;

#[cfg(feature = "office")]
use mouli_apps::Apps;
use mouli_interface::routing::Route;
use mouli_interface::{Reply, Workspace};
use mouli_screen::key::Key;
use mouli_screen::screen::Action;
use mouli_screen::screens::{ApprovalRow, AuditRow, Chain, Role};
use mouli_screen::shell::Shell;

pub use keep::{restore, Restored};
pub use patch::PatchError;
pub use roles::{add_default_roles, CLERK, OWNER, VIEWER};

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
    /// Restart.
    Reboot,
    /// A request only the host can carry out (loading a model, Wi-Fi, hardware report...).
    Host(Action),
}

/// The principal and role the model's tool calls run as.
pub const AGENT_USER: &str = "agent:chat";
/// The workspace role the host must define for [`Device::run_agent`] to work.
pub const AGENT_ROLE: &str = "agent";

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
    keeper: Option<keep::Keeper>,
    #[cfg(feature = "office")]
    apps: Option<Apps>,
    #[cfg(feature = "office")]
    store: Option<AppStore>,
    #[cfg(feature = "office")]
    player: Option<mouli_apps::SharedPlayer>,
    agent: Option<String>,
}

#[cfg(feature = "office")]
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
            keeper: None,
            #[cfg(feature = "office")]
            apps: None,
            #[cfg(feature = "office")]
            store: None,
            #[cfg(feature = "office")]
            player: None,
            agent: None,
        };
        d.refresh();
        Ok(d)
    }

    #[cfg(feature = "office")]
    /// Connects the applications whose capabilities are registered in the workspace, so the Sheet
    /// and Studio screens show their real state.
    pub fn attach_apps(&mut self, apps: Apps) {
        self.apps = Some(apps);
        self.refresh();
    }

    #[cfg(feature = "office")]
    /// Connects the player registered in the workspace (see `mouli_apps::register_player`), so the
    /// Player screen shows its real state and its keys control it through the governed gate.
    pub fn attach_player(&mut self, player: mouli_apps::SharedPlayer) {
        self.player = Some(player);
        self.refresh();
    }

    #[cfg(feature = "office")]
    /// Passes on what the player's backend reported (the video ended, an error, the position ...).
    /// The host calls this as events arrive from mpv or from the YouTube page.
    pub fn player_event(&mut self, event: mouli_player::Event) {
        if let Some(p) = &self.player {
            p.borrow_mut().on_event(event);
        }
        self.refresh();
    }

    /// Connects the Code application registered in the workspace (see `mouli_apps::register_code`).
    /// From then on applying, undoing and rejecting a change on the review screen are capability
    /// calls: applying and undoing need a second person's approval.
    pub fn attach_code(&mut self, state: mouli_apps::SharedCode) {
        self.patch = patch::PatchSlot::with(state);
        self.refresh();
    }

    /// Opens a project folder for the Code application and lists its files in the editor.
    ///
    /// # Errors
    /// [`PatchError`] when the folder cannot be opened.
    pub fn open_project(&mut self, root: &std::path::Path) -> Result<(), PatchError> {
        self.patch.state.borrow_mut().open(root)?;
        self.call("code.files", Vec::new());
        self.refresh();
        Ok(())
    }

    #[cfg(feature = "office")]
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

    #[cfg(feature = "office")]
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

    /// Signs another person in on this device (for example a second person who must approve what
    /// the first asked for). The conversation screen is cleared, a new conversation is opened
    /// for them, and the sign-in is written to the audit chain under their name.
    ///
    /// # Errors
    /// A message for a blank name or an unknown role; nothing changes then.
    pub fn switch_user(&mut self, user: &str, role: &str) -> Result<(), String> {
        let id = self
            .ws
            .open_session(&self.tenant, user.trim(), role)
            .map_err(|e| e.to_string())?;
        let previous = std::mem::replace(&mut self.user, user.trim().to_string());
        self.role = role.to_string();
        self.session = id;
        self.shell.chat.clear();
        self.audit_event("signin", &format!("{role} (was {previous})"), "ok");
        self.refresh();
        Ok(())
    }

    /// The id of the conversation the device is working in.
    pub fn session(&self) -> &str {
        &self.session
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
            Action::Reboot => Outcome::Reboot,
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
            Action::SwitchUser => {
                self.notice(&format!(
                    "Signed in as {} ({}). To work as someone else type: /signin <name> <role>   (roles: {})",
                    self.user,
                    self.role,
                    self.ws.role_names().join(", ")
                ));
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
            | Action::RescanStorage
            | Action::Navigate(_)
            | Action::DeleteStorage(_)
            | Action::Wifi
            | Action::Hosted
            | Action::HardwareInfo) => Outcome::Host(other),
        }
    }

    fn notice(&mut self, text: &str) {
        self.shell.chat.push(Role::Notice, text);
    }

    /// Runs a tool call made by the model in Chat, and returns the text the model should see.
    ///
    /// The model acts as its own principal, `agent:chat`, with the workspace role `agent`, never as
    /// the person: the same policy, approvals and audit apply, and a change that needs approval is
    /// held for the *person* to decide (four-eyes would refuse the person approving their own
    /// request). What happened is also shown in the conversation.
    pub fn run_agent(&mut self, command: &str) -> String {
        let session = match self.agent.clone() {
            Some(s) => s,
            None => match self.ws.open_session(&self.tenant, AGENT_USER, AGENT_ROLE) {
                Ok(s) => {
                    self.agent = Some(s.clone());
                    s
                }
                Err(e) => {
                    let text = format!("The agent cannot act: {e}");
                    self.notice(&text);
                    return text;
                }
            },
        };
        let shown: String = command.chars().take(160).collect();
        self.notice(&format!("Agent: {shown}"));
        let text = match self.ws.submit(&self.tenant, &session, command) {
            Ok(Reply::Done(out)) => {
                self.shell.chat.push(Role::Tool, &out.text);
                out.text
            }
            Ok(Reply::AwaitingApproval { id }) => {
                let text = format!(
                    "The change is held for the person's approval ({id}); it has not been made. Tell them to review it under F3."
                );
                self.notice(&format!(
                    "The agent's request is waiting for your approval ({id}). Press F3 to review it."
                ));
                text
            }
            Ok(Reply::Refused { by }) => {
                let text = format!("Refused by {by}.");
                self.notice(&text);
                text
            }
            Ok(Reply::Chat { .. }) => {
                let text = "That is not a tool.".to_string();
                self.notice(&text);
                text
            }
            Err(e) => {
                let text = e.to_string();
                self.notice(&text);
                text
            }
        };
        self.refresh();
        text
    }

    fn send(&mut self, text: &str) {
        if let Some(rest) = text.strip_prefix("/signin") {
            let mut w = rest.split_whitespace();
            match (w.next(), w.next(), w.next()) {
                (Some(name), Some(role), None) => match self.switch_user(name, role) {
                    Ok(()) => self.notice(&format!("Signed in as {name} ({role}).")),
                    Err(e) => self.notice(&format!("Could not sign in: {e}")),
                },
                _ => self.notice("Usage: /signin <name> <role>"),
            }
            return;
        }
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
            Some("code") if matches!(capability, "code.files" | "code.read" | "code.edit") => {
                self.shell.editor.message = text.to_string();
            }
            Some("code") => self.shell.review.message = text.to_string(),
            Some("sheet") => self.shell.sheet.message = text.to_string(),
            Some("studio") => self.shell.studio.message = text.to_string(),
            Some("player") => self.shell.player.message = text.to_string(),
            _ => self.notice(text),
        }
    }

    /// Runs a capability a screen asked for, through the same gate as the chat.
    fn call(&mut self, capability: &str, args: Vec<(String, String)>) {
        #[cfg(feature = "office")]
        if capability.starts_with("player.") && self.player.is_none() {
            self.app_message(capability, "The player is not connected.");
            return;
        }
        if capability.starts_with("code.") && !self.patch.gated {
            self.app_message(capability, "The Code application is not connected.");
            return;
        }
        let args: std::collections::BTreeMap<String, String> = args.into_iter().collect();
        let path = args.get("path").cloned().unwrap_or_default();
        match self.ws.call(&self.tenant, &self.session, capability, args) {
            Ok(Reply::Done(out)) => {
                self.app_message(capability, "");
                self.deliver(capability, &path, &out.text);
            }
            Ok(Reply::AwaitingApproval { id }) => self.app_message(
                capability,
                &format!("Held for approval ({id}). Someone else must approve it under Approvals."),
            ),
            Ok(Reply::Refused { by }) => self.app_message(capability, &format!("Refused by {by}.")),
            Ok(Reply::Chat { .. }) => {}
            Err(e) => self.app_message(capability, &e.to_string()),
        }
    }

    /// Hands what a capability returned to the screen that asked for it.
    fn deliver(&mut self, capability: &str, path: &str, text: &str) {
        match capability {
            // Controls such as play, pause and seek show their result in the screen itself; only
            // the actions with something to say (a title was added, an item taken out) get a line.
            "player.add" | "player.remove" | "player.rate" | "player.clear" | "player.stop" => {
                self.shell.player.message = text.to_string();
            }
            cap if cap.starts_with("player.") => self.shell.player.message.clear(),
            "code.files" => {
                let files = text.lines().map(str::to_string).collect();
                self.shell.editor.set_files(files);
            }
            "code.read" => {
                if let Err(m) = self.shell.editor.load(path, text) {
                    self.shell.editor.message = m;
                }
            }
            "code.edit" => {
                self.shell.editor.mark_saved();
                self.shell.editor.message =
                    "Sent for review. Approve it under F6 to write the file.".into();
            }
            _ => {}
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

    /// Keeps the audit chain (append-only, synced) and the held approvals in `dir`, and writes
    /// everything that is new at once. Load the saved state first with [`restore`].
    ///
    /// # Errors
    /// A message when the directory or an existing file cannot be read.
    pub fn keep_state_in(&mut self, dir: &std::path::Path) -> Result<(), String> {
        self.keeper = Some(keep::Keeper::open(dir)?);
        self.refresh();
        Ok(())
    }

    /// Rebuilds the approvals, audit and security screens from the workspace.
    pub fn refresh(&mut self) {
        if let Some(k) = &mut self.keeper {
            if let Err(e) = k.save(&self.ws) {
                self.shell.approvals.message = format!("Could not save the audit log: {e}");
            }
        }
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
        #[cfg(feature = "office")]
        {
            self.save_apps();
            self.refresh_apps();
            self.refresh_player();
        }
        patch::sync(self);
    }

    #[cfg(feature = "office")]
    /// Copies the player's state into its screen.
    fn refresh_player(&mut self) {
        use mouli_screen::screens::{PlayerRow, PlayerView};
        let Some(p) = &self.player else { return };
        let s = p.borrow().snapshot();
        self.shell.player.set_view(PlayerView {
            state: s.state.to_string(),
            failure: s.failure,
            now: s.now,
            position: s.position,
            duration: s.duration,
            volume: s.volume,
            muted: s.muted,
            speed: s.speed,
            autoplay: s.autoplay,
            shorts: s.shorts,
            shuffle: s.shuffle,
            repeat: s.repeat.to_string(),
            queue: s
                .queue
                .into_iter()
                .map(|r| PlayerRow {
                    title: r.title,
                    kind: r.kind.to_string(),
                    current: r.current,
                })
                .collect(),
        });
    }

    #[cfg(feature = "office")]
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
