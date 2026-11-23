//! The protocol engine: one request line in, a stream of event lines out.
//!
//! Requests are JSON objects with an `op` and an optional client `id` that is echoed on every
//! event so a client can run several requests over one connection:
//!
//! | `op`      | fields                                          | events                                  |
//! |-----------|-------------------------------------------------|-----------------------------------------|
//! | `open`    | `tenant`, `user`, `role`                        | `session`                               |
//! | `say`     | `tenant`, `session`, `text`                     | `delta`*, `done` / `output` / `approval` |
//! | `decide`  | `tenant`, `approval`, `approver`, `role`, `approve` | `output` / `refused` / `delta`*, `done` |
//! | `history` | `tenant`, `session`                             | `history`                               |
//! | `help`    | -                                               | `help`                                  |
//!
//! Failures are a single `error` event with a `kind` (`invalid`, `denied`, `not_found`,
//! `conflict`, `limit`, `state`, `handler`, `model`, `protocol`).

use crate::json::{obj, parse, s, Value};
use aeon_interface::message::Role;
use aeon_interface::routing::Route;
use aeon_interface::{Error, Reply, Workspace};

/// Longest accepted request line in bytes.
pub const MAX_LINE: usize = 1 << 20;

/// Something that can answer a chat turn, streaming pieces as they are produced.
pub trait Backend {
    /// Answer. `history` is the earlier turns as `(role, text)`; `prompt` is the new user turn
    /// (already washed if it had to leave the machine). Call `on_delta` for each piece and
    /// return the complete reply.
    ///
    /// # Errors
    /// A human-readable reason when the model cannot answer.
    fn complete(
        &mut self,
        route: &Route,
        history: &[(String, String)],
        prompt: &str,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, String>;
}

/// Serves protocol requests against a [`Workspace`] and a [`Backend`].
pub struct Gateway<B: Backend> {
    workspace: Workspace,
    backend: B,
}

fn error_kind(e: &Error) -> &'static str {
    match e {
        Error::Invalid(_) => "invalid",
        Error::Denied(_) => "denied",
        Error::NotFound(_) => "not_found",
        Error::Conflict(_) => "conflict",
        Error::Limit(_) => "limit",
        Error::State(_) => "state",
        Error::Handler(_) => "handler",
    }
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

impl<B: Backend> Gateway<B> {
    /// Wrap a configured workspace (roles, applications and models already added).
    pub fn new(workspace: Workspace, backend: B) -> Self {
        Self { workspace, backend }
    }

    /// The workspace, for inspection (audit, artifacts).
    #[must_use]
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Handle one request line, calling `emit` with each event line (no trailing newline).
    pub fn handle(&mut self, line: &str, emit: &mut dyn FnMut(String)) {
        let req = match parse(line) {
            Ok(v @ Value::Obj(_)) => v,
            Ok(_) => {
                return emit(event(
                    None,
                    "error",
                    fail("protocol", "request must be an object"),
                ))
            }
            Err(e) => return emit(event(None, "error", fail("protocol", &e))),
        };
        let id = req.str("id").map(str::to_string);
        let id = id.as_deref();
        let mut out = |kind: &str, fields: Vec<(&str, Value)>| emit(event(id, kind, fields));
        let field = |name: &str| {
            req.str(name)
                .ok_or_else(|| format!("missing field '{name}'"))
        };
        let result = match req.str("op") {
            Some("help") => {
                let items = self
                    .workspace
                    .help()
                    .into_iter()
                    .map(|(n, d)| obj(vec![("name", s(n)), ("about", s(d))]))
                    .collect();
                out("help", vec![("capabilities", Value::Arr(items))]);
                Ok(())
            }
            Some("open") => (|| {
                let sid = self
                    .workspace
                    .open_session(
                        field("tenant").map_err(Error::Invalid)?,
                        field("user").map_err(Error::Invalid)?,
                        field("role").map_err(Error::Invalid)?,
                    )
                    .map_err(Step::from)?;
                out("session", vec![("session", s(&sid))]);
                Ok(())
            })(),
            Some("history") => (|| {
                let tenant = field("tenant").map_err(Error::Invalid)?;
                let sid = field("session").map_err(Error::Invalid)?;
                let ses = self.workspace.session(tenant, sid)?;
                let items = ses
                    .messages()
                    .iter()
                    .map(|m| {
                        obj(vec![
                            ("role", s(role_name(m.role))),
                            ("text", s(&m.content)),
                        ])
                    })
                    .collect();
                out("history", vec![("messages", Value::Arr(items))]);
                Ok(())
            })(),
            Some("say") => (|| {
                let tenant = field("tenant").map_err(Error::Invalid)?;
                let sid = field("session").map_err(Error::Invalid)?;
                let text = field("text").map_err(Error::Invalid)?;
                let reply = self.workspace.submit(tenant, sid, text)?;
                self.finish(tenant, sid, reply, &mut out)
            })(),
            Some("decide") => (|| {
                let tenant = field("tenant").map_err(Error::Invalid)?;
                let approval = field("approval").map_err(Error::Invalid)?;
                let approver = field("approver").map_err(Error::Invalid)?;
                let role = field("role").map_err(Error::Invalid)?;
                let approve = req
                    .bool("approve")
                    .ok_or_else(|| Error::Invalid("missing field 'approve'".into()))?;
                let reply = self
                    .workspace
                    .decide(tenant, approval, approver, role, approve)?;
                // The held call belongs to a session of this tenant; chat never needs approval,
                // so only capability results come back here.
                self.finish(tenant, "", reply, &mut out)
            })(),
            Some(other) => Err(Step::Fail(Error::Invalid(format!("unknown op '{other}'")))),
            None => Err(Step::Fail(Error::Invalid("missing field 'op'".into()))),
        };
        if let Err(step) = result {
            let (kind, msg) = match step {
                Step::Fail(e) => (error_kind(&e), e.to_string()),
                Step::Model(m) => ("model", m),
            };
            emit(event(id, "error", fail(kind, &msg)));
        }
    }

