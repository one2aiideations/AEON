//! The workspace: one object that ties sessions, policy, applications, approvals, data
//! washing, model routing, artifacts and audit together. Every application and every agent goes
//! through [`Workspace::submit`], so there is one gate and one audit trail.

use crate::approval::{ApprovalQueue, Status};
use crate::artifact::{Artifact, ArtifactStore};
use crate::audit::AuditLog;
use crate::capability::{Application, Call, Output, Registry};
use crate::error::{Error, Result};
use crate::ids::IdGen;
use crate::intent::{Intent, CHAT};
use crate::message::{Role, Session};
use crate::policy::{Policy, Role as Permissions, Verdict};
use crate::routing::{choose, ModelEntry, Needs, Route};
use crate::wash::{is_sensitive, wash, Mode, Vault};
use crate::workflow::Limits;
use std::collections::BTreeMap;

/// What the interface should do next after a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Run the chosen model on `prompt`, then call [`Workspace::record_reply`].
    Chat {
        /// Selected model.
        route: Route,
        /// Text to send (washed when it left the machine's trust boundary).
        prompt: String,
        /// Whether `prompt` differs from the user's text because of washing.
        washed: bool,
    },
    /// A capability ran.
    Done(Output),
    /// The call is held; show the request to an approver.
    AwaitingApproval {
        /// Approval id to pass to [`Workspace::decide`].
        id: String,
    },
    /// An approver refused the held call.
    Refused {
        /// Who refused.
        by: String,
    },
}

/// A held call in a form that can be saved and restored: everything needed to run it later,
/// once someone else approves, including who asked and in which role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    /// The approval id.
    pub id: String,
    /// The tenant of the requester's session.
    pub tenant: String,
    /// Who asked.
    pub user: String,
    /// The role the requester was acting in.
    pub role: String,
    /// The capability to run.
    pub capability: String,
    /// Its arguments.
    pub args: BTreeMap<String, String>,
    /// The `key=value` text shown to the approver.
    pub detail: String,
    /// When it was asked (logical time).
    pub tick: u64,
}

struct Held {
    session: String,
    call: Call,
}

/// The main interface state.
pub struct Workspace {
    ids: IdGen,
    tick: u64,
    policy: Policy,
    limits: Limits,
    roles: BTreeMap<String, Permissions>,
    sessions: BTreeMap<String, Session>,
    registry: Registry,
    approvals: ApprovalQueue,
    held: BTreeMap<String, Held>,
    artifacts: ArtifactStore,
    models: Vec<ModelEntry>,
    audit: AuditLog,
    vault: Vault,
}