    fn finish(
        &mut self,
        tenant: &str,
        session: &str,
        reply: Reply,
        out: &mut dyn Emit,
    ) -> Result<(), Step> {
        match reply {
            Reply::Done(o) => {
                let arts = o.artifacts.iter().map(|a| s(a)).collect();
                out(
                    "output",
                    vec![("text", s(&o.text)), ("artifacts", Value::Arr(arts))],
                );
            }
            Reply::AwaitingApproval { id } => out("approval", vec![("approval", s(&id))]),
            Reply::Refused { by } => out("refused", vec![("by", s(&by))]),
            Reply::Chat {
                route,
                prompt,
                washed,
            } => {
                let history: Vec<(String, String)> = {
                    let ses = self.workspace.session(tenant, session)?;
                    let m = ses.messages();
                    // The newest message is this turn's user text; `prompt` replaces it.
                    m[..m.len().saturating_sub(1)]
                        .iter()
                        .map(|x| (role_name(x.role).to_string(), x.content.clone()))
                        .collect()
                };
                let mut sink = |piece: &str| out("delta", vec![("text", s(piece))]);
                let full = self
                    .backend
                    .complete(&route, &history, &prompt, &mut sink)
                    .map_err(Step::Model)?;
                self.workspace.record_reply(tenant, session, &full)?;
                out(
                    "done",
                    vec![
                        ("text", s(&full)),
                        ("model", s(&route.model)),
                        ("washed", Value::Bool(washed)),
                    ],
                );
            }
        }
        Ok(())
    }
}

/// Callback that emits one event of a given kind with its fields.
type Fields<'a> = Vec<(&'a str, Value)>;

trait Emit: FnMut(&str, Fields<'_>) {}
impl<T: FnMut(&str, Fields<'_>)> Emit for T {}

enum Step {
    Fail(Error),
    Model(String),
}

impl From<Error> for Step {
    fn from(e: Error) -> Self {
        Self::Fail(e)
    }
}

fn fail<'a>(kind: &'a str, msg: &'a str) -> Vec<(&'a str, Value)> {
    vec![("kind", s(kind)), ("message", s(msg))]
}

fn event(id: Option<&str>, kind: &str, fields: Vec<(&str, Value)>) -> String {
    let mut pairs = Vec::with_capacity(fields.len() + 2);
    if let Some(id) = id {
        pairs.push(("id", s(id)));
    }
    pairs.push(("event", s(kind)));
    pairs.extend(fields);
    obj(pairs).to_json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeon_interface::capability::{Application, CapabilitySpec, Output, Risk};
    use aeon_interface::policy::{Policy, Role as Perm};
    use aeon_interface::routing::ModelEntry;
    use aeon_interface::workflow::Limits;
    use aeon_interface::Call;

    struct Echo;
    impl Backend for Echo {
        fn complete(
            &mut self,
            _r: &Route,
            history: &[(String, String)],
            prompt: &str,
            on: &mut dyn FnMut(&str),
        ) -> Result<String, String> {
            if prompt.contains("fail") {
                return Err("model offline".into());
            }
            on("echo ");
            on(prompt);
            Ok(format!("echo {prompt} (after {} turns)", history.len()))
        }
    }

    struct Notes;
    impl Application for Notes {
        fn name(&self) -> &str {
            "notes"
        }
        fn capabilities(&self) -> Vec<CapabilitySpec> {
            vec![
                CapabilitySpec::new("notes.list", "List notes", Risk::Read),
                CapabilitySpec::new("notes.delete", "Delete a note", Risk::Consequential),
            ]
        }
        fn invoke(&mut self, c: &Call) -> aeon_interface::Result<Output> {
            Ok(Output {
                text: format!("ran {}", c.capability),
                artifacts: vec![],
            })
        }
    }

    fn gw() -> Gateway<Echo> {
        let mut ws = Workspace::new(Policy::default(), Limits::default()).unwrap();
        ws.add_role(Perm::new("user", &["chat", "notes.*"]));
        ws.add_role(Perm::new("boss", &["approve"]));
        ws.add_model(ModelEntry::new(
            "local-1",
            "local",
            true,
            &["text"],
            4096,
            0,
            50,
        ));
        ws.register_app(Box::new(Notes)).unwrap();
        Gateway::new(ws, Echo)
    }

    fn run(g: &mut Gateway<Echo>, line: &str) -> Vec<Value> {
        let mut v = Vec::new();
        g.handle(line, &mut |l| v.push(parse(&l).unwrap()));
        v
    }

    fn open(g: &mut Gateway<Echo>, tenant: &str) -> String {
        let r = run(
            g,
            &format!(r#"{{"op":"open","tenant":"{tenant}","user":"ann","role":"user"}}"#),
        );
        r[0].str("session").unwrap().to_string()
    }

    #[test]
    fn chat_streams_and_records() {
        let mut g = gw();
        let sid = open(&mut g, "t1");
        let ev = run(
            &mut g,
            &format!(r#"{{"id":"7","op":"say","tenant":"t1","session":"{sid}","text":"hello"}}"#),
        );
        let kinds: Vec<_> = ev.iter().map(|e| e.str("event").unwrap()).collect();
        assert_eq!(kinds, ["delta", "delta", "done"]);
        assert!(ev.iter().all(|e| e.str("id") == Some("7")));
        assert_eq!(ev[2].str("text"), Some("echo hello (after 0 turns)"));
        let h = run(
            &mut g,
            &format!(r#"{{"op":"history","tenant":"t1","session":"{sid}"}}"#),
        );
        let Some(Value::Arr(m)) = h[0].get("messages") else {
            panic!("no messages")
        };
        assert_eq!(m.len(), 2);
        // The second turn sees the first as history.
        let ev = run(
            &mut g,
            &format!(r#"{{"op":"say","tenant":"t1","session":"{sid}","text":"again"}}"#),
        );
        assert_eq!(ev[2].str("text"), Some("echo again (after 2 turns)"));
    }

    #[test]
    fn capability_and_approval() {
        let mut g = gw();
        let sid = open(&mut g, "t1");
        let ev = run(
            &mut g,
            &format!(r#"{{"op":"say","tenant":"t1","session":"{sid}","text":"/notes.list"}}"#),
        );
        assert_eq!(ev[0].str("event"), Some("output"));
        let ev = run(
            &mut g,
            &format!(r#"{{"op":"say","tenant":"t1","session":"{sid}","text":"/notes.delete"}}"#),
        );
        assert_eq!(ev[0].str("event"), Some("approval"));
        let aid = ev[0].str("approval").unwrap().to_string();
        // Another tenant cannot decide it.
        let ev = run(
            &mut g,
            &format!(
                r#"{{"op":"decide","tenant":"t2","approval":"{aid}","approver":"bob","role":"boss","approve":true}}"#
            ),
        );
        assert_eq!(ev[0].str("kind"), Some("not_found"));
        let ev = run(
            &mut g,
            &format!(
                r#"{{"op":"decide","tenant":"t1","approval":"{aid}","approver":"bob","role":"boss","approve":true}}"#
            ),
        );
        assert_eq!(ev[0].str("event"), Some("output"));
        // Single use.
        let ev = run(
            &mut g,
            &format!(
                r#"{{"op":"decide","tenant":"t1","approval":"{aid}","approver":"bob","role":"boss","approve":true}}"#
            ),
        );
        assert_eq!(ev[0].str("event"), Some("error"));
    }

    #[test]
    fn errors_are_events() {
        let mut g = gw();
        assert_eq!(run(&mut g, "nope")[0].str("kind"), Some("protocol"));
        assert_eq!(run(&mut g, "[]")[0].str("kind"), Some("protocol"));
        assert_eq!(
            run(&mut g, r#"{"op":"zap"}"#)[0].str("kind"),
            Some("invalid")
        );
        assert_eq!(
            run(&mut g, r#"{"op":"say"}"#)[0].str("kind"),
            Some("invalid")
        );
        let sid = open(&mut g, "t1");
        // Another tenant's session looks missing.
        let ev = run(
            &mut g,
            &format!(r#"{{"op":"say","tenant":"t2","session":"{sid}","text":"hi"}}"#),
        );
        assert_eq!(ev[0].str("kind"), Some("not_found"));
        let ev = run(
            &mut g,
            &format!(r#"{{"op":"say","tenant":"t1","session":"{sid}","text":"please fail"}}"#),
        );
        assert_eq!(ev.last().unwrap().str("kind"), Some("model"));
    }
}