impl Workspace {
    /// Create a workspace.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the limits are not positive.
    pub fn new(policy: Policy, limits: Limits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            ids: IdGen::new(),
            tick: 0,
            policy,
            limits,
            roles: BTreeMap::new(),
            sessions: BTreeMap::new(),
            registry: Registry::new(),
            approvals: ApprovalQueue::new(),
            held: BTreeMap::new(),
            artifacts: ArtifactStore::new(),
            models: Vec::new(),
            audit: AuditLog::new(),
            vault: Vault::new(),
        })
    }

    fn now(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Define a role.
    pub fn add_role(&mut self, role: Permissions) {
        self.roles.insert(role.name.clone(), role);
    }

    /// Register an application.
    ///
    /// # Errors
    /// See [`Registry::register`].
    pub fn register_app(&mut self, app: Box<dyn Application>) -> Result<()> {
        self.registry.register(app)
    }

    /// Make a model available to the router.
    pub fn add_model(&mut self, m: ModelEntry) {
        self.models.push(m);
    }

    /// Open a session for `user` acting as `role`.
    ///
    /// # Errors
    /// [`Error::NotFound`] when the role is unknown; [`Error::Invalid`] for blank names.
    pub fn open_session(&mut self, tenant: &str, user: &str, role: &str) -> Result<String> {
        if tenant.trim().is_empty() || user.trim().is_empty() {
            return Err(Error::Invalid("tenant and user are required".into()));
        }
        if !self.roles.contains_key(role) {
            return Err(Error::NotFound(format!("role {role}")));
        }
        let id = self.ids.next("ses");
        self.sessions
            .insert(id.clone(), Session::new(id.clone(), tenant, user, role));
        let t = self.now();
        self.audit.append(t, user, "session.open", &id, "ok");
        Ok(id)
    }

    /// A session, if it belongs to `tenant`. Another tenant's session looks like a missing one.
    ///
    /// # Errors
    /// [`Error::NotFound`].
    pub fn session(&self, tenant: &str, id: &str) -> Result<&Session> {
        self.sessions
            .get(id)
            .filter(|s| s.tenant == tenant)
            .ok_or_else(|| Error::NotFound(format!("session {id}")))
    }

    /// Handle one line typed by the user (or sent by an agent acting for them).
    ///
    /// # Errors
    /// Validation, limit, policy ([`Error::Denied`]) and routing errors; handler failures.
    pub fn submit(&mut self, tenant: &str, session: &str, text: &str) -> Result<Reply> {
        self.limits.check_input(text.len())?;
        let (user, role) = {
            let s = self.session(tenant, session)?;
            (s.user.clone(), s.role.clone())
        };
        let intent = Intent::parse(text)?;
        let (t, ids) = (self.now(), &mut self.ids);
        self.sessions
            .get_mut(session)
            .ok_or_else(|| Error::NotFound(format!("session {session}")))?
            .add(Role::User, text, t, ids)?;
        if intent.name == CHAT {
            return self.chat(&user, text);
        }
        let call = Call {
            capability: intent.name,
            args: intent.params,
        };
        self.run(session, &user, &role, call, None)
    }

    fn chat(&mut self, user: &str, text: &str) -> Result<Reply> {
        let mut needs = Needs::default();
        needs.caps.insert("text".into());
        let sensitive = is_sensitive(text);
        let t = self.now();
        let (route, prompt, washed) = if sensitive {
            needs.local_only = true;
            if let Ok(r) = choose(&self.models, &needs, self.policy.allow_external_models) {
                (r, text.to_string(), false)
            } else if self.policy.allow_external_models {
                needs.local_only = false;
                let r = choose(&self.models, &needs, true)?;
                (r, wash(text, Mode::Redact, &mut self.vault).text, true)
            } else {
                self.audit.append(
                    t,
                    user,
                    "chat",
                    "-",
                    "denied: sensitive text and no local model",
                );
                return Err(Error::Denied(
                    "text contains sensitive data and no local model is available".into(),
                ));
            }
        } else {
            let r = choose(&self.models, &needs, self.policy.allow_external_models)?;
            (r, text.to_string(), false)
        };
        let outcome = format!(
            "ok via {}{}",
            route.model,
            if washed { " (washed)" } else { "" }
        );
        self.audit.append(t, user, "chat", &route.model, &outcome);
        Ok(Reply::Chat {
            route,
            prompt,
            washed,
        })
    }

    fn run(
        &mut self,
        session: &str,
        user: &str,
        role: &str,
        call: Call,
        spend: Option<&str>,
    ) -> Result<Reply> {
        let t = self.now();
        let spec = match self.registry.spec(&call.capability) {
            Ok(s) => s.clone(),
            Err(e) => {
                self.audit
                    .append(t, user, &call.capability, "-", "denied: unknown capability");
                return Err(e);
            }
        };
        let perms = self
            .roles
            .get(role)
            .ok_or_else(|| Error::NotFound(format!("role {role}")))?;
        let verdict = self.policy.check(&spec.name, spec.risk, &spec.uses, perms);
        match (verdict, spend) {
            (Verdict::Deny(why), _) => {
                self.audit
                    .append(t, user, &spec.name, "-", &format!("denied: {why}"));
                return Err(Error::Denied(why));
            }
            (Verdict::NeedsApproval, None) => {
                let detail = call
                    .args
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let id = self
                    .approvals
                    .request(&spec.name, user, &detail, t, &mut self.ids);
                self.audit
                    .append(t, user, &spec.name, &id, "pending approval");
                self.held.insert(
                    id.clone(),
                    Held {
                        session: session.to_string(),
                        call,
                    },
                );
                return Ok(Reply::AwaitingApproval { id });
            }
            (Verdict::NeedsApproval, Some(id)) => self.approvals.spend(id, &spec.name)?,
            (Verdict::Allow, _) => {}
        }
        let result = self.registry.dispatch(&call).and_then(|o| {
            self.limits.check_output(o.text.len())?;
            Ok(o)
        });
        match result {
            Ok(out) => {
                let t = self.now();
                if let Some(s) = self.sessions.get_mut(session) {
                    let _ = s.add(
                        Role::Tool,
                        &format!("{}: {}", spec.name, out.text),
                        t,
                        &mut self.ids,
                    );
                }
                self.audit.append(t, user, &spec.name, "-", "ok");
                Ok(Reply::Done(out))
            }
            Err(e) => {
                self.audit
                    .append(t, user, &spec.name, "-", &format!("failed: {e}"));
                Err(e)
            }
        }
    }

    /// Decide a held call. On approval the call runs immediately, once.
    ///
    /// # Errors
    /// [`Error::Denied`] when `approver_role` lacks the `approve` permission or the approver is
    /// the requester; [`Error::NotFound`] for an unknown id or another tenant's request;
    /// [`Error::Conflict`] when already decided; handler errors when the call runs.
    pub fn decide(
        &mut self,
        tenant: &str,
        id: &str,
        approver: &str,
        approver_role: &str,
        approve: bool,
    ) -> Result<Reply> {
        let t = self.now();
        let held = self
            .held
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("approval {id}")))?;
        let (session, requester_role) = {
            let s = self
                .session(tenant, &held.session)
                .map_err(|_| Error::NotFound(format!("approval {id}")))?;
            (s.id.clone(), s.role.clone())
        };
        let can = self
            .roles
            .get(approver_role)
            .is_some_and(|r| r.can("approve"));
        if !can {
            self.audit
                .append(t, approver, "approve", id, "denied: role cannot approve");
            return Err(Error::Denied(format!(
                "role {approver_role} cannot approve"
            )));
        }
        if let Err(e) = self.approvals.decide(id, approver, approve) {
            self.audit
                .append(t, approver, "approve", id, &format!("rejected: {e}"));
            return Err(e);
        }
        let Some(h) = self.held.remove(id) else {
            return Err(Error::NotFound(format!("approval {id}")));
        };
        if !approve {
            self.audit.append(t, approver, "approve", id, "refused");
            return Ok(Reply::Refused {
                by: approver.into(),
            });
        }
        self.audit.append(t, approver, "approve", id, "approved");
        let requester = self
            .approvals
            .get(id)
            .map(|r| r.requester.clone())
            .unwrap_or_default();
        self.run(&session, &requester, &requester_role, h.call, Some(id))
    }

    /// Record the model's answer to a [`Reply::Chat`].
    ///
    /// # Errors
    /// [`Error::Limit`] when too long, [`Error::Invalid`] when blank, [`Error::NotFound`].
    pub fn record_reply(&mut self, tenant: &str, session: &str, text: &str) -> Result<()> {
        self.limits.check_output(text.len())?;
        let (t, ids) = (self.now(), &mut self.ids);
        let s = self
            .sessions
            .get_mut(session)
            .filter(|s| s.tenant == tenant)
            .ok_or_else(|| Error::NotFound(format!("session {session}")))?;
        s.add(Role::Assistant, text, t, ids)?;
        Ok(())
    }

    /// Store an artifact and audit it.
    ///
    /// # Errors
    /// See [`ArtifactStore::put`]; [`Error::NotFound`] for the session.
    pub fn put_artifact(
        &mut self,
        tenant: &str,
        session: &str,
        name: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<Artifact> {
        let user = self.session(tenant, session)?.user.clone();
        let t = self.now();
        let a = self
            .artifacts
            .put(name, media_type, bytes, t, &mut self.ids)?;
        self.audit.append(t, &user, "artifact.put", &a.id, "ok");
        Ok(a)
    }

    /// Loads a saved audit log into an empty workspace and continues its logical clock after it.
    /// The records are kept exactly as saved, even if the chain is damaged: [`Workspace::audit`]
    /// then reports the damage through `verify`.
    ///
    /// # Errors
    /// [`Error::Conflict`] when this workspace already has audit records.
    pub fn load_audit(&mut self, records: Vec<crate::audit::AuditRecord>) -> Result<()> {
        if !self.audit.records().is_empty() {
            return Err(Error::Conflict("the audit log is not empty".into()));
        }
        self.tick = self
            .tick
            .max(records.iter().map(|r| r.tick).max().unwrap_or(0));
        for r in &records {
            self.ids.observe(&r.subject);
        }
        self.audit = AuditLog::from_records(records);
        Ok(())
    }

    /// The calls waiting for approval, in a form that can be saved.
    #[must_use]
    pub fn export_pending(&self) -> Vec<PendingCall> {
        self.approvals
            .pending()
            .into_iter()
            .filter_map(|r| {
                let held = self.held.get(&r.id)?;
                let s = self.sessions.get(&held.session)?;
                Some(PendingCall {
                    id: r.id.clone(),
                    tenant: s.tenant.clone(),
                    user: r.requester.clone(),
                    role: s.role.clone(),
                    capability: held.call.capability.clone(),
                    args: held.call.args.clone(),
                    detail: r.detail.clone(),
                    tick: r.tick,
                })
            })
            .collect()
    }

    /// Puts saved calls back in the queue, still waiting. Each gets a fresh session for its
    /// requester; the original id is kept, and the restore is audited.
    ///
    /// # Errors
    /// [`Error::NotFound`] for an unknown role or capability, [`Error::Conflict`] for an id that
    /// is already queued. Calls before the failing one stay restored.
    pub fn import_pending(&mut self, calls: Vec<PendingCall>) -> Result<()> {
        for c in calls {
            if !self.roles.contains_key(&c.role) {
                return Err(Error::NotFound(format!("role {}", c.role)));
            }
            let spec = self.registry.spec(&c.capability)?.clone();
            self.approvals
                .restore(&c.id, &spec.name, &c.user, &c.detail, c.tick)?;
            self.ids.observe(&c.id);
            let session = self.ids.next("ses");
            self.sessions.insert(
                session.clone(),
                Session::new(session.clone(), &c.tenant, &c.user, &c.role),
            );
            self.held.insert(
                c.id.clone(),
                Held {
                    session,
                    call: Call {
                        capability: c.capability,
                        args: c.args,
                    },
                },
            );
            let t = self.now();
            self.audit
                .append(t, &c.user, "approval.restore", &c.id, "pending approval");
        }
        Ok(())
    }

    /// Approvals waiting for a decision.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<(&str, &str, &str)> {
        self.approvals
            .pending()
            .into_iter()
            .map(|r| (r.id.as_str(), r.action.as_str(), r.detail.as_str()))
            .collect()
    }

    /// Who asked for a held call (so a console can refuse to let them approve it).
    #[must_use]
    pub fn approval_requester(&self, id: &str) -> Option<&str> {
        self.approvals.get(id).map(|r| r.requester.as_str())
    }

    /// Whether the named role exists and holds `permission`.
    #[must_use]
    pub fn role_can(&self, role: &str, permission: &str) -> bool {
        self.roles.get(role).is_some_and(|r| r.can(permission))
    }

    /// Writes an event from a part of the system outside this workspace (for example a code
    /// change applied on the device) into the same tamper-evident audit chain.
    pub fn record_event(&mut self, actor: &str, action: &str, subject: &str, outcome: &str) {
        let t = self.now();
        self.audit.append(t, actor, action, subject, outcome);
    }

    /// Status of an approval.
    #[must_use]
    pub fn approval_status(&self, id: &str) -> Option<&Status> {
        self.approvals.get(id).map(|r| r.status())
    }

    /// Artifact store.
    #[must_use]
    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    /// Audit log.
    #[must_use]
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// Registered capabilities, sorted by name, as `(name, description)`.
    #[must_use]
    pub fn help(&self) -> Vec<(&str, &str)> {
        self.registry
            .specs()
            .into_iter()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::testapp::Notes;

    const T: &str = "acme";

    fn ws(external: bool) -> (Workspace, String) {
        let policy = Policy {
            allow_external_models: external,
            ..Policy::default()
        };
        let mut w = Workspace::new(policy, Limits::default()).unwrap();
        w.add_role(Permissions::new("member", &["note.*"]));
        w.add_role(Permissions::new("manager", &["note.*", "approve"]));
        w.add_role(Permissions::new("guest", &["note.read"]));
        w.register_app(Box::new(Notes { sent: vec![] })).unwrap();
        w.add_model(ModelEntry::new(
            "local-1",
            "local",
            true,
            &["text"],
            4096,
            0,
            50,
        ));
        let s = w.open_session(T, "ann", "member").unwrap();
        (w, s)
    }

    #[test]
    fn read_runs_and_is_audited() {
        let (mut w, s) = ws(false);
        let r = w.submit(T, &s, "/note.read").unwrap();
        assert_eq!(
            r,
            Reply::Done(Output {
                text: "ran note.read".into(),
                artifacts: vec![]
            })
        );
        assert_eq!(w.session(T, &s).unwrap().messages().len(), 2);
        assert!(w
            .audit()
            .records()
            .iter()
            .any(|a| a.action == "note.read" && a.outcome == "ok"));
        assert!(w.audit().verify().is_ok());
    }

    #[test]
    fn consequential_call_waits_for_a_different_approver() {
        let (mut w, s) = ws(false);
        let Reply::AwaitingApproval { id } = w.submit(T, &s, "/note.send to=bob@x.io").unwrap()
        else {
            panic!("expected hold");
        };
        assert_eq!(w.pending_approvals().len(), 1);
        assert!(
            matches!(
                w.decide(T, &id, "ann", "member", true),
                Err(Error::Denied(_))
            ),
            "role cannot approve"
        );
        assert!(
            matches!(
                w.decide(T, &id, "ann", "manager", true),
                Err(Error::Denied(_))
            ),
            "requester cannot approve"
        );
        let r = w.decide(T, &id, "mia", "manager", true).unwrap();
        assert!(matches!(r, Reply::Done(o) if o.text == "sent to bob@x.io"));
        assert!(
            matches!(
                w.decide(T, &id, "mia", "manager", true),
                Err(Error::NotFound(_))
            ),
            "single use"
        );
        assert!(w.pending_approvals().is_empty());
        assert!(w.audit().verify().is_ok());
    }

    #[test]
    fn refused_call_never_runs() {
        let (mut w, s) = ws(false);
        let Reply::AwaitingApproval { id } = w.submit(T, &s, "/note.send to=bob@x.io").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            w.decide(T, &id, "mia", "manager", false).unwrap(),
            Reply::Refused { by: "mia".into() }
        );
        assert_eq!(w.approval_status(&id), Some(&Status::Denied("mia".into())));
        assert!(!w
            .session(T, &s)
            .unwrap()
            .messages()
            .iter()
            .any(|m| m.content.contains("sent to")));
    }

    #[test]
    fn roles_and_policy_deny() {
        let (mut w, _) = ws(false);
        let g = w.open_session(T, "gus", "guest").unwrap();
        assert!(matches!(
            w.submit(T, &g, "/note.write text=x"),
            Err(Error::Denied(_))
        ));
        assert!(w.submit(T, &g, "/note.read").is_ok());
        let (mut w2, s2) = ws(false);
        assert!(
            matches!(w2.submit(T, &s2, "/note.cloud"), Err(Error::Denied(_))),
            "external model forbidden"
        );
        assert!(matches!(
            w2.submit(T, &s2, "/nope.x"),
            Err(Error::NotFound(_))
        ));
        assert!(
            matches!(w2.submit(T, &s2, "/note.write"), Err(Error::Invalid(_))),
            "missing arg"
        );
        let (mut w3, s3) = ws(true);
        assert!(w3.submit(T, &s3, "/note.cloud").is_ok());
    }

    #[test]
    fn tenants_are_isolated() {
        let (mut w, s) = ws(false);
        assert!(matches!(
            w.submit("other", &s, "hello"),
            Err(Error::NotFound(_))
        ));
        let Reply::AwaitingApproval { id } = w.submit(T, &s, "/note.send to=a@b.io").unwrap()
        else {
            panic!()
        };
        assert!(matches!(
            w.decide("other", &id, "mia", "manager", true),
            Err(Error::NotFound(_))
        ));
        assert!(w.session("other", &s).is_err());
    }

    #[test]
    fn chat_routes_and_records() {
        let (mut w, s) = ws(false);
        let Reply::Chat {
            route,
            prompt,
            washed,
        } = w.submit(T, &s, "hello there").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (route.model.as_str(), prompt.as_str(), washed),
            ("local-1", "hello there", false)
        );
        w.record_reply(T, &s, "hi!").unwrap();
        assert_eq!(w.session(T, &s).unwrap().messages().len(), 2);
    }

    #[test]
    fn sensitive_chat_stays_local_or_is_washed() {
        let (mut w, s) = ws(false);
        let Reply::Chat {
            route,
            prompt,
            washed,
        } = w.submit(T, &s, "my password=hunter2 help").unwrap()
        else {
            panic!()
        };
        assert!(route.local && prompt.contains("hunter2") && !washed);

        let policy = Policy {
            allow_external_models: true,
            ..Policy::default()
        };
        let mut w2 = Workspace::new(policy, Limits::default()).unwrap();
        w2.add_role(Permissions::new("member", &["*"]));
        w2.add_model(ModelEntry::new(
            "cloud",
            "cloud",
            false,
            &["text"],
            8192,
            100,
            90,
        ));
        let s2 = w2.open_session(T, "ann", "member").unwrap();
        let Reply::Chat {
            route,
            prompt,
            washed,
        } = w2.submit(T, &s2, "my password=hunter2 help").unwrap()
        else {
            panic!()
        };
        assert!(!route.local && washed);
        assert!(!prompt.contains("hunter2") && prompt.contains("[REDACTED:secret]"));

        let policy = Policy::default();
        let mut w3 = Workspace::new(policy, Limits::default()).unwrap();
        w3.add_role(Permissions::new("member", &["*"]));
        w3.add_model(ModelEntry::new(
            "cloud",
            "cloud",
            false,
            &["text"],
            8192,
            100,
            90,
        ));
        let s3 = w3.open_session(T, "ann", "member").unwrap();
        assert!(
            w3.submit(T, &s3, "hello").is_err(),
            "no local model and external not allowed"
        );
    }

    #[test]
    fn limits_are_enforced() {
        let mut w = Workspace::new(
            Policy::default(),
            Limits {
                max_input_bytes: 8,
                max_output_bytes: 4,
                max_steps: 1,
            },
        )
        .unwrap();
        w.add_role(Permissions::new("m", &["*"]));
        w.add_model(ModelEntry::new("l", "local", true, &["text"], 10, 0, 1));
        let s = w.open_session(T, "ann", "m").unwrap();
        assert!(matches!(
            w.submit(T, &s, "far too long input"),
            Err(Error::Limit(_))
        ));
        assert!(w.submit(T, &s, "hi").is_ok());
        assert!(matches!(
            w.record_reply(T, &s, "too long"),
            Err(Error::Limit(_))
        ));
        assert!(Workspace::new(
            Policy::default(),
            Limits {
                max_steps: 0,
                ..Limits::default()
            }
        )
        .is_err());
    }

    #[test]
    fn artifacts_and_help() {
        let (mut w, s) = ws(false);
        let a = w
            .put_artifact(T, &s, "plan.md", "text/markdown", b"# plan")
            .unwrap();
        assert_eq!(w.artifacts().get(&a.id).unwrap().1, b"# plan");
        assert!(w.open_session(T, "x", "nope").is_err());
        let h = w.help();
        assert_eq!(h.len(), 5);
        assert_eq!(h[0].0, "note.cloud");
    }

    #[test]
    fn failing_handler_is_audited() {
        let (mut w, s) = ws(false);
        assert!(matches!(
            w.submit(T, &s, "/note.fail"),
            Err(Error::Handler(_))
        ));
        assert!(w
            .audit()
            .records()
            .last()
            .unwrap()
            .outcome
            .starts_with("failed"));
    }

    /// Builds the same workspace again, as a restart would.
    fn restarted() -> Workspace {
        let (w, _s) = ws(false);
        drop(w);
        let mut w = Workspace::new(Policy::default(), Limits::default()).unwrap();
        w.add_role(Permissions::new("member", &["note.*"]));
        w.add_role(Permissions::new("manager", &["note.*", "approve"]));
        w.register_app(Box::new(Notes { sent: vec![] })).unwrap();
        w
    }

    #[test]
    fn a_held_call_and_the_audit_chain_survive_a_restart() {
        let (mut before, s) = ws(false);
        let held = before.submit(T, &s, "/note.send to=bob").unwrap();
        let Reply::AwaitingApproval { id } = held else {
            panic!("expected a held call")
        };
        let saved_audit = before.audit().records().to_vec();
        let saved_pending = before.export_pending();
        assert_eq!(saved_pending.len(), 1);
        assert_eq!(saved_pending[0].user, "ann");
        assert_eq!(saved_pending[0].args["to"], "bob");

        let mut after = restarted();
        after.load_audit(saved_audit.clone()).unwrap();
        after.import_pending(saved_pending).unwrap();
        assert_eq!(after.audit().verify(), Ok(()));
        assert_eq!(after.pending_approvals().len(), 1);
        assert_eq!(after.approval_requester(&id), Some("ann"));

        // The requester still cannot approve it; a manager can, and it runs once.
        assert!(after.decide(T, &id, "ann", "member", true).is_err());
        let done = after.decide(T, &id, "mia", "manager", true).unwrap();
        assert!(matches!(done, Reply::Done(_)));
        assert!(after.pending_approvals().is_empty());
        assert_eq!(after.audit().verify(), Ok(()));
        assert!(after.audit().records().len() > saved_audit.len());
        // New ids never collide with restored ones.
        let sid = after.open_session(T, "ann", "member").unwrap();
        let again = after.submit(T, &sid, "/note.send to=carol").unwrap();
        assert!(matches!(&again, Reply::AwaitingApproval { id: n } if *n != id));
    }

    #[test]
    fn a_damaged_saved_log_is_loaded_as_it_is_and_reports_the_damage() {
        let (mut before, s) = ws(false);
        before.submit(T, &s, "/note.read").unwrap();
        let mut records = before.audit().records().to_vec();
        records[1].outcome = "tampered".into();
        let mut after = restarted();
        after.load_audit(records.clone()).unwrap();
        assert_eq!(after.audit().records(), records.as_slice());
        assert_eq!(after.audit().verify(), Err(1));
        assert!(after.load_audit(records).is_err(), "only into an empty log");
    }

    #[test]
    fn importing_a_call_for_an_unknown_role_or_capability_fails() {
        let (mut before, s) = ws(false);
        before.submit(T, &s, "/note.send to=bob").unwrap();
        let good = before.export_pending();
        let mut bad_role = good.clone();
        bad_role[0].role = "nobody".into();
        let mut bad_cap = good.clone();
        bad_cap[0].capability = "ghost.run".into();
        assert!(restarted().import_pending(bad_role).is_err());
        assert!(restarted().import_pending(bad_cap).is_err());
        let mut w = restarted();
        w.import_pending(good.clone()).unwrap();
        assert!(w.import_pending(good).is_err(), "the same id twice");
    }
}
