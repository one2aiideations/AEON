//! MOULI-SYSTEMS: the platform services of MOULI OS in one crate.
//!
//! | Area | Module |
//! |---|---|
//! | Model Gateway | [`gateway`]: how applications and agents outside the console use the governed interface (JSON, server) |
//! | Identity | [`identity`] (semantic messages, hooks, friction) and [`auth`] (tenancy, principals, maker-checker, envelopes) |
//! | Security | [`security`] (rules, detection, scoring, cases, evidence) and [`gate`] (the permission gate) |
//! | Scheduler | [`sched`]: placement of work on CPU, integrated and discrete GPU |
//! | State | [`store`]: small OS state kept in memory and saved |
//! | Integrations | [`topstar`] (capabilities, devices, protocol, sessions, execution records), [`comms`] (encrypted messages and calls), [`mesh`] (a user's devices) |
//! | Monitoring | [`monitor`]: metrics, health checks, alerts, service-level objectives |
//! | Billing | [`billing`]: usage metering, plans and quotas, invoices |
//! | BYOK key management | [`keys`]: a passphrase-encrypted vault for people's own API keys, bound to hosts, with rotation, limits and an audit trail |
//! | Data washing | [`wash`]: finds and removes personal data and secrets before text leaves the machine, with GDPR and DPDP profiles |
//! | Privacy duties | [`privacy`]: consent records, requests from people with deadlines, retention, breach clocks (GDPR and DPDP) |
//!
//! The AI runtime, memory, knowledge, workflow engine, queue and automation live in
//! `mouli-llm` and `mouli-agentic`. Features: `topstar`, `security` and `comms` (all on by default)
//! (and `identity`, `auth`, `mesh`, `billing`, `keys`) bring in their third-party crates; the OS image
//! builds without them. `wash`, `privacy` and `monitor` need none.
//!
//! Merged from ten crates; behaviour and tests are unchanged.

#[allow(clippy::module_inception)]
pub mod gateway {
    //! MOULI gateway: how applications and agents outside the console use the governed interface.
    //!
    //! The console chat in `moulid` talks to the model directly. Anything else (a desktop shell, a
    //! web front end, an agent in another process) connects to a Unix socket and speaks
    //! newline-delimited JSON: one request per line, a stream of event lines back, so replies can
    //! be shown as they are generated. Every request goes through
    //! [`mouli_interface::Workspace`], so policy, approvals, data washing and audit apply to every
    //! caller the same way.
    //!
    //! - [`json`]: a small JSON reader and writer (no external crate).
    //! - [`gateway`]: the [`Gateway`] protocol engine and the [`Backend`] trait that supplies model
    //!   replies.
    //! - [`server`]: the Unix-socket listener (mode 0600) and connection loop.
    //!
    //! Trust model: the socket's file permissions decide who may connect; the `tenant`, `user` and
    //! `role` in a request are taken as claimed by that caller. Binding them to authenticated
    //! identities (see `mouli-auth` envelopes) is not done yet. The server handles one connection at
    //! a time; no real front end or model backend has been attached, and nothing here has been run
    //! outside tests.
    #![forbid(unsafe_code)]

    pub mod gateway {
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

        use crate::gateway::json::{obj, parse, s, Value};
        use mouli_interface::message::Role;
        use mouli_interface::routing::Route;
        use mouli_interface::{Error, Reply, Workspace};

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
                let mut out =
                    |kind: &str, fields: Vec<(&str, Value)>| emit(event(id, kind, fields));
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
            use mouli_interface::capability::{Application, CapabilitySpec, Output, Risk};
            use mouli_interface::policy::{Policy, Role as Perm};
            use mouli_interface::routing::ModelEntry;
            use mouli_interface::workflow::Limits;
            use mouli_interface::Call;

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
                fn invoke(&mut self, c: &Call) -> mouli_interface::Result<Output> {
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
                    &format!(
                        r#"{{"id":"7","op":"say","tenant":"t1","session":"{sid}","text":"hello"}}"#
                    ),
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
                    &format!(
                        r#"{{"op":"say","tenant":"t1","session":"{sid}","text":"/notes.list"}}"#
                    ),
                );
                assert_eq!(ev[0].str("event"), Some("output"));
                let ev = run(
                    &mut g,
                    &format!(
                        r#"{{"op":"say","tenant":"t1","session":"{sid}","text":"/notes.delete"}}"#
                    ),
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
                    &format!(
                        r#"{{"op":"say","tenant":"t1","session":"{sid}","text":"please fail"}}"#
                    ),
                );
                assert_eq!(ev.last().unwrap().str("kind"), Some("model"));
            }
        }
    }
    pub mod json {
        //! A small JSON reader and writer, enough for the gateway protocol (no external crate).

        use std::fmt::Write as _;

        /// Deepest nesting accepted, so hostile input cannot exhaust the stack.
        const MAX_DEPTH: usize = 32;

        /// A JSON value. Objects keep their key order.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Value {
            /// `null`.
            Null,
            /// `true` or `false`.
            Bool(bool),
            /// Any number.
            Num(f64),
            /// A string.
            Str(String),
            /// An array.
            Arr(Vec<Value>),
            /// An object.
            Obj(Vec<(String, Value)>),
        }

        impl Value {
            /// The member `key` of an object.
            #[must_use]
            pub fn get(&self, key: &str) -> Option<&Value> {
                match self {
                    Self::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                    _ => None,
                }
            }

            /// The string member `key`.
            #[must_use]
            pub fn str(&self, key: &str) -> Option<&str> {
                match self.get(key)? {
                    Value::Str(s) => Some(s),
                    _ => None,
                }
            }

            /// The boolean member `key`.
            #[must_use]
            pub fn bool(&self, key: &str) -> Option<bool> {
                match self.get(key)? {
                    Value::Bool(b) => Some(*b),
                    _ => None,
                }
            }

            /// Render as compact JSON.
            #[must_use]
            pub fn to_json(&self) -> String {
                let mut s = String::new();
                self.write(&mut s);
                s
            }

            fn write(&self, out: &mut String) {
                match self {
                    Self::Null => out.push_str("null"),
                    Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                    Self::Num(n) if n.is_finite() => {
                        let _ = write!(out, "{n}");
                    }
                    Self::Num(_) => out.push_str("null"),
                    Self::Str(s) => quote(s, out),
                    Self::Arr(a) => {
                        out.push('[');
                        for (i, v) in a.iter().enumerate() {
                            if i > 0 {
                                out.push(',');
                            }
                            v.write(out);
                        }
                        out.push(']');
                    }
                    Self::Obj(m) => {
                        out.push('{');
                        for (i, (k, v)) in m.iter().enumerate() {
                            if i > 0 {
                                out.push(',');
                            }
                            quote(k, out);
                            out.push(':');
                            v.write(out);
                        }
                        out.push('}');
                    }
                }
            }
        }

        /// Build an object from pairs.
        #[must_use]
        pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
            Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
        }

        /// A string value.
        #[must_use]
        pub fn s(text: &str) -> Value {
            Value::Str(text.to_string())
        }

        fn quote(text: &str, out: &mut String) {
            out.push('"');
            for c in text.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        let _ = write!(out, "\\u{:04x}", c as u32);
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }

        /// Parse one JSON document.
        ///
        /// # Errors
        /// A message naming the problem and its byte offset.
        pub fn parse(text: &str) -> Result<Value, String> {
            let mut p = Parser {
                b: text.as_bytes(),
                i: 0,
            };
            let v = p.value(0)?;
            p.ws();
            if p.i != p.b.len() {
                return Err(p.err("trailing characters"));
            }
            Ok(v)
        }

        struct Parser<'a> {
            b: &'a [u8],
            i: usize,
        }

        impl Parser<'_> {
            fn err(&self, what: &str) -> String {
                format!("json: {what} at byte {}", self.i)
            }

            fn ws(&mut self) {
                while self.i < self.b.len()
                    && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r')
                {
                    self.i += 1;
                }
            }

            fn eat(&mut self, c: u8) -> Result<(), String> {
                if self.b.get(self.i) == Some(&c) {
                    self.i += 1;
                    Ok(())
                } else {
                    Err(self.err(&format!("expected '{}'", c as char)))
                }
            }

            fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
                if self.b[self.i..].starts_with(w.as_bytes()) {
                    self.i += w.len();
                    Ok(v)
                } else {
                    Err(self.err("unknown literal"))
                }
            }

            fn value(&mut self, depth: usize) -> Result<Value, String> {
                if depth > MAX_DEPTH {
                    return Err(self.err("nesting too deep"));
                }
                self.ws();
                match self.b.get(self.i) {
                    None => Err(self.err("unexpected end")),
                    Some(b'n') => self.word("null", Value::Null),
                    Some(b't') => self.word("true", Value::Bool(true)),
                    Some(b'f') => self.word("false", Value::Bool(false)),
                    Some(b'"') => Ok(Value::Str(self.string()?)),
                    Some(b'[') => {
                        self.i += 1;
                        let mut a = Vec::new();
                        self.ws();
                        if self.b.get(self.i) == Some(&b']') {
                            self.i += 1;
                            return Ok(Value::Arr(a));
                        }
                        loop {
                            a.push(self.value(depth + 1)?);
                            self.ws();
                            match self.b.get(self.i) {
                                Some(b',') => self.i += 1,
                                Some(b']') => {
                                    self.i += 1;
                                    return Ok(Value::Arr(a));
                                }
                                _ => return Err(self.err("expected ',' or ']'")),
                            }
                        }
                    }
                    Some(b'{') => {
                        self.i += 1;
                        let mut m = Vec::new();
                        self.ws();
                        if self.b.get(self.i) == Some(&b'}') {
                            self.i += 1;
                            return Ok(Value::Obj(m));
                        }
                        loop {
                            self.ws();
                            let k = self.string()?;
                            self.ws();
                            self.eat(b':')?;
                            m.push((k, self.value(depth + 1)?));
                            self.ws();
                            match self.b.get(self.i) {
                                Some(b',') => self.i += 1,
                                Some(b'}') => {
                                    self.i += 1;
                                    return Ok(Value::Obj(m));
                                }
                                _ => return Err(self.err("expected ',' or '}'")),
                            }
                        }
                    }
                    Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
                    Some(_) => Err(self.err("unexpected character")),
                }
            }

            fn number(&mut self) -> Result<Value, String> {
                let start = self.i;
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(Value::Num)
                    .ok_or_else(|| self.err("bad number"))
            }

            fn hex4(&mut self) -> Result<u32, String> {
                let h = self
                    .b
                    .get(self.i..self.i + 4)
                    .and_then(|x| std::str::from_utf8(x).ok())
                    .and_then(|x| u32::from_str_radix(x, 16).ok())
                    .ok_or_else(|| self.err("bad \\u escape"))?;
                self.i += 4;
                Ok(h)
            }

            fn string(&mut self) -> Result<String, String> {
                self.eat(b'"')?;
                let mut out: Vec<u8> = Vec::new();
                loop {
                    let Some(&c) = self.b.get(self.i) else {
                        return Err(self.err("unterminated string"));
                    };
                    self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let Some(&e) = self.b.get(self.i) else {
                                return Err(self.err("unterminated escape"));
                            };
                            self.i += 1;
                            let ch = match e {
                                b'"' => '"',
                                b'\\' => '\\',
                                b'/' => '/',
                                b'n' => '\n',
                                b'r' => '\r',
                                b't' => '\t',
                                b'b' => '\u{8}',
                                b'f' => '\u{c}',
                                b'u' => {
                                    let mut cp = self.hex4()?;
                                    if (0xD800..0xDC00).contains(&cp) {
                                        if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                            return Err(self.err("lone surrogate"));
                                        }
                                        self.i += 2;
                                        let lo = self.hex4()?;
                                        if !(0xDC00..0xE000).contains(&lo) {
                                            return Err(self.err("bad surrogate pair"));
                                        }
                                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    }
                                    char::from_u32(cp).ok_or_else(|| self.err("bad code point"))?
                                }
                                _ => return Err(self.err("bad escape")),
                            };
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        c if c < 0x20 => return Err(self.err("control character in string")),
                        c => out.push(c),
                    }
                }
                String::from_utf8(out).map_err(|_| self.err("invalid utf-8"))
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn round_trip() {
                let src = r#"{"a":[1,2.5,true,null],"b":"x\ny\u00e9\ud83d\ude00","c":{}}"#;
                let v = parse(src).unwrap();
                assert_eq!(v.str("b"), Some("x\nyé😀"));
                assert_eq!(parse(&v.to_json()).unwrap(), v);
            }

            #[test]
            fn rejects_bad_input() {
                for bad in [
                    "",
                    "{",
                    "[1,]",
                    "{\"a\" 1}",
                    "\"abc",
                    "tru",
                    "1 2",
                    "\"\\ud800\"",
                    "\"a\u{1}\"",
                ] {
                    assert!(parse(bad).is_err(), "{bad:?} should fail");
                }
                let deep = "[".repeat(100) + &"]".repeat(100);
                assert!(parse(&deep).is_err());
            }

            #[test]
            fn escapes_output() {
                assert_eq!(s("a\"b\\\u{1}").to_json(), r#""a\"b\\\u0001""#);
                assert_eq!(Value::Num(f64::NAN).to_json(), "null");
            }
        }
    }
    pub mod server {
        //! A Unix-socket server for the gateway: one request per line, events written back as lines.

        use crate::gateway::gateway::{Backend, Gateway, MAX_LINE};
        use std::io::{BufRead, BufReader, Read, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::path::Path;

        /// Bind `path` (replacing a stale socket file) and restrict it to the owner (mode 0600), so
        /// only the user running the gateway can talk to it.
        ///
        /// # Errors
        /// I/O errors from removing, binding or changing permissions.
        pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            let l = UnixListener::bind(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            Ok(l)
        }

        /// Serve one connection until it closes. Over-long lines are refused with an `error` event
        /// and the connection is closed.
        ///
        /// # Errors
        /// I/O errors on the stream.
        pub fn serve_connection<B: Backend>(
            gw: &mut Gateway<B>,
            stream: UnixStream,
        ) -> std::io::Result<()> {
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut writer = stream;
            loop {
                let mut raw = Vec::new();
                let n = (&mut reader)
                    .take(MAX_LINE as u64 + 1)
                    .read_until(b'\n', &mut raw)?;
                if n == 0 {
                    return Ok(());
                }
                if raw.len() > MAX_LINE {
                    writer.write_all(
                        b"{\"event\":\"error\",\"kind\":\"protocol\",\"message\":\"request line too long\"}\n",
                    )?;
                    return Ok(());
                }
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let mut failed = None;
                gw.handle(line, &mut |ev| {
                    if failed.is_none() {
                        let r = writer
                            .write_all(ev.as_bytes())
                            .and_then(|()| writer.write_all(b"\n"))
                            .and_then(|()| writer.flush());
                        if let Err(e) = r {
                            failed = Some(e);
                        }
                    }
                });
                if let Some(e) = failed {
                    return Err(e);
                }
            }
        }

        /// Accept connections one after another until the listener fails.
        ///
        /// A client that disconnects mid-reply only ends its own connection.
        ///
        /// # Errors
        /// The error from `accept`.
        pub fn serve<B: Backend>(
            gw: &mut Gateway<B>,
            listener: &UnixListener,
        ) -> std::io::Result<()> {
            for stream in listener.incoming() {
                let _ = serve_connection(gw, stream?);
            }
            Ok(())
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use mouli_interface::policy::{Policy, Role as Perm};
            use mouli_interface::routing::{ModelEntry, Route};
            use mouli_interface::workflow::Limits;
            use mouli_interface::Workspace;

            struct Fixed;
            impl Backend for Fixed {
                fn complete(
                    &mut self,
                    _: &Route,
                    _: &[(String, String)],
                    _: &str,
                    on: &mut dyn FnMut(&str),
                ) -> Result<String, String> {
                    on("hi");
                    Ok("hi".into())
                }
            }

            #[test]
            fn talks_over_a_socket() {
                let dir = std::env::temp_dir().join(format!("mouli-gw-{}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join("gw.sock");
                let listener = bind(&path).unwrap();
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                let mut ws = Workspace::new(Policy::default(), Limits::default()).unwrap();
                ws.add_role(Perm::new("user", &["chat"]));
                ws.add_model(ModelEntry::new("m", "local", true, &["text"], 4096, 0, 1));
                let mut gw = Gateway::new(ws, Fixed);
                let p2 = path.clone();
                let client = std::thread::spawn(move || {
                    let mut c = UnixStream::connect(p2).unwrap();
                    c.write_all(
                        b"{\"op\":\"open\",\"tenant\":\"t\",\"user\":\"u\",\"role\":\"user\"}\n",
                    )
                    .unwrap();
                    let mut r = BufReader::new(c.try_clone().unwrap());
                    let mut l = String::new();
                    r.read_line(&mut l).unwrap();
                    let sid = crate::gateway::json::parse(&l)
                        .unwrap()
                        .str("session")
                        .unwrap()
                        .to_string();
                    let say = format!(
                        "{{\"op\":\"say\",\"tenant\":\"t\",\"session\":\"{sid}\",\"text\":\"yo\"}}\n"
                    );
                    c.write_all(say.as_bytes()).unwrap();
                    let mut got = Vec::new();
                    for _ in 0..2 {
                        let mut l = String::new();
                        r.read_line(&mut l).unwrap();
                        got.push(l);
                    }
                    // Oversized line is refused.
                    let big = vec![b'a'; MAX_LINE + 10];
                    let _ = c.write_all(&big);
                    let mut l = String::new();
                    let _ = r.read_line(&mut l);
                    (got, l)
                });
                let (stream, _) = listener.accept().unwrap();
                let _ = serve_connection(&mut gw, stream);
                let (got, big) = client.join().unwrap();
                assert!(got[0].contains("\"delta\"") && got[1].contains("\"done\""));
                assert!(big.contains("too long"));
                std::fs::remove_dir_all(&dir).unwrap();
            }
        }
    }

    pub use gateway::{Backend, Gateway};
}

#[cfg(feature = "identity")]
#[allow(clippy::module_inception)]
pub mod identity {
    //! MOULI identity layer.
    //!
    //! Semantic messages are subject/predicate/object triples. A message may
    //! leave or enter the machine only when:
    //!
    //! 1. its SHA-256 digest matches the triple,
    //! 2. the sender's DID is registered, active and human-verified,
    //! 3. an Ed25519 signature over the digest verifies with the registered key,
    //! 4. a human-presence ("friction") challenge was answered with a signed
    //!    attestation bound to the same context,
    //! 5. it carries a credential hash and a content address (CID).
    //!
    //! Modules: [`registry`], [`semantic`], [`friction`], [`wire`], [`gate`], [`hooks`].
    //! Ported from the retired C core (`aeon_identity*`, `aeon_semantic*`,
    //! `aeon_friction`, `aeon_syscall`, `aeon_io_gate`, `aeon_process_gate`,
    //! `aeon_net_policy`).
    #![forbid(unsafe_code)]

    pub mod friction {
        //! Human-presence friction: a challenge bound to a context, answered by a
        //! verifier-signed attestation.
        //!
        //! The attestation signs `challenge (u64, little-endian) || context_hash`.

        use crate::identity::semantic::verify_ed25519;
        use crate::identity::IdError;
        use ed25519_dalek::{Signer, SigningKey};

        /// An issued challenge.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Challenge {
            /// Non-zero nonce.
            pub nonce: u64,
            /// Hash of what the human is approving.
            pub context_hash: [u8; 32],
        }

        /// A verifier's signed confirmation that a human answered `nonce`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Attestation {
            /// Nonce answered.
            pub nonce: u64,
            /// Context the human approved.
            pub context_hash: [u8; 32],
            /// Verifier Ed25519 public key.
            pub verifier_key: [u8; 32],
            /// Signature over `message(nonce, context_hash)`.
            pub signature: [u8; 64],
        }

        /// Bytes signed by the verifier.
        #[must_use]
        pub fn message(nonce: u64, context_hash: &[u8; 32]) -> [u8; 40] {
            let mut m = [0u8; 40];
            m[..8].copy_from_slice(&nonce.to_le_bytes());
            m[8..].copy_from_slice(context_hash);
            m
        }

        impl Challenge {
            /// Issue a challenge; `nonce` must be non-zero.
            pub fn issue(nonce: u64, context_hash: [u8; 32]) -> Result<Self, IdError> {
                if nonce == 0 {
                    return Err(IdError::BadArg);
                }
                Ok(Self {
                    nonce,
                    context_hash,
                })
            }

            /// Check an attestation answers this challenge and is validly signed.
            pub fn verify(&self, a: &Attestation) -> Result<(), IdError> {
                if self.nonce == 0 || a.nonce != self.nonce || a.context_hash != self.context_hash {
                    return Err(IdError::Friction);
                }
                verify_ed25519(
                    &a.verifier_key,
                    &a.signature,
                    &message(a.nonce, &a.context_hash),
                )
                .map_err(|_| IdError::Friction)
            }
        }

        impl Attestation {
            /// Sign an answer to `c` with the verifier key (used by the verifier service and tests).
            #[must_use]
            pub fn sign(c: &Challenge, verifier: &SigningKey) -> Self {
                Self {
                    nonce: c.nonce,
                    context_hash: c.context_hash,
                    verifier_key: verifier.verifying_key().to_bytes(),
                    signature: verifier.sign(&message(c.nonce, &c.context_hash)).to_bytes(),
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn verify_round_trip() {
                let v = SigningKey::from_bytes(&[3; 32]);
                let c = Challenge::issue(7, [1; 32]).unwrap();
                let a = Attestation::sign(&c, &v);
                c.verify(&a).unwrap();
                let other = Challenge::issue(8, [1; 32]).unwrap();
                assert_eq!(other.verify(&a), Err(IdError::Friction));
                let mut bad = a;
                bad.signature[10] ^= 0xff;
                assert_eq!(c.verify(&bad), Err(IdError::Friction));
                assert_eq!(Challenge::issue(0, [0; 32]), Err(IdError::BadArg));
            }
        }
    }
    pub mod gate {
        //! I/O gates: submit flags, per-process accounting, and network hook policy.

        use crate::identity::registry::DidRegistry;
        use crate::identity::semantic::{Frame, Verdict};
        use crate::identity::IdError;
        use std::collections::HashMap;

        /// Extra requirements a submitter may demand on top of the full check.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct SubmitFlags {
            /// Require a human-presence attestation.
            pub require_human: bool,
            /// Require the sender to claim human verification.
            pub require_did: bool,
            /// Require a credential hash.
            pub require_vc: bool,
            /// Require a content address.
            pub require_cid: bool,
        }

        /// Kind of I/O being gated.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum IoClass {
            /// Local inter-process message.
            Ipc,
            /// Outgoing network.
            NetTx,
            /// Incoming network.
            NetRx,
        }

        /// Check a frame for submission: fast flag checks, then the full gate.
        pub fn submit(
            registry: &DidRegistry,
            flags: SubmitFlags,
            f: &Frame,
        ) -> Result<(), IdError> {
            if flags.require_did && !f.auth.human_verified {
                return Err(IdError::NotEligible);
            }
            if flags.require_cid && f.cid.is_empty() {
                return Err(IdError::NoCid);
            }
            if flags.require_vc && f.auth.vc_hash == [0; 32] {
                return Err(IdError::NoCredential);
            }
            // The full check always verifies friction, so `require_human` is implied.
            match f.check(registry) {
                Verdict::Allow => Ok(()),
                Verdict::Malformed => Err(IdError::Malformed),
                Verdict::Digest => Err(IdError::Digest),
                Verdict::Identity(e) => Err(e),
            }
        }

        /// Per-process gate state.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct ProcessState {
            /// Last submission was accepted.
            pub authenticated: bool,
            /// Accepted submissions.
            pub accepted: u64,
            /// Denied submissions.
            pub denied: u64,
        }

        /// Tracks gate outcomes per process id.
        #[derive(Debug, Clone)]
        pub struct ProcessGate {
            states: HashMap<u32, ProcessState>,
            max_processes: usize,
            generation: u32,
        }

        impl ProcessGate {
            /// Gate tracking at most `max_processes` processes.
            #[must_use]
            pub fn new(max_processes: usize) -> Self {
                Self {
                    states: HashMap::new(),
                    max_processes,
                    generation: 1,
                }
            }

            /// Submit on behalf of `pid`; records the outcome.
            pub fn submit(
                &mut self,
                registry: &DidRegistry,
                pid: u32,
                _class: IoClass,
                flags: SubmitFlags,
                f: &Frame,
            ) -> Result<(), IdError> {
                if pid == 0 {
                    return Err(IdError::BadArg);
                }
                if !self.states.contains_key(&pid) && self.states.len() >= self.max_processes {
                    return Err(IdError::TooManyProcesses);
                }
                let result = submit(registry, flags, f);
                let st = self.states.entry(pid).or_default();
                match result {
                    Ok(()) => {
                        st.authenticated = true;
                        st.accepted += 1;
                    }
                    Err(_) => {
                        st.authenticated = false;
                        st.denied += 1;
                    }
                }
                result
            }

            /// State of `pid`, if seen.
            #[must_use]
            pub fn query(&self, pid: u32) -> Option<ProcessState> {
                self.states.get(&pid).copied()
            }

            /// Policy generation (bumped when policy changes).
            #[must_use]
            pub fn generation(&self) -> u32 {
                self.generation
            }

            /// Forget all process state and bump the generation.
            pub fn reset_policy(&mut self) {
                self.states.clear();
                self.generation = self.generation.wrapping_add(1).max(1);
            }
        }

        /// Network hook point.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum NetHook {
            /// Before transmit.
            PreTx,
            /// Before receive.
            PreRx,
        }

        /// Packet decision.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum NetAction {
            /// Drop.
            Drop,
            /// Pass.
            Accept,
            /// Hold until the semantic frame is available.
            DeferIdentity,
        }

        /// Decide a packet. Non-semantic traffic passes; semantic traffic must
        /// carry a frame that passes the gate.
        #[must_use]
        pub fn net_decide(
            registry: &DidRegistry,
            _hook: NetHook,
            semantic_candidate: bool,
            frame: Option<&Frame>,
        ) -> NetAction {
            if !semantic_candidate {
                return NetAction::Accept;
            }
            match frame {
                None => NetAction::DeferIdentity,
                Some(f) if f.check(registry) == Verdict::Allow => NetAction::Accept,
                Some(_) => NetAction::Drop,
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::identity::semantic::tests::signed_frame;

            #[test]
            fn process_accounting() {
                let (f, reg) = signed_frame();
                let mut g = ProcessGate::new(1);
                g.submit(&reg, 10, IoClass::NetTx, SubmitFlags::default(), &f)
                    .unwrap();
                let mut bad = f.clone();
                bad.digest[0] ^= 1;
                assert_eq!(
                    g.submit(&reg, 10, IoClass::Ipc, SubmitFlags::default(), &bad),
                    Err(IdError::Digest)
                );
                let st = g.query(10).unwrap();
                assert_eq!((st.accepted, st.denied, st.authenticated), (1, 1, false));
                assert_eq!(
                    g.submit(&reg, 11, IoClass::Ipc, SubmitFlags::default(), &f),
                    Err(IdError::TooManyProcesses)
                );
                g.reset_policy();
                assert_eq!(g.generation(), 2);
                assert!(g.query(10).is_none());
            }

            #[test]
            fn net_policy() {
                let (f, reg) = signed_frame();
                assert_eq!(
                    net_decide(&reg, NetHook::PreRx, false, None),
                    NetAction::Accept
                );
                assert_eq!(
                    net_decide(&reg, NetHook::PreRx, true, None),
                    NetAction::DeferIdentity
                );
                assert_eq!(
                    net_decide(&reg, NetHook::PreTx, true, Some(&f)),
                    NetAction::Accept
                );
                let mut bad = f;
                bad.cid.clear();
                assert_eq!(
                    net_decide(&reg, NetHook::PreTx, true, Some(&bad)),
                    NetAction::Drop
                );
            }

            #[test]
            fn submit_flags() {
                let (mut f, reg) = signed_frame();
                f.auth.vc_hash = [0; 32];
                let flags = SubmitFlags {
                    require_vc: true,
                    ..SubmitFlags::default()
                };
                assert_eq!(submit(&reg, flags, &f), Err(IdError::NoCredential));
            }
        }
    }
    pub mod hooks {
        //! Kernel-side hook accounting and the fast eBPF pre-classifier.
        //!
        //! The eBPF program cannot verify signatures, so it only sorts events into
        //! "not semantic, pass" and "send to the identity gate". [`HookTable`]
        //! wraps [`net_decide`] with counters for the attached hooks.

        use crate::identity::gate::{net_decide, IoClass, NetAction, NetHook};
        use crate::identity::registry::DidRegistry;
        use crate::identity::semantic::Frame;
        use crate::identity::IdError;

        /// Hook counters.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct HookStats {
            /// Transmit events seen.
            pub tx: u64,
            /// Receive events seen.
            pub rx: u64,
            /// Events held for identity.
            pub deferred: u64,
            /// Events dropped.
            pub dropped: u64,
        }

        /// Attached network hooks.
        #[derive(Debug, Clone, Default)]
        pub struct HookTable {
            attached: bool,
            stats: HookStats,
        }

        impl HookTable {
            /// Attach (resets counters).
            pub fn attach(&mut self) {
                self.attached = true;
                self.stats = HookStats::default();
            }

            /// Detach.
            pub fn detach(&mut self) -> Result<(), IdError> {
                if !self.attached {
                    return Err(IdError::BadArg);
                }
                self.attached = false;
                Ok(())
            }

            /// Decide and count one event.
            pub fn event(
                &mut self,
                registry: &DidRegistry,
                hook: NetHook,
                semantic_candidate: bool,
                frame: Option<&Frame>,
            ) -> Result<NetAction, IdError> {
                if !self.attached {
                    return Err(IdError::BadArg);
                }
                let action = net_decide(registry, hook, semantic_candidate, frame);
                match hook {
                    NetHook::PreTx => self.stats.tx += 1,
                    NetHook::PreRx => self.stats.rx += 1,
                }
                match action {
                    NetAction::DeferIdentity => self.stats.deferred += 1,
                    NetAction::Drop => self.stats.dropped += 1,
                    NetAction::Accept => {}
                }
                Ok(action)
            }

            /// Counters.
            #[must_use]
            pub fn stats(&self) -> HookStats {
                self.stats
            }
        }

        /// eBPF pre-classification result.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Classify {
            /// Not semantic traffic: pass without the identity gate.
            NotSemantic,
            /// Semantic traffic: must go through the identity gate.
            IdentityGate,
        }

        /// Classify an event for process `pid` (0 is invalid).
        pub fn classify(
            pid: u32,
            _class: IoClass,
            semantic_candidate: bool,
        ) -> Result<Classify, IdError> {
            if pid == 0 {
                return Err(IdError::BadArg);
            }
            Ok(if semantic_candidate {
                Classify::IdentityGate
            } else {
                Classify::NotSemantic
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::identity::semantic::tests::signed_frame;

            #[test]
            fn counts_events() {
                let (f, reg) = signed_frame();
                let mut h = HookTable::default();
                assert!(h.event(&reg, NetHook::PreTx, false, None).is_err());
                h.attach();
                h.event(&reg, NetHook::PreTx, true, Some(&f)).unwrap();
                h.event(&reg, NetHook::PreRx, true, None).unwrap();
                let mut bad = f;
                bad.digest[1] ^= 1;
                h.event(&reg, NetHook::PreRx, true, Some(&bad)).unwrap();
                assert_eq!(
                    h.stats(),
                    HookStats {
                        tx: 1,
                        rx: 2,
                        deferred: 1,
                        dropped: 1
                    }
                );
                h.detach().unwrap();
                assert!(h.detach().is_err());
            }

            #[test]
            fn classifier() {
                assert_eq!(classify(1, IoClass::Ipc, false), Ok(Classify::NotSemantic));
                assert_eq!(
                    classify(1, IoClass::NetTx, true),
                    Ok(Classify::IdentityGate)
                );
                assert!(classify(0, IoClass::NetRx, true).is_err());
            }
        }
    }
    pub mod registry {
        //! Registry of decentralised identifiers (DIDs) and their Ed25519 keys.

        use crate::identity::{check_field, IdError};

        /// One registered identity.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct DidRecord {
            /// The DID string.
            pub did: String,
            /// Ed25519 public key.
            pub public_key: [u8; 32],
            /// A human was verified behind this identity.
            pub human_verified: bool,
            /// Record is in use.
            pub active: bool,
        }

        /// Bounded DID registry.
        #[derive(Debug, Clone)]
        pub struct DidRegistry {
            records: Vec<DidRecord>,
            capacity: usize,
        }

        impl DidRegistry {
            /// Empty registry with room for `capacity` records.
            pub fn new(capacity: usize) -> Result<Self, IdError> {
                if capacity == 0 {
                    return Err(IdError::BadArg);
                }
                Ok(Self {
                    records: Vec::new(),
                    capacity,
                })
            }

            /// Add an active, human-verified record.
            pub fn add(&mut self, rec: DidRecord) -> Result<(), IdError> {
                check_field(&rec.did)?;
                if !rec.active || !rec.human_verified {
                    return Err(IdError::NotEligible);
                }
                if self.records.len() >= self.capacity {
                    return Err(IdError::Full);
                }
                if self.find(&rec.did).is_some() {
                    return Err(IdError::Duplicate);
                }
                self.records.push(rec);
                Ok(())
            }

            /// Look up a DID.
            #[must_use]
            pub fn find(&self, did: &str) -> Option<&DidRecord> {
                self.records.iter().find(|r| r.did == did)
            }

            /// Mark a DID inactive (revocation). Returns whether it existed.
            pub fn revoke(&mut self, did: &str) -> bool {
                match self.records.iter_mut().find(|r| r.did == did) {
                    Some(r) => {
                        r.active = false;
                        true
                    }
                    None => false,
                }
            }

            /// True when `did` is registered, active, human-verified and uses `key`.
            #[must_use]
            pub fn matches(&self, did: &str, key: &[u8; 32]) -> bool {
                self.find(did)
                    .is_some_and(|r| r.active && r.human_verified && &r.public_key == key)
            }

            /// Number of records.
            #[must_use]
            pub fn len(&self) -> usize {
                self.records.len()
            }

            /// True when empty.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.records.is_empty()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn rec(did: &str) -> DidRecord {
                DidRecord {
                    did: did.into(),
                    public_key: [7; 32],
                    human_verified: true,
                    active: true,
                }
            }

            #[test]
            fn add_find_revoke() {
                let mut r = DidRegistry::new(2).unwrap();
                r.add(rec("did:mouli:a")).unwrap();
                assert_eq!(r.add(rec("did:mouli:a")), Err(IdError::Duplicate));
                r.add(rec("did:mouli:b")).unwrap();
                assert_eq!(r.add(rec("did:mouli:c")), Err(IdError::Full));
                assert!(r.matches("did:mouli:a", &[7; 32]));
                assert!(!r.matches("did:mouli:a", &[8; 32]));
                assert!(r.revoke("did:mouli:a"));
                assert!(!r.matches("did:mouli:a", &[7; 32]));
            }

            #[test]
            fn rejects_unverified() {
                let mut r = DidRegistry::new(1).unwrap();
                let mut x = rec("did:mouli:x");
                x.human_verified = false;
                assert_eq!(r.add(x), Err(IdError::NotEligible));
                assert_eq!(r.add(rec("")), Err(IdError::BadField));
            }
        }
    }
    pub mod semantic {
        //! Semantic triples, their canonical digest, and signed transmissions.

        use crate::identity::friction::{Attestation, Challenge};
        use crate::identity::registry::DidRegistry;
        use crate::identity::{check_field, IdError};
        use ed25519_dalek::{Signature, VerifyingKey};
        use sha2::{Digest, Sha256};

        /// Subject / predicate / object.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Triple {
            /// Subject.
            pub subject: String,
            /// Predicate.
            pub predicate: String,
            /// Object.
            pub object: String,
        }

        impl Triple {
            /// Validate fields (non-empty, bounded, no NUL or newline).
            pub fn validate(&self) -> Result<(), IdError> {
                for f in [&self.subject, &self.predicate, &self.object] {
                    check_field(f)?;
                    if f.contains('\n') {
                        return Err(IdError::BadField);
                    }
                }
                Ok(())
            }

            /// Canonical bytes: `S=<s>\nP=<p>\nO=<o>\n`.
            pub fn canonical(&self) -> Result<Vec<u8>, IdError> {
                self.validate()?;
                Ok(format!(
                    "S={}\nP={}\nO={}\n",
                    self.subject, self.predicate, self.object
                )
                .into_bytes())
            }

            /// SHA-256 of the canonical bytes.
            pub fn digest(&self) -> Result<[u8; 32], IdError> {
                Ok(Sha256::digest(self.canonical()?).into())
            }
        }

        /// Sender identity attached to a transmission.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct AuthPayload {
            /// Sender DID.
            pub did: String,
            /// Sender Ed25519 public key.
            pub public_key: [u8; 32],
            /// Ed25519 signature over the triple digest.
            pub signature: [u8; 64],
            /// Hash of the verifiable credential presented.
            pub vc_hash: [u8; 32],
            /// Sender claims human verification.
            pub human_verified: bool,
        }

        /// A complete semantic message as carried on the wire.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Frame {
            /// Reserved flag bits.
            pub flags: u16,
            /// Claimed digest of `triple`.
            pub digest: [u8; 32],
            /// Payload.
            pub triple: Triple,
            /// Sender.
            pub auth: AuthPayload,
            /// Human-presence challenge.
            pub friction: Challenge,
            /// Signed answer to the challenge.
            pub attestation: Attestation,
            /// Content address of the full payload.
            pub cid: String,
        }

        /// Why a frame was refused.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Verdict {
            /// Frame accepted.
            Allow,
            /// Structure invalid.
            Malformed,
            /// Digest mismatch.
            Digest,
            /// Identity, signature, friction, credential or CID check failed.
            Identity(IdError),
        }

        /// Ed25519 verification of `sig` over `msg`.
        pub fn verify_ed25519(key: &[u8; 32], sig: &[u8; 64], msg: &[u8]) -> Result<(), IdError> {
            let vk = VerifyingKey::from_bytes(key).map_err(|_| IdError::Signature)?;
            vk.verify_strict(msg, &Signature::from_bytes(sig))
                .map_err(|_| IdError::Signature)
        }

        impl Frame {
            /// Structural checks only.
            pub fn validate(&self) -> Result<(), IdError> {
                self.triple.validate()?;
                check_field(&self.auth.did)?;
                check_field(&self.cid)
            }

            /// Full identity check (steps 2-5 of the crate docs), given the digest is right.
            pub fn verify_identity(&self, registry: &DidRegistry) -> Result<(), IdError> {
                self.friction.verify(&self.attestation)?;
                if !self.auth.human_verified {
                    return Err(IdError::NotEligible);
                }
                if !registry.matches(&self.auth.did, &self.auth.public_key) {
                    return Err(IdError::UnknownIdentity);
                }
                if self.auth.vc_hash == [0; 32] {
                    return Err(IdError::NoCredential);
                }
                verify_ed25519(&self.auth.public_key, &self.auth.signature, &self.digest)?;
                if self.cid.is_empty() {
                    return Err(IdError::NoCid);
                }
                Ok(())
            }

            /// Gate decision for this frame.
            #[must_use]
            pub fn check(&self, registry: &DidRegistry) -> Verdict {
                if self.validate().is_err() {
                    return Verdict::Malformed;
                }
                match self.triple.digest() {
                    Ok(d) if d == self.digest => {}
                    _ => return Verdict::Digest,
                }
                match self.verify_identity(registry) {
                    Ok(()) => Verdict::Allow,
                    Err(e) => Verdict::Identity(e),
                }
            }
        }

        #[cfg(test)]
        pub(crate) mod tests {
            use super::*;
            use crate::identity::registry::DidRecord;
            use ed25519_dalek::{Signer, SigningKey};

            pub(crate) fn signed_frame() -> (Frame, DidRegistry) {
                let sender = SigningKey::from_bytes(&[1; 32]);
                let verifier = SigningKey::from_bytes(&[2; 32]);
                let triple = Triple {
                    subject: "file:report".into(),
                    predicate: "shareWith".into(),
                    object: "did:mouli:bob".into(),
                };
                let digest = triple.digest().unwrap();
                let friction = Challenge::issue(42, [9; 32]).unwrap();
                let attestation = Attestation::sign(&friction, &verifier);
                let frame = Frame {
                    flags: 0,
                    digest,
                    triple,
                    auth: AuthPayload {
                        did: "did:mouli:alice".into(),
                        public_key: sender.verifying_key().to_bytes(),
                        signature: sender.sign(&digest).to_bytes(),
                        vc_hash: [5; 32],
                        human_verified: true,
                    },
                    friction,
                    attestation,
                    cid: "bafy-demo".into(),
                };
                let mut reg = DidRegistry::new(4).unwrap();
                reg.add(DidRecord {
                    did: "did:mouli:alice".into(),
                    public_key: sender.verifying_key().to_bytes(),
                    human_verified: true,
                    active: true,
                })
                .unwrap();
                (frame, reg)
            }

            #[test]
            fn canonical_form() {
                let t = Triple {
                    subject: "a".into(),
                    predicate: "b".into(),
                    object: "c".into(),
                };
                assert_eq!(t.canonical().unwrap(), b"S=a\nP=b\nO=c\n");
                let bad = Triple {
                    subject: "a\nP=x".into(),
                    ..t
                };
                assert_eq!(bad.digest(), Err(IdError::BadField));
            }

            #[test]
            fn good_frame_allowed() {
                let (f, reg) = signed_frame();
                assert_eq!(f.check(&reg), Verdict::Allow);
            }

            #[test]
            fn tampering_is_caught() {
                let (f, reg) = signed_frame();
                let mut t = f.clone();
                t.triple.object = "did:mouli:mallory".into();
                assert_eq!(t.check(&reg), Verdict::Digest);

                let mut s = f.clone();
                s.auth.signature[0] ^= 1;
                assert_eq!(s.check(&reg), Verdict::Identity(IdError::Signature));

                let mut v = f.clone();
                v.auth.vc_hash = [0; 32];
                assert_eq!(v.check(&reg), Verdict::Identity(IdError::NoCredential));

                let mut a = f.clone();
                a.attestation.context_hash[0] ^= 1;
                assert_eq!(a.check(&reg), Verdict::Identity(IdError::Friction));

                let empty = DidRegistry::new(1).unwrap();
                assert_eq!(f.check(&empty), Verdict::Identity(IdError::UnknownIdentity));
            }
        }
    }
    pub mod wire {
        //! Big-endian wire encoding of a [`Frame`].
        //!
        //! Layout: magic u32, version u16, flags u16, digest\[32\], subject, predicate,
        //! object (each u16 length + bytes), auth (key\[32\], sig\[64\], vc\[32\], did,
        //! human u8), friction (nonce u64, ctx\[32\]), attestation (nonce u64, ctx\[32\],
        //! key\[32\], sig\[64\]), cid. Trailing bytes are rejected.

        use crate::identity::friction::{Attestation, Challenge};
        use crate::identity::semantic::{AuthPayload, Frame, Triple};
        use crate::identity::IdError;

        /// Wire magic ("AEW2").
        pub const MAGIC: u32 = 0x4145_5732;
        /// Wire version.
        pub const VERSION: u16 = 2;

        struct Writer(Vec<u8>);

        impl Writer {
            fn bytes(&mut self, b: &[u8]) {
                self.0.extend_from_slice(b);
            }
            fn str16(&mut self, s: &str) -> Result<(), IdError> {
                let n = u16::try_from(s.len()).map_err(|_| IdError::BadField)?;
                self.bytes(&n.to_be_bytes());
                self.bytes(s.as_bytes());
                Ok(())
            }
        }

        struct Reader<'a>(&'a [u8]);

        impl<'a> Reader<'a> {
            fn take(&mut self, n: usize) -> Result<&'a [u8], IdError> {
                if self.0.len() < n {
                    return Err(IdError::Malformed);
                }
                let (h, t) = self.0.split_at(n);
                self.0 = t;
                Ok(h)
            }
            fn arr<const N: usize>(&mut self) -> Result<[u8; N], IdError> {
                let mut a = [0u8; N];
                a.copy_from_slice(self.take(N)?);
                Ok(a)
            }
            fn u16(&mut self) -> Result<u16, IdError> {
                Ok(u16::from_be_bytes(self.arr()?))
            }
            fn u32(&mut self) -> Result<u32, IdError> {
                Ok(u32::from_be_bytes(self.arr()?))
            }
            fn u64(&mut self) -> Result<u64, IdError> {
                Ok(u64::from_be_bytes(self.arr()?))
            }
            fn str16(&mut self) -> Result<String, IdError> {
                let n = usize::from(self.u16()?);
                let b = self.take(n)?;
                String::from_utf8(b.to_vec()).map_err(|_| IdError::Malformed)
            }
        }

        /// Encode a structurally valid frame.
        pub fn encode(f: &Frame) -> Result<Vec<u8>, IdError> {
            f.validate()?;
            let mut w = Writer(Vec::with_capacity(512));
            w.bytes(&MAGIC.to_be_bytes());
            w.bytes(&VERSION.to_be_bytes());
            w.bytes(&f.flags.to_be_bytes());
            w.bytes(&f.digest);
            w.str16(&f.triple.subject)?;
            w.str16(&f.triple.predicate)?;
            w.str16(&f.triple.object)?;
            w.bytes(&f.auth.public_key);
            w.bytes(&f.auth.signature);
            w.bytes(&f.auth.vc_hash);
            w.str16(&f.auth.did)?;
            w.bytes(&[u8::from(f.auth.human_verified)]);
            w.bytes(&f.friction.nonce.to_be_bytes());
            w.bytes(&f.friction.context_hash);
            w.bytes(&f.attestation.nonce.to_be_bytes());
            w.bytes(&f.attestation.context_hash);
            w.bytes(&f.attestation.verifier_key);
            w.bytes(&f.attestation.signature);
            w.str16(&f.cid)?;
            Ok(w.0)
        }

        /// Decode and structurally validate a frame.
        pub fn decode(b: &[u8]) -> Result<Frame, IdError> {
            let mut r = Reader(b);
            if r.u32()? != MAGIC || r.u16()? != VERSION {
                return Err(IdError::Malformed);
            }
            let flags = r.u16()?;
            let digest = r.arr()?;
            let triple = Triple {
                subject: r.str16()?,
                predicate: r.str16()?,
                object: r.str16()?,
            };
            let public_key = r.arr()?;
            let signature = r.arr()?;
            let vc_hash = r.arr()?;
            let did = r.str16()?;
            let human_verified = match r.take(1)?[0] {
                0 => false,
                1 => true,
                _ => return Err(IdError::Malformed),
            };
            let friction = Challenge {
                nonce: r.u64()?,
                context_hash: r.arr()?,
            };
            let attestation = Attestation {
                nonce: r.u64()?,
                context_hash: r.arr()?,
                verifier_key: r.arr()?,
                signature: r.arr()?,
            };
            let cid = r.str16()?;
            if !r.0.is_empty() {
                return Err(IdError::Malformed);
            }
            let f = Frame {
                flags,
                digest,
                triple,
                auth: AuthPayload {
                    did,
                    public_key,
                    signature,
                    vc_hash,
                    human_verified,
                },
                friction,
                attestation,
                cid,
            };
            f.validate().map_err(|_| IdError::Malformed)?;
            Ok(f)
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::identity::semantic::{tests::signed_frame, Verdict};

            #[test]
            fn round_trip_keeps_verdict() {
                let (f, reg) = signed_frame();
                let bytes = encode(&f).unwrap();
                let back = decode(&bytes).unwrap();
                assert_eq!(back, f);
                assert_eq!(back.check(&reg), Verdict::Allow);
            }

            #[test]
            fn rejects_truncation_and_trailing() {
                let (f, _) = signed_frame();
                let bytes = encode(&f).unwrap();
                for cut in [0, 4, 10, bytes.len() - 1] {
                    assert_eq!(decode(&bytes[..cut]), Err(IdError::Malformed));
                }
                let mut extra = bytes.clone();
                extra.push(0);
                assert_eq!(decode(&extra), Err(IdError::Malformed));
                let mut magic = bytes;
                magic[0] ^= 1;
                assert_eq!(decode(&magic), Err(IdError::Malformed));
            }
        }
    }

    use core::fmt;

    /// Identity-layer errors.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum IdError {
        /// A field is empty, too long or contains NUL.
        BadField,
        /// Registry is full.
        Full,
        /// DID already registered.
        Duplicate,
        /// Record is not active or not human-verified.
        NotEligible,
        /// DID unknown or key mismatch.
        UnknownIdentity,
        /// Digest does not match the triple.
        Digest,
        /// Signature invalid.
        Signature,
        /// Friction challenge missing, mismatched or unsigned.
        Friction,
        /// Credential hash missing.
        NoCredential,
        /// Content address missing.
        NoCid,
        /// Wire bytes malformed.
        Malformed,
        /// Process table full.
        TooManyProcesses,
        /// Bad argument.
        BadArg,
    }

    impl fmt::Display for IdError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(match self {
                Self::BadField => "bad field",
                Self::Full => "registry full",
                Self::Duplicate => "duplicate identity",
                Self::NotEligible => "identity not active or not human-verified",
                Self::UnknownIdentity => "unknown identity or key mismatch",
                Self::Digest => "digest mismatch",
                Self::Signature => "invalid signature",
                Self::Friction => "human-presence check failed",
                Self::NoCredential => "missing credential hash",
                Self::NoCid => "missing content address",
                Self::Malformed => "malformed wire data",
                Self::TooManyProcesses => "process table full",
                Self::BadArg => "bad argument",
            })
        }
    }

    impl std::error::Error for IdError {}

    /// Longest DID, CID or triple field in bytes.
    pub const FIELD_MAX: usize = 255;

    pub(crate) fn check_field(s: &str) -> Result<(), IdError> {
        if s.is_empty() || s.len() > FIELD_MAX || s.contains('\0') {
            Err(IdError::BadField)
        } else {
            Ok(())
        }
    }
}

#[allow(clippy::module_inception)]
pub mod gate {
    //! MOULI permission gate: every user or agent action passes through here before it runs.
    #![forbid(unsafe_code)]

    pub mod policy {
        #![forbid(unsafe_code)]
        //! Deterministic capability policy gate for AI-initiated MOULI actions.
        //!
        //! This module does not parse natural language and never executes an action.
        //! Callers are responsible for extracting a typed [`Action`] from a model
        //! response or tool call, then asking [`Policy::evaluate`] whether the action
        //! is allowed, denied, or needs an interactive confirmation prompt.

        use std::collections::hash_map::RandomState;
        use std::error::Error;
        use std::fmt;
        use std::hash::BuildHasher;

        /// Maximum accepted length for the raw operator or model utterance being reviewed.
        pub const MAX_INPUT_BYTES: usize = 4096;
        /// Maximum accepted length for a workspace-relative path.
        pub const MAX_PATH_BYTES: usize = 512;
        /// Maximum accepted length for a general single argument.
        pub const MAX_ARG_BYTES: usize = 1024;
        /// Maximum accepted size for a single workspace write or patch.
        pub const MAX_WRITE_BYTES: u64 = 8 * 1024 * 1024;
        const MAX_COMMAND_ARGS: usize = 32;
        const MAX_COMMAND_ARG_BYTES: usize = 256;
        const MAX_COMMAND_TOTAL_BYTES: usize = 2048;
        const MAX_PACKAGE_BYTES: usize = 128;
        const MAX_SYMBOL_BYTES: usize = 64;
        const MAX_QUERY_BYTES: usize = 256;

        /// Coarse capability required to perform an action.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
        pub enum Capability {
            /// Observe read-only status without reading private workspace content.
            Observe,
            /// Show suggestions, explanations, or proposed diffs without touching state.
            Suggest,
            /// Read user workspace files and local indexes.
            ReadWorkspace,
            /// Create, modify, delete, or patch user workspace files.
            WriteWorkspace,
            /// Change network configuration or connectivity.
            Network,
            /// Change installed software, services, or system packages.
            ModifySystem,
            /// Reboot or shut down the machine.
            Power,
        }

        impl Capability {
            /// All capabilities, in stable declaration order.
            pub const ALL: [Capability; 7] = [
                Capability::Observe,
                Capability::Suggest,
                Capability::ReadWorkspace,
                Capability::WriteWorkspace,
                Capability::Network,
                Capability::ModifySystem,
                Capability::Power,
            ];
        }

        impl fmt::Display for Capability {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    Capability::Observe => f.write_str("observe"),
                    Capability::Suggest => f.write_str("suggest"),
                    Capability::ReadWorkspace => f.write_str("read workspace"),
                    Capability::WriteWorkspace => f.write_str("write workspace"),
                    Capability::Network => f.write_str("network"),
                    Capability::ModifySystem => f.write_str("modify system"),
                    Capability::Power => f.write_str("power"),
                }
            }
        }

        const fn capability_bit(capability: Capability) -> u16 {
            match capability {
                Capability::Observe => 1 << 0,
                Capability::Suggest => 1 << 1,
                Capability::ReadWorkspace => 1 << 2,
                Capability::WriteWorkspace => 1 << 3,
                Capability::Network => 1 << 4,
                Capability::ModifySystem => 1 << 5,
                Capability::Power => 1 << 6,
            }
        }

        /// Compact immutable set of granted capabilities.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
        pub struct CapabilitySet(u16);

        impl CapabilitySet {
            /// Creates a set granting no capabilities.
            pub const fn empty() -> Self {
                Self(0)
            }

            /// Creates a set granting every capability.
            pub const fn all() -> Self {
                Self(0b111_1111)
            }

            /// Builds a set from a slice of capabilities.
            pub fn from_slice(capabilities: &[Capability]) -> Self {
                let mut bits = 0u16;
                for &capability in capabilities {
                    bits |= capability_bit(capability);
                }
                Self(bits)
            }

            /// Returns true if `capability` is granted.
            pub fn contains(&self, capability: Capability) -> bool {
                self.0 & capability_bit(capability) != 0
            }

            /// Returns a new set with `capability` added.
            pub fn with(mut self, capability: Capability) -> Self {
                self.0 |= capability_bit(capability);
                self
            }

            /// Returns true if no capabilities are granted.
            pub fn is_empty(&self) -> bool {
                self.0 == 0
            }
        }

        impl fmt::Display for CapabilitySet {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut wrote = false;
                for capability in Capability::ALL {
                    if self.contains(capability) {
                        if wrote {
                            f.write_str(", ")?;
                        }
                        write!(f, "{capability}")?;
                        wrote = true;
                    }
                }
                if !wrote {
                    f.write_str("none")?;
                }
                Ok(())
            }
        }

        /// Kind of real-world consequence a confirmed action may have.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
        pub enum Consequence {
            /// Changes user workspace content or metadata.
            WorkspaceWrite,
            /// Permanently removes workspace content.
            EraseData,
            /// Changes network configuration or connectivity state.
            NetworkChange,
            /// Installs or removes system software or services.
            SystemChange,
            /// Starts a process with effects outside this policy gate.
            SpawnProcess,
            /// Reboots or shuts down the machine.
            Power,
        }

        impl fmt::Display for Consequence {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    Consequence::WorkspaceWrite => f.write_str("workspace write"),
                    Consequence::EraseData => f.write_str("erase data"),
                    Consequence::NetworkChange => f.write_str("network change"),
                    Consequence::SystemChange => f.write_str("system change"),
                    Consequence::SpawnProcess => f.write_str("spawn process"),
                    Consequence::Power => f.write_str("power"),
                }
            }
        }

        /// A typed, non-executing description of an AI-proposed action.
        #[derive(Clone, Debug, Eq, PartialEq, Hash)]
        pub enum Action {
            /// Read a named read-only status source such as `cpu`, `memory`, or `thermal`.
            ReadStatus {
                /// Status identifier.
                name: String,
            },
            /// Search the user's local file or memory index.
            SearchLocalIndex {
                /// Search query text.
                query: String,
            },
            /// Read a workspace-relative file.
            ReadWorkspaceFile {
                /// Workspace-relative path using `/` separators.
                path: String,
            },
            /// Write bytes to a workspace-relative file.
            WriteWorkspaceFile {
                /// Workspace-relative path using `/` separators.
                path: String,
                /// Number of bytes to write.
                bytes: u64,
            },
            /// Apply a text patch to a workspace-relative file.
            ApplyWorkspacePatch {
                /// Workspace-relative path using `/` separators.
                path: String,
                /// Number of patch bytes.
                bytes: u64,
            },
            /// Delete a workspace-relative file.
            DeleteWorkspaceFile {
                /// Workspace-relative path using `/` separators.
                path: String,
            },
            /// Install a system package by lowercase package name.
            InstallPackage {
                /// Distribution package name.
                name: String,
            },
            /// Enable or disable an interface.
            ConfigureNetwork {
                /// Interface name, such as `eth0` or `wlan0`.
                interface: String,
                /// True to enable networking, false to disable it.
                enable: bool,
            },
            /// Reboot the machine.
            Reboot,
            /// Shut down the machine.
            Shutdown,
            /// Run a sandboxed argv vector. This still requires explicit confirmation.
            RunSandboxedCommand {
                /// Argument vector; argv\[0\] is the executable.
                argv: Vec<String>,
            },
        }

        impl Action {
            /// Returns the capability required to attempt this action.
            pub fn required(&self) -> Capability {
                match self {
                    Action::ReadStatus { .. } => Capability::Observe,
                    Action::SearchLocalIndex { .. } | Action::ReadWorkspaceFile { .. } => {
                        Capability::ReadWorkspace
                    }
                    Action::WriteWorkspaceFile { .. }
                    | Action::ApplyWorkspacePatch { .. }
                    | Action::DeleteWorkspaceFile { .. } => Capability::WriteWorkspace,
                    Action::InstallPackage { .. } | Action::RunSandboxedCommand { .. } => {
                        Capability::ModifySystem
                    }
                    Action::ConfigureNetwork { .. } => Capability::Network,
                    Action::Reboot | Action::Shutdown => Capability::Power,
                }
            }

            /// Returns the confirmation consequence, if any, for this action.
            pub fn consequence(&self) -> Option<Consequence> {
                match self {
                    Action::ReadStatus { .. }
                    | Action::SearchLocalIndex { .. }
                    | Action::ReadWorkspaceFile { .. } => None,
                    Action::WriteWorkspaceFile { .. } | Action::ApplyWorkspacePatch { .. } => {
                        Some(Consequence::WorkspaceWrite)
                    }
                    Action::DeleteWorkspaceFile { .. } => Some(Consequence::EraseData),
                    Action::ConfigureNetwork { .. } => Some(Consequence::NetworkChange),
                    Action::InstallPackage { .. } => Some(Consequence::SystemChange),
                    Action::RunSandboxedCommand { .. } => Some(Consequence::SpawnProcess),
                    Action::Reboot | Action::Shutdown => Some(Consequence::Power),
                }
            }

            /// Returns a short human-readable summary suitable for confirmation UI.
            pub fn summary(&self) -> String {
                match self {
                    Action::ReadStatus { name } => format!("read status `{name}`"),
                    Action::SearchLocalIndex { query } => {
                        format!("search local index with a {}-byte query", query.len())
                    }
                    Action::ReadWorkspaceFile { path } => format!("read workspace file `{path}`"),
                    Action::WriteWorkspaceFile { path, bytes } => {
                        format!("write {bytes} bytes to workspace file `{path}`")
                    }
                    Action::ApplyWorkspacePatch { path, bytes } => {
                        format!("apply a {bytes}-byte patch to workspace file `{path}`")
                    }
                    Action::DeleteWorkspaceFile { path } => {
                        format!("delete workspace file `{path}`")
                    }
                    Action::InstallPackage { name } => format!("install package `{name}`"),
                    Action::ConfigureNetwork { interface, enable } => {
                        if *enable {
                            format!("enable networking on interface `{interface}`")
                        } else {
                            format!("disable networking on interface `{interface}`")
                        }
                    }
                    Action::Reboot => "reboot the system".to_string(),
                    Action::Shutdown => "shut down the system".to_string(),
                    Action::RunSandboxedCommand { argv } => {
                        format!("run a sandboxed command with {} argument(s)", argv.len())
                    }
                }
            }
        }

        /// Typed error produced before an action may be considered by policy.
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub enum PolicyError {
            /// The raw input contains no non-whitespace characters.
            EmptyInput,
            /// The raw input exceeds the configured byte budget.
            InputTooLong {
                /// Maximum allowed bytes.
                max: usize,
                /// Actual bytes.
                actual: usize,
            },
            /// A typed field exceeds its byte budget.
            TooLong {
                /// Field name.
                field: &'static str,
                /// Maximum allowed bytes.
                max: usize,
                /// Actual bytes.
                actual: usize,
            },
            /// A field contains a control or otherwise invalid character.
            InvalidCharacter {
                /// Field name.
                field: &'static str,
            },
            /// A workspace path is not confined to an MOULI workspace.
            InvalidPath {
                /// Offending path.
                path: String,
                /// Short reason.
                reason: &'static str,
            },
            /// A field is structurally invalid.
            InvalidArgument {
                /// Field name.
                field: &'static str,
                /// Short reason.
                reason: &'static str,
            },
            /// A confirmation token was supplied that was not issued by this policy
            /// for this exact action.
            ConfirmationMismatch,
        }

        impl fmt::Display for PolicyError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    PolicyError::EmptyInput => f.write_str("input is empty"),
                    PolicyError::InputTooLong { max, actual } => {
                        write!(f, "input is {actual} bytes, exceeds {max} bytes")
                    }
                    PolicyError::TooLong { field, max, actual } => {
                        write!(f, "{field} is {actual} bytes, exceeds {max} bytes")
                    }
                    PolicyError::InvalidCharacter { field } => {
                        write!(
                            f,
                            "{field} contains a control or otherwise invalid character"
                        )
                    }
                    PolicyError::InvalidPath { path, reason } => {
                        write!(f, "invalid workspace path `{path}`: {reason}")
                    }
                    PolicyError::InvalidArgument { field, reason } => {
                        write!(f, "invalid {field}: {reason}")
                    }
                    PolicyError::ConfirmationMismatch => {
                        f.write_str("confirmation does not match this action")
                    }
                }
            }
        }

        impl Error for PolicyError {}

        /// Result of reviewing an action against a granted capability set.
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub enum Decision {
            /// The action may proceed without prompting.
            Allow {
                /// Human-readable action summary.
                summary: String,
            },
            /// The action is well-formed and authorized but needs explicit user confirmation.
            Confirm {
                /// Human-readable action summary.
                summary: String,
                /// Kind of consequence being confirmed.
                consequence: Consequence,
                /// Token to pass back to [`Policy::evaluate`] once the user approves.
                /// It is valid only for this exact action and this policy instance.
                token: ConfirmToken,
            },
            /// The action is well-formed but not authorized by the granted capabilities.
            Deny {
                /// Capabilities missing from the grant.
                missing: CapabilitySet,
                /// Human-readable action summary.
                summary: String,
            },
        }

        /// Proof that the user approved one specific action shown by [`Decision::Confirm`].
        ///
        /// The value is a keyed hash of the action under a per-policy random key, so a
        /// token for one action cannot be reused for another, and a caller without the
        /// policy instance cannot forge one.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
        pub struct ConfirmToken(u64);

        /// Configuration for the policy gate.
        #[derive(Clone, Debug)]
        pub struct Policy {
            max_input_bytes: usize,
            confirm_key: RandomState,
        }

        impl Policy {
            /// Creates a policy with production defaults.
            pub fn new() -> Self {
                Self::default()
            }

            /// Returns the configured raw-input byte budget.
            pub fn max_input_bytes(&self) -> usize {
                self.max_input_bytes
            }

            /// Creates a policy with a custom raw-input byte budget.
            ///
            /// The budget must be between 64 and [`MAX_INPUT_BYTES`].
            pub fn with_max_input_bytes(
                mut self,
                max_input_bytes: usize,
            ) -> Result<Self, PolicyError> {
                if !(64..=MAX_INPUT_BYTES).contains(&max_input_bytes) {
                    return Err(PolicyError::InvalidArgument {
                        field: "max_input_bytes",
                        reason: "must be between 64 and MAX_INPUT_BYTES",
                    });
                }
                self.max_input_bytes = max_input_bytes;
                Ok(self)
            }

            /// Reviews `action` derived from `raw` under `granted`.
            ///
            /// `confirmation` must be the token from a prior [`Decision::Confirm`] for
            /// this exact action, passed only after the user explicitly approved it.
            /// A token issued for a different action, or by a different policy
            /// instance, returns [`PolicyError::ConfirmationMismatch`].
            pub fn evaluate(
                &self,
                raw: &str,
                action: &Action,
                granted: CapabilitySet,
                confirmation: Option<ConfirmToken>,
            ) -> Result<Decision, PolicyError> {
                validate_raw(raw, self.max_input_bytes)?;
                validate_action(action)?;

                let required = action.required();
                if !granted.contains(required) {
                    return Ok(Decision::Deny {
                        missing: CapabilitySet::from_slice(&[required]),
                        summary: action.summary(),
                    });
                }

                if let Some(consequence) = action.consequence() {
                    let expected = self.token_for(action, consequence);
                    match confirmation {
                        None => {
                            return Ok(Decision::Confirm {
                                summary: action.summary(),
                                consequence,
                                token: expected,
                            })
                        }
                        Some(token) if token != expected => {
                            return Err(PolicyError::ConfirmationMismatch)
                        }
                        Some(_) => {}
                    }
                }

                Ok(Decision::Allow {
                    summary: action.summary(),
                })
            }

            fn token_for(&self, action: &Action, consequence: Consequence) -> ConfirmToken {
                ConfirmToken(self.confirm_key.hash_one((action, consequence)))
            }
        }

        impl Default for Policy {
            fn default() -> Self {
                Self {
                    max_input_bytes: MAX_INPUT_BYTES,
                    confirm_key: RandomState::new(),
                }
            }
        }

        fn validate_len(field: &'static str, value: &str, max: usize) -> Result<(), PolicyError> {
            let actual = value.len();
            if actual > max {
                return Err(PolicyError::TooLong { field, max, actual });
            }
            Ok(())
        }

        fn validate_raw(raw: &str, max_input_bytes: usize) -> Result<(), PolicyError> {
            if raw.trim().is_empty() {
                return Err(PolicyError::EmptyInput);
            }
            let actual = raw.len();
            if actual > max_input_bytes {
                return Err(PolicyError::InputTooLong {
                    max: max_input_bytes,
                    actual,
                });
            }
            for byte in raw.bytes() {
                let allowed_ws = byte == b' ' || byte == b'\n' || byte == b'\t';
                if (byte < 0x20 && !allowed_ws) || byte == 0x7f {
                    return Err(PolicyError::InvalidCharacter { field: "input" });
                }
            }
            Ok(())
        }

        fn validate_printable(
            field: &'static str,
            value: &str,
            max: usize,
        ) -> Result<(), PolicyError> {
            validate_len(field, value, max)?;
            if value
                .bytes()
                .any(|byte| (byte < 0x20 && byte != b' ') || byte == 0x7f)
            {
                return Err(PolicyError::InvalidCharacter { field });
            }
            Ok(())
        }

        fn validate_symbol(field: &'static str, value: &str) -> Result<(), PolicyError> {
            validate_len(field, value, MAX_SYMBOL_BYTES)?;
            let mut bytes = value.bytes();
            match bytes.next() {
                Some(byte) if byte.is_ascii_alphanumeric() => {}
                _ => {
                    return Err(PolicyError::InvalidArgument {
                        field,
                        reason: "must start with an ASCII letter or digit",
                    })
                }
            }
            if !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            {
                return Err(PolicyError::InvalidArgument {
                    field,
                    reason: "only ASCII letters, digits, `_`, `-`, and `.` are allowed",
                });
            }
            Ok(())
        }

        fn validate_package_name(value: &str) -> Result<(), PolicyError> {
            validate_len("package", value, MAX_PACKAGE_BYTES)?;
            let mut bytes = value.bytes();
            match bytes.next() {
                Some(byte) if byte.is_ascii_lowercase() || byte.is_ascii_digit() => {}
                _ => {
                    return Err(PolicyError::InvalidArgument {
                        field: "package",
                        reason: "must start with a lowercase ASCII letter or digit",
                    })
                }
            }
            if !bytes.all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.' | b'+')
            }) {
                return Err(PolicyError::InvalidArgument {
                    field: "package",
                    reason:
                        "only lowercase ASCII letters, digits, `-`, `_`, `.`, and `+` are allowed",
                });
            }
            Ok(())
        }

        fn validate_workspace_path(path: &str) -> Result<(), PolicyError> {
            validate_len("path", path, MAX_PATH_BYTES)?;
            if path.starts_with('/') {
                return Err(PolicyError::InvalidPath {
                    path: path.to_string(),
                    reason: "absolute paths are not allowed",
                });
            }
            if path.contains('\\') {
                return Err(PolicyError::InvalidPath {
                    path: path.to_string(),
                    reason: "backslashes are not workspace path separators",
                });
            }

            let mut segments = 0usize;
            for segment in path.split('/') {
                segments += 1;
                if segments > 32 {
                    return Err(PolicyError::InvalidPath {
                        path: path.to_string(),
                        reason: "path has too many segments",
                    });
                }
                if segment.is_empty() {
                    return Err(PolicyError::InvalidPath {
                        path: path.to_string(),
                        reason: "empty path segment",
                    });
                }
                if segment == "." || segment == ".." {
                    return Err(PolicyError::InvalidPath {
                        path: path.to_string(),
                        reason: "path traversal segments are not allowed",
                    });
                }
                if segment
                    .bytes()
                    .any(|byte| byte < 0x20 || byte == 0x7f || byte == b'\\')
                {
                    return Err(PolicyError::InvalidPath {
                        path: path.to_string(),
                        reason: "control characters are not allowed",
                    });
                }
            }
            Ok(())
        }

        fn validate_write_bytes(field: &'static str, bytes: u64) -> Result<(), PolicyError> {
            if bytes > MAX_WRITE_BYTES {
                return Err(PolicyError::InvalidArgument {
                    field,
                    reason: "exceeds the maximum single-operation write budget",
                });
            }
            Ok(())
        }

        fn validate_command_argv(argv: &[String]) -> Result<(), PolicyError> {
            if argv.is_empty() {
                return Err(PolicyError::InvalidArgument {
                    field: "argv",
                    reason: "must contain at least one argument",
                });
            }
            if argv.len() > MAX_COMMAND_ARGS {
                return Err(PolicyError::InvalidArgument {
                    field: "argv",
                    reason: "too many arguments",
                });
            }
            if let Some(first) = argv.first() {
                if first.starts_with('-') {
                    return Err(PolicyError::InvalidArgument {
                        field: "argv",
                        reason: "executable must not start with `-`",
                    });
                }
            }

            let mut total = 0usize;
            for argument in argv {
                validate_len("argv", argument, MAX_COMMAND_ARG_BYTES)?;
                if argument.is_empty() {
                    return Err(PolicyError::InvalidArgument {
                        field: "argv",
                        reason: "empty argument",
                    });
                }
                if argument.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
                    return Err(PolicyError::InvalidCharacter { field: "argv" });
                }
                total = total
                    .checked_add(argument.len())
                    .ok_or(PolicyError::InvalidArgument {
                        field: "argv",
                        reason: "argument byte count overflow",
                    })?;
                if total > MAX_COMMAND_TOTAL_BYTES {
                    return Err(PolicyError::InvalidArgument {
                        field: "argv",
                        reason: "total command bytes exceed the sandbox budget",
                    });
                }
            }
            Ok(())
        }

        fn validate_action(action: &Action) -> Result<(), PolicyError> {
            match action {
                Action::ReadStatus { name } => validate_symbol("status", name),
                Action::SearchLocalIndex { query } => {
                    validate_len("query", query, MAX_QUERY_BYTES)?;
                    if query.trim().is_empty() {
                        return Err(PolicyError::InvalidArgument {
                            field: "query",
                            reason: "must not be blank",
                        });
                    }
                    validate_printable("query", query, MAX_QUERY_BYTES)
                }
                Action::ReadWorkspaceFile { path } | Action::DeleteWorkspaceFile { path } => {
                    validate_workspace_path(path)
                }
                Action::WriteWorkspaceFile { path, bytes } => {
                    validate_workspace_path(path)?;
                    validate_write_bytes("bytes", *bytes)
                }
                Action::ApplyWorkspacePatch { path, bytes } => {
                    validate_workspace_path(path)?;
                    validate_write_bytes("bytes", *bytes)
                }
                Action::InstallPackage { name } => validate_package_name(name),
                Action::ConfigureNetwork { interface, .. } => {
                    validate_symbol("interface", interface)
                }
                Action::Reboot | Action::Shutdown => Ok(()),
                Action::RunSandboxedCommand { argv } => validate_command_argv(argv),
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn granted(capabilities: &[Capability]) -> CapabilitySet {
                CapabilitySet::from_slice(capabilities)
            }

            #[test]
            fn allows_read_only_status_when_observed() {
                let action = Action::ReadStatus {
                    name: "cpu".to_string(),
                };
                let decision = Policy::new()
                    .evaluate("show cpu", &action, granted(&[Capability::Observe]), None)
                    .expect("read-only action should be well-formed");
                assert!(matches!(decision, Decision::Allow { .. }));
            }

            #[test]
            fn denies_missing_capability_without_prompting() {
                let action = Action::ConfigureNetwork {
                    interface: "wlan0".to_string(),
                    enable: true,
                };
                let decision = Policy::new()
                    .evaluate("go online", &action, granted(&[Capability::Observe]), None)
                    .expect("action is well-formed but unauthorized");
                assert!(
                    matches!(decision, Decision::Deny { missing, .. } if missing.contains(Capability::Network))
                );
            }

            #[test]
            fn confirmation_is_required_for_destructive_action() {
                let action = Action::DeleteWorkspaceFile {
                    path: "notes/tmp.txt".to_string(),
                };
                let decision = Policy::new()
                    .evaluate(
                        "delete the temp note",
                        &action,
                        granted(&[Capability::WriteWorkspace]),
                        None,
                    )
                    .expect("delete action is well-formed");
                assert!(matches!(
                    decision,
                    Decision::Confirm {
                        ref summary,
                        consequence: Consequence::EraseData,
                        ..
                    } if summary == "delete workspace file `notes/tmp.txt`"
                ));
            }

            fn confirm_token(
                policy: &Policy,
                action: &Action,
                caps: CapabilitySet,
            ) -> ConfirmToken {
                match policy.evaluate("approve", action, caps, None) {
                    Ok(Decision::Confirm { token, .. }) => token,
                    other => panic!("expected Confirm, got {other:?}"),
                }
            }

            #[test]
            fn token_for_one_action_is_rejected_for_another() {
                let policy = Policy::new();
                let caps = granted(&[Capability::WriteWorkspace]);
                let shown = Action::DeleteWorkspaceFile {
                    path: "notes/tmp.txt".to_string(),
                };
                let swapped = Action::DeleteWorkspaceFile {
                    path: "notes/important.txt".to_string(),
                };
                let token = confirm_token(&policy, &shown, caps);
                assert_eq!(
                    policy.evaluate("delete", &swapped, caps, Some(token)),
                    Err(PolicyError::ConfirmationMismatch)
                );
                assert!(matches!(
                    policy.evaluate("delete", &shown, caps, Some(token)),
                    Ok(Decision::Allow { .. })
                ));
            }

            #[test]
            fn token_from_another_policy_instance_is_rejected() {
                let caps = granted(&[Capability::Power]);
                let token = confirm_token(&Policy::new(), &Action::Reboot, caps);
                assert_eq!(
                    Policy::new().evaluate("restart", &Action::Reboot, caps, Some(token)),
                    Err(PolicyError::ConfirmationMismatch)
                );
            }

            #[test]
            fn confirmed_write_is_allowed() {
                let action = Action::WriteWorkspaceFile {
                    path: "src/main.rs".to_string(),
                    bytes: 32,
                };
                let policy = Policy::new();
                let caps = granted(&[Capability::WriteWorkspace]);
                let token = confirm_token(&policy, &action, caps);
                let decision = policy
                    .evaluate("write main", &action, caps, Some(token))
                    .expect("write action is well-formed");
                assert!(matches!(decision, Decision::Allow { .. }));
            }

            #[test]
            fn workspace_patch_requires_confirmation_even_when_small() {
                let action = Action::ApplyWorkspacePatch {
                    path: "README.md".to_string(),
                    bytes: 12,
                };
                let decision = Policy::new()
                    .evaluate(
                        "apply readme patch",
                        &action,
                        granted(&[Capability::WriteWorkspace]),
                        None,
                    )
                    .expect("patch is well-formed");
                assert!(matches!(
                    decision,
                    Decision::Confirm {
                        consequence: Consequence::WorkspaceWrite,
                        ..
                    }
                ));
            }

            #[test]
            fn rejects_empty_and_control_laden_raw_input() {
                let action = Action::ReadStatus {
                    name: "cpu".to_string(),
                };
                assert_eq!(
                    Policy::new().evaluate("   \n\t", &action, CapabilitySet::all(), None),
                    Err(PolicyError::EmptyInput)
                );
                assert_eq!(
                    Policy::new().evaluate("show\x07cpu", &action, CapabilitySet::all(), None),
                    Err(PolicyError::InvalidCharacter { field: "input" })
                );
            }

            #[test]
            fn rejects_overlong_raw_input() {
                let raw = "a".repeat(MAX_INPUT_BYTES + 1);
                let action = Action::ReadStatus {
                    name: "cpu".to_string(),
                };
                assert_eq!(
                    Policy::new().evaluate(&raw, &action, CapabilitySet::all(), None),
                    Err(PolicyError::InputTooLong {
                        max: MAX_INPUT_BYTES,
                        actual: MAX_INPUT_BYTES + 1,
                    })
                );
            }

            #[test]
            fn rejects_path_traversal_and_absolute_paths() {
                let traversal = Action::ReadWorkspaceFile {
                    path: "../secrets".to_string(),
                };
                let absolute = Action::ReadWorkspaceFile {
                    path: "/etc/shadow".to_string(),
                };
                assert!(matches!(
                    Policy::new().evaluate(
                        "read",
                        &traversal,
                        granted(&[Capability::ReadWorkspace]),
                        None
                    ),
                    Err(PolicyError::InvalidPath { .. })
                ));
                assert!(matches!(
                    Policy::new().evaluate(
                        "read",
                        &absolute,
                        granted(&[Capability::ReadWorkspace]),
                        None
                    ),
                    Err(PolicyError::InvalidPath { .. })
                ));
            }

            #[test]
            fn rejects_empty_segments_backslashes_and_dot_segments() {
                for path in ["a//b", "a/./b", "./b", "a/b/", "a\\b", ""] {
                    let action = Action::ReadWorkspaceFile {
                        path: path.to_string(),
                    };
                    assert!(matches!(
                        Policy::new().evaluate(
                            "read",
                            &action,
                            granted(&[Capability::ReadWorkspace]),
                            None
                        ),
                        Err(PolicyError::InvalidPath { .. })
                    ));
                }
            }

            #[test]
            fn accepts_nested_unix_style_workspace_paths() {
                let action = Action::ReadWorkspaceFile {
                    path: "docs/notes/äon.txt".to_string(),
                };
                let decision = Policy::new()
                    .evaluate(
                        "read note",
                        &action,
                        granted(&[Capability::ReadWorkspace]),
                        None,
                    )
                    .expect("nested UTF-8 workspace path should be valid");
                assert!(matches!(decision, Decision::Allow { .. }));
            }

            #[test]
            fn rejects_uppercase_package_names() {
                let action = Action::InstallPackage {
                    name: "Vim".to_string(),
                };
                assert_eq!(
                    Policy::new().evaluate(
                        "install vim",
                        &action,
                        granted(&[Capability::ModifySystem]),
                        None
                    ),
                    Err(PolicyError::InvalidArgument {
                        field: "package",
                        reason: "must start with a lowercase ASCII letter or digit",
                    })
                );
            }

            #[test]
            fn rejects_empty_sandbox_argv() {
                let action = Action::RunSandboxedCommand { argv: Vec::new() };
                assert_eq!(
                    Policy::new().evaluate(
                        "run command",
                        &action,
                        granted(&[Capability::ModifySystem]),
                        None
                    ),
                    Err(PolicyError::InvalidArgument {
                        field: "argv",
                        reason: "must contain at least one argument",
                    })
                );
            }

            #[test]
            fn rejects_leading_dash_executable() {
                let action = Action::RunSandboxedCommand {
                    argv: vec!["-rm".to_string()],
                };
                assert_eq!(
                    Policy::new().evaluate(
                        "run command",
                        &action,
                        granted(&[Capability::ModifySystem]),
                        None
                    ),
                    Err(PolicyError::InvalidArgument {
                        field: "argv",
                        reason: "executable must not start with `-`",
                    })
                );
            }

            #[test]
            fn rejects_oversized_workspace_write() {
                let action = Action::WriteWorkspaceFile {
                    path: "big.bin".to_string(),
                    bytes: MAX_WRITE_BYTES + 1,
                };
                assert_eq!(
                    Policy::new().evaluate(
                        "write",
                        &action,
                        granted(&[Capability::WriteWorkspace]),
                        None
                    ),
                    Err(PolicyError::InvalidArgument {
                        field: "bytes",
                        reason: "exceeds the maximum single-operation write budget",
                    })
                );
            }

            #[test]
            fn policy_budget_must_be_sane() {
                assert!(matches!(
                    Policy::new().with_max_input_bytes(10),
                    Err(PolicyError::InvalidArgument {
                        field: "max_input_bytes",
                        ..
                    })
                ));
                assert!(Policy::new().with_max_input_bytes(64).is_ok());
                let policy = Policy::new()
                    .with_max_input_bytes(64)
                    .expect("64-byte budget is valid");
                let action = Action::ReadStatus {
                    name: "cpu".to_string(),
                };
                let raw = "a".repeat(65);
                assert_eq!(
                    policy.evaluate(&raw, &action, granted(&[Capability::Observe]), None),
                    Err(PolicyError::InputTooLong {
                        max: 64,
                        actual: 65
                    })
                );
            }

            #[test]
            fn capability_set_contains_union_and_empty() {
                let set = CapabilitySet::empty()
                    .with(Capability::Observe)
                    .with(Capability::Power);
                assert!(!set.is_empty());
                assert!(set.contains(Capability::Observe));
                assert!(set.contains(Capability::Power));
                assert!(!set.contains(Capability::Network));
                assert!(CapabilitySet::all().contains(Capability::ModifySystem));
                assert!(CapabilitySet::empty().is_empty());
            }

            #[test]
            fn power_actions_require_power_capability_and_confirmation() {
                let action = Action::Reboot;
                let denied = Policy::new()
                    .evaluate("restart", &action, granted(&[Capability::Observe]), None)
                    .expect("reboot is well-formed");
                assert!(
                    matches!(denied, Decision::Deny { missing, .. } if missing.contains(Capability::Power))
                );

                let unconfirmed = Policy::new()
                    .evaluate("restart", &action, granted(&[Capability::Power]), None)
                    .expect("reboot is well-formed");
                assert!(matches!(
                    unconfirmed,
                    Decision::Confirm {
                        consequence: Consequence::Power,
                        ..
                    }
                ));
            }
        }
    }
}

#[allow(clippy::module_inception)]
pub mod sched {
    //! MOULI heterogeneous compute placement.
    //!
    //! A laptop has a few very different compute units: CPU cores, an
    //! integrated GPU sharing RAM, maybe a discrete GPU with its own VRAM, maybe
    //! an NPU. Work items (inference layers, agent jobs, media pipelines) prefer
    //! some kinds of unit, need memory, and may or may not be movable.
    //!
    //! [`Topology::place`] picks the best unit for a task; [`Topology::rebalance`]
    //! moves migratable tasks off units that run too hot or too full.
    #![forbid(unsafe_code)]

    use core::fmt;
    use std::collections::BTreeMap;

    /// Kind of compute unit.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub enum UnitKind {
        /// CPU cores.
        Cpu,
        /// Integrated GPU (shared memory).
        IGpu,
        /// Discrete GPU (own memory).
        DGpu,
        /// Neural accelerator.
        Npu,
    }

    /// What a task mostly does.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Work {
        /// Dense tensor math (matmul, attention).
        Tensor,
        /// Branchy scalar code.
        Scalar,
        /// Memory-bound streaming.
        Memory,
    }

    impl Work {
        /// Relative speed of `kind` for this work (higher is better).
        #[must_use]
        pub fn affinity(self, kind: UnitKind) -> f32 {
            match (self, kind) {
                (Self::Tensor, UnitKind::DGpu) => 10.0,
                (Self::Tensor, UnitKind::Npu) => 6.0,
                (Self::Tensor, UnitKind::IGpu) => 4.0,
                (Self::Tensor, UnitKind::Cpu) => 1.0,
                (Self::Scalar, UnitKind::Cpu) => 10.0,
                (Self::Scalar, UnitKind::IGpu | UnitKind::DGpu) => 0.5,
                (Self::Scalar, UnitKind::Npu) => 0.1,
                (Self::Memory, UnitKind::DGpu) => 6.0,
                (Self::Memory, UnitKind::IGpu | UnitKind::Cpu) => 3.0,
                (Self::Memory, UnitKind::Npu) => 1.0,
            }
        }
    }

    /// A compute unit and its live state.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Unit {
        /// Name ("cpu0", "igpu", "dgpu").
        pub name: String,
        /// Kind.
        pub kind: UnitKind,
        /// Memory usable by tasks on this unit, bytes.
        pub mem_bytes: u64,
        /// Temperature in Celsius.
        pub temp_c: f32,
        /// Temperature at which work should move away.
        pub temp_limit_c: f32,
        /// Unit is powered and usable (e.g. dGPU may be off on battery).
        pub online: bool,
    }

    /// A schedulable task.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Task {
        /// Id.
        pub id: u64,
        /// Work profile.
        pub work: Work,
        /// Memory needed, bytes.
        pub mem_bytes: u64,
        /// Share of a unit's throughput it uses (0..=1).
        pub load: f32,
        /// May be moved after placement.
        pub migratable: bool,
    }

    /// Placement errors.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SchedError {
        /// No online unit has room.
        NoCapacity,
        /// Unknown unit or task.
        Unknown,
        /// Duplicate id or name.
        Duplicate,
    }

    impl fmt::Display for SchedError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(match self {
                Self::NoCapacity => "no unit has capacity",
                Self::Unknown => "unknown unit or task",
                Self::Duplicate => "duplicate id",
            })
        }
    }

    impl std::error::Error for SchedError {}

    /// A move made by [`Topology::rebalance`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Migration {
        /// Task moved.
        pub task: u64,
        /// From unit.
        pub from: String,
        /// To unit.
        pub to: String,
    }

    /// Units, tasks and the current placement.
    #[derive(Debug, Clone, Default)]
    pub struct Topology {
        units: Vec<Unit>,
        tasks: BTreeMap<u64, (Task, usize)>,
    }

    impl Topology {
        /// Empty topology.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Add a unit.
        pub fn add_unit(&mut self, u: Unit) -> Result<(), SchedError> {
            if self.units.iter().any(|x| x.name == u.name) {
                return Err(SchedError::Duplicate);
            }
            self.units.push(u);
            Ok(())
        }

        fn unit_index(&self, name: &str) -> Result<usize, SchedError> {
            self.units
                .iter()
                .position(|u| u.name == name)
                .ok_or(SchedError::Unknown)
        }

        /// Memory and load in use on unit `i`.
        fn usage(&self, i: usize) -> (u64, f32) {
            self.tasks
                .values()
                .filter(|(_, u)| *u == i)
                .fold((0, 0.0), |(m, l), (t, _)| (m + t.mem_bytes, l + t.load))
        }

        fn fits(&self, i: usize, t: &Task) -> bool {
            let u = &self.units[i];
            let (mem, load) = self.usage(i);
            u.online
                && u.temp_c < u.temp_limit_c
                && mem.saturating_add(t.mem_bytes) <= u.mem_bytes
                && load + t.load <= 1.0 + 1e-6
        }

        /// Score of running `t` on unit `i`: affinity, discounted by load and heat.
        fn score(&self, i: usize, t: &Task) -> f32 {
            let u = &self.units[i];
            let (_, load) = self.usage(i);
            let heat = (u.temp_c / u.temp_limit_c).clamp(0.0, 1.0);
            t.work.affinity(u.kind) * (1.0 - 0.5 * load) * (1.0 - 0.5 * heat)
        }

        fn best_unit(&self, t: &Task, exclude: Option<usize>) -> Option<usize> {
            (0..self.units.len())
                .filter(|&i| Some(i) != exclude && self.fits(i, t))
                .max_by(|&a, &b| self.score(a, t).total_cmp(&self.score(b, t)))
        }

        /// Place a new task on the best fitting unit; returns the unit name.
        pub fn place(&mut self, t: Task) -> Result<&str, SchedError> {
            if self.tasks.contains_key(&t.id) {
                return Err(SchedError::Duplicate);
            }
            let i = self.best_unit(&t, None).ok_or(SchedError::NoCapacity)?;
            self.tasks.insert(t.id, (t, i));
            Ok(&self.units[i].name)
        }

        /// Remove a finished task.
        pub fn finish(&mut self, id: u64) -> Result<Task, SchedError> {
            self.tasks
                .remove(&id)
                .map(|(t, _)| t)
                .ok_or(SchedError::Unknown)
        }

        /// Update a unit's sensor readings.
        pub fn update_unit(
            &mut self,
            name: &str,
            temp_c: f32,
            online: bool,
        ) -> Result<(), SchedError> {
            let i = self.unit_index(name)?;
            self.units[i].temp_c = temp_c;
            self.units[i].online = online;
            Ok(())
        }

        /// Move migratable tasks off units that are offline, too hot, or over
        /// capacity, to the best other unit that fits. Pinned tasks stay.
        pub fn rebalance(&mut self) -> Vec<Migration> {
            let mut moves = Vec::new();
            let ids: Vec<u64> = self.tasks.keys().copied().collect();
            for id in ids {
                let (task, from) = match self.tasks.get(&id) {
                    Some((t, u)) => (t.clone(), *u),
                    None => continue,
                };
                let u = &self.units[from];
                let (mem, load) = self.usage(from);
                let stressed = !u.online
                    || u.temp_c >= u.temp_limit_c
                    || mem > u.mem_bytes
                    || load > 1.0 + 1e-6;
                if !stressed || !task.migratable {
                    continue;
                }
                // Evaluate fit without this task counted on its current unit.
                self.tasks.remove(&id);
                match self.best_unit(&task, Some(from)) {
                    Some(to) => {
                        moves.push(Migration {
                            task: id,
                            from: self.units[from].name.clone(),
                            to: self.units[to].name.clone(),
                        });
                        self.tasks.insert(id, (task, to));
                    }
                    None => {
                        self.tasks.insert(id, (task, from));
                    }
                }
            }
            moves
        }

        /// Unit currently running `id`.
        #[must_use]
        pub fn unit_of(&self, id: u64) -> Option<&str> {
            self.tasks
                .get(&id)
                .map(|(_, u)| self.units[*u].name.as_str())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn unit(name: &str, kind: UnitKind, mem: u64) -> Unit {
            Unit {
                name: name.into(),
                kind,
                mem_bytes: mem,
                temp_c: 50.0,
                temp_limit_c: 90.0,
                online: true,
            }
        }

        fn task(id: u64, work: Work, mem: u64, migratable: bool) -> Task {
            Task {
                id,
                work,
                mem_bytes: mem,
                load: 0.4,
                migratable,
            }
        }

        fn laptop() -> Topology {
            let mut t = Topology::new();
            t.add_unit(unit("cpu", UnitKind::Cpu, 16 << 30)).unwrap();
            t.add_unit(unit("igpu", UnitKind::IGpu, 4 << 30)).unwrap();
            t.add_unit(unit("dgpu", UnitKind::DGpu, 8 << 30)).unwrap();
            t
        }

        #[test]
        fn placement_follows_affinity_and_memory() {
            let mut t = laptop();
            assert_eq!(
                t.place(task(1, Work::Tensor, 6 << 30, true)).unwrap(),
                "dgpu"
            );
            assert_eq!(
                t.place(task(2, Work::Scalar, 1 << 30, true)).unwrap(),
                "cpu"
            );
            // Too big for any GPU now: dGPU has 2 GiB left, iGPU 4 GiB.
            assert_eq!(
                t.place(task(3, Work::Tensor, 5 << 30, true)).unwrap(),
                "cpu"
            );
            assert_eq!(
                t.place(task(3, Work::Tensor, 1, true)),
                Err(SchedError::Duplicate)
            );
            assert_eq!(
                t.place(task(9, Work::Tensor, 64 << 30, true)),
                Err(SchedError::NoCapacity)
            );
        }

        #[test]
        fn hot_or_offline_units_shed_migratable_work() {
            let mut t = laptop();
            t.place(task(1, Work::Tensor, 1 << 30, true)).unwrap();
            t.place(task(2, Work::Tensor, 1 << 30, false)).unwrap();
            assert_eq!(t.unit_of(1), Some("dgpu"));
            t.update_unit("dgpu", 95.0, true).unwrap();
            let moves = t.rebalance();
            assert_eq!(
                moves,
                vec![Migration {
                    task: 1,
                    from: "dgpu".into(),
                    to: "igpu".into()
                }]
            );
            assert_eq!(t.unit_of(2), Some("dgpu"));
            t.update_unit("igpu", 50.0, false).unwrap();
            let moves = t.rebalance();
            assert_eq!(moves[0].to, "cpu");
            assert!(t.finish(1).is_ok());
            assert!(t.finish(1).is_err());
        }
    }
}

#[allow(clippy::module_inception)]
pub mod store {
    //! MOULI system state store.
    //!
    //! Settings, agent jobs and other small OS state live in memory for speed
    //! and are made durable through an append-only log:
    //!
    //! ```text
    //! record = crc32 (u32 LE) | op (u8: 1 put, 2 delete) | key_len (u32 LE) | val_len (u32 LE) | key | val
    //! ```
    //!
    //! Replay stops at the first torn or corrupt record (a crash mid-write), so
    //! the store always recovers to the last complete write. [`Store::compact`]
    //! rewrites the log with only live keys.
    #![forbid(unsafe_code)]

    use std::collections::BTreeMap;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};

    /// CRC-32 (IEEE, reflected) without tables.
    #[must_use]
    pub fn crc32(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                let mask = (c & 1).wrapping_neg();
                c = (c >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !c
    }

    const PUT: u8 = 1;
    const DEL: u8 = 2;
    const HEADER: usize = 13;

    fn encode(op: u8, key: &[u8], val: &[u8]) -> io::Result<Vec<u8>> {
        let kl = u32::try_from(key.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "key too long"))?;
        let vl = u32::try_from(val.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "value too long"))?;
        let mut body = Vec::with_capacity(HEADER - 4 + key.len() + val.len());
        body.push(op);
        body.extend_from_slice(&kl.to_le_bytes());
        body.extend_from_slice(&vl.to_le_bytes());
        body.extend_from_slice(key);
        body.extend_from_slice(val);
        let mut rec = crc32(&body).to_le_bytes().to_vec();
        rec.extend_from_slice(&body);
        Ok(rec)
    }

    /// Apply every complete record in `log`; returns the map and the number of
    /// valid bytes (the rest is a torn tail).
    #[must_use]
    pub fn replay(log: &[u8]) -> (BTreeMap<Vec<u8>, Vec<u8>>, usize) {
        let mut map = BTreeMap::new();
        let mut pos = 0;
        while log.len() - pos >= HEADER {
            let crc = u32::from_le_bytes([log[pos], log[pos + 1], log[pos + 2], log[pos + 3]]);
            let op = log[pos + 4];
            let kl = u32::from_le_bytes([log[pos + 5], log[pos + 6], log[pos + 7], log[pos + 8]])
                as usize;
            let vl = u32::from_le_bytes([log[pos + 9], log[pos + 10], log[pos + 11], log[pos + 12]])
                as usize;
            let Some(end) = (pos + HEADER)
                .checked_add(kl)
                .and_then(|x| x.checked_add(vl))
            else {
                break;
            };
            if end > log.len() || crc32(&log[pos + 4..end]) != crc {
                break;
            }
            let key = log[pos + HEADER..pos + HEADER + kl].to_vec();
            match op {
                PUT => {
                    map.insert(key, log[pos + HEADER + kl..end].to_vec());
                }
                DEL => {
                    map.remove(&key);
                }
                _ => break,
            }
            pos = end;
        }
        (map, pos)
    }

    /// Durable key-value store.
    #[derive(Debug)]
    pub struct Store {
        path: PathBuf,
        file: File,
        map: BTreeMap<Vec<u8>, Vec<u8>>,
        log_bytes: u64,
    }

    impl Store {
        /// Open or create the log at `path`, recovering its contents. A torn
        /// tail is truncated away.
        pub fn open(path: &Path) -> io::Result<Self> {
            let mut data = Vec::new();
            if path.exists() {
                File::open(path)?.read_to_end(&mut data)?;
            }
            let (map, valid) = replay(&data);
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            if valid < data.len() {
                file.set_len(valid as u64)?;
            }
            Ok(Self {
                path: path.to_owned(),
                file,
                map,
                log_bytes: valid as u64,
            })
        }

        fn append(&mut self, rec: &[u8]) -> io::Result<()> {
            self.file.write_all(rec)?;
            self.file.sync_data()?;
            self.log_bytes += rec.len() as u64;
            Ok(())
        }

        /// Set `key` to `val` (durable when this returns).
        pub fn put(&mut self, key: &[u8], val: &[u8]) -> io::Result<()> {
            let rec = encode(PUT, key, val)?;
            self.append(&rec)?;
            self.map.insert(key.to_vec(), val.to_vec());
            Ok(())
        }

        /// Remove `key`. Returns whether it existed.
        pub fn delete(&mut self, key: &[u8]) -> io::Result<bool> {
            if !self.map.contains_key(key) {
                return Ok(false);
            }
            let rec = encode(DEL, key, &[])?;
            self.append(&rec)?;
            self.map.remove(key);
            Ok(true)
        }

        /// Value of `key`.
        #[must_use]
        pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
            self.map.get(key).map(Vec::as_slice)
        }

        /// Keys starting with `prefix`, in order.
        pub fn scan<'a>(
            &'a self,
            prefix: &'a [u8],
        ) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + 'a {
            self.map
                .range(prefix.to_vec()..)
                .take_while(move |(k, _)| k.starts_with(prefix))
                .map(|(k, v)| (k.as_slice(), v.as_slice()))
        }

        /// Number of live keys.
        #[must_use]
        pub fn len(&self) -> usize {
            self.map.len()
        }

        /// True when empty.
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.map.is_empty()
        }

        /// Bytes in the log file.
        #[must_use]
        pub fn log_bytes(&self) -> u64 {
            self.log_bytes
        }

        /// Rewrite the log with only live keys (write to a temp file, then rename).
        pub fn compact(&mut self) -> io::Result<()> {
            let tmp = self.path.with_extension("compact");
            {
                let mut f = File::create(&tmp)?;
                for (k, v) in &self.map {
                    f.write_all(&encode(PUT, k, v)?)?;
                }
                f.sync_all()?;
            }
            fs::rename(&tmp, &self.path)?;
            self.file = OpenOptions::new().append(true).open(&self.path)?;
            self.log_bytes = self.file.metadata()?.len();
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tmp(name: &str) -> PathBuf {
            let p = std::env::temp_dir().join(format!("mouli-store-{}-{name}", std::process::id()));
            let _ = fs::remove_file(&p);
            p
        }

        #[test]
        fn crc_known_value() {
            assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        }

        #[test]
        fn persists_and_recovers() {
            let p = tmp("persist");
            {
                let mut s = Store::open(&p).unwrap();
                s.put(b"agent/1", b"daily digest").unwrap();
                s.put(b"agent/2", b"backup").unwrap();
                s.put(b"theme", b"dark").unwrap();
                assert!(s.delete(b"agent/2").unwrap());
                assert!(!s.delete(b"nope").unwrap());
            }
            let s = Store::open(&p).unwrap();
            assert_eq!(s.get(b"agent/1"), Some(&b"daily digest"[..]));
            assert_eq!(s.get(b"agent/2"), None);
            let agents: Vec<&[u8]> = s.scan(b"agent/").map(|(k, _)| k).collect();
            assert_eq!(agents, vec![&b"agent/1"[..]]);
            drop(s);
            fs::remove_file(&p).unwrap();
        }

        #[test]
        fn torn_tail_is_dropped() {
            let p = tmp("torn");
            {
                let mut s = Store::open(&p).unwrap();
                s.put(b"a", b"1").unwrap();
                s.put(b"b", b"2").unwrap();
            }
            let full = fs::read(&p).unwrap();
            fs::write(&p, &full[..full.len() - 1]).unwrap();
            let mut s = Store::open(&p).unwrap();
            assert_eq!(s.get(b"a"), Some(&b"1"[..]));
            assert_eq!(s.get(b"b"), None);
            s.put(b"c", b"3").unwrap();
            drop(s);
            let s = Store::open(&p).unwrap();
            assert_eq!(s.len(), 2);
            drop(s);
            fs::remove_file(&p).unwrap();
        }

        #[test]
        fn compaction_shrinks_log() {
            let p = tmp("compact");
            let mut s = Store::open(&p).unwrap();
            for i in 0..50u8 {
                s.put(b"counter", &[i]).unwrap();
            }
            let before = s.log_bytes();
            s.compact().unwrap();
            assert!(s.log_bytes() < before / 10);
            s.put(b"x", b"y").unwrap();
            drop(s);
            let s = Store::open(&p).unwrap();
            assert_eq!(s.get(b"counter"), Some(&[49u8][..]));
            assert_eq!(s.get(b"x"), Some(&b"y"[..]));
            drop(s);
            fs::remove_file(&p).unwrap();
        }
    }
}

#[cfg(feature = "mesh")]
#[allow(clippy::module_inception)]
pub mod mesh {
    //! MOULI device mesh.
    //!
    //! A user's devices (laptop, phone, desktop) form a small mesh for sharing
    //! files and offloading work.
    //!
    //! * [`nodes`]: membership by heartbeat, capability and load.
    //! * [`dispatch`]: task assignment with checkpoints and reassignment when a
    //!   node disappears.
    //! * [`files`]: content-addressed chunking, replica placement, repair of corrupt or
    //!   missing chunks from replicas, and version vectors that detect concurrent edits
    //!   instead of silently dropping one.
    //! * [`aggregate`]: collecting the results of a task split into numbered parts.
    //!
    //! Transport is out of scope here; these types are the state machines the
    //! network layer drives. Time is passed in (seconds) for deterministic tests.
    #![forbid(unsafe_code)]

    pub mod aggregate {
        //! Collecting the results of a task that was split into numbered parts.
        //!
        //! A coordinator splits work into `parts`, hands them to nodes, and submits each result by
        //! part index as it arrives (in any order, possibly after a retry). The aggregator accepts each
        //! part once, reports what is still missing, and returns the results in part order when
        //! complete.

        use std::collections::BTreeMap;
        use std::fmt;

        /// Why a result was refused.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum AggregateError {
            /// The part index is not below the part count.
            OutOfRange,
            /// That part already has a result.
            Duplicate,
            /// The part count is zero.
            NoParts,
        }

        impl fmt::Display for AggregateError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self {
                    AggregateError::OutOfRange => "part index out of range",
                    AggregateError::Duplicate => "part already has a result",
                    AggregateError::NoParts => "a task needs at least one part",
                })
            }
        }

        impl std::error::Error for AggregateError {}

        /// Gathers one result per part.
        #[derive(Debug, Clone)]
        pub struct Aggregator {
            parts: usize,
            results: BTreeMap<usize, Vec<u8>>,
        }

        impl Aggregator {
            /// Expect results for parts `0..parts`.
            pub fn new(parts: usize) -> Result<Self, AggregateError> {
                if parts == 0 {
                    return Err(AggregateError::NoParts);
                }
                Ok(Aggregator {
                    parts,
                    results: BTreeMap::new(),
                })
            }

            /// Record the result of one part. A second result for the same part is refused, so a
            /// late duplicate from a retried node cannot overwrite the accepted one.
            pub fn submit(&mut self, part: usize, result: Vec<u8>) -> Result<(), AggregateError> {
                if part >= self.parts {
                    return Err(AggregateError::OutOfRange);
                }
                if self.results.contains_key(&part) {
                    return Err(AggregateError::Duplicate);
                }
                self.results.insert(part, result);
                Ok(())
            }

            /// Parts received so far.
            pub fn received(&self) -> usize {
                self.results.len()
            }

            /// Total parts expected.
            pub fn parts(&self) -> usize {
                self.parts
            }

            /// `true` once every part has a result.
            pub fn is_complete(&self) -> bool {
                self.results.len() == self.parts
            }

            /// Part indices still waiting for a result, ascending.
            pub fn missing(&self) -> Vec<usize> {
                (0..self.parts)
                    .filter(|p| !self.results.contains_key(p))
                    .collect()
            }

            /// Results in part order, or `None` while incomplete.
            pub fn finish(self) -> Option<Vec<Vec<u8>>> {
                self.is_complete()
                    .then(|| self.results.into_values().collect())
            }

            /// All results concatenated in part order, or `None` while incomplete.
            pub fn finish_concat(self) -> Option<Vec<u8>> {
                self.finish().map(|v| v.concat())
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn zero_parts_is_an_error() {
                assert_eq!(Aggregator::new(0).unwrap_err(), AggregateError::NoParts);
            }

            #[test]
            fn collects_out_of_order_and_returns_in_order() {
                let mut a = Aggregator::new(3).unwrap();
                a.submit(2, b"c".to_vec()).unwrap();
                assert_eq!(a.missing(), vec![0, 1]);
                assert!(!a.is_complete());
                a.submit(0, b"a".to_vec()).unwrap();
                a.submit(1, b"b".to_vec()).unwrap();
                assert!(a.is_complete());
                assert_eq!((a.received(), a.parts()), (3, 3));
                assert_eq!(
                    a.clone().finish().unwrap(),
                    vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
                );
                assert_eq!(a.finish_concat().unwrap(), b"abc");
            }

            #[test]
            fn incomplete_gives_nothing() {
                let mut a = Aggregator::new(2).unwrap();
                a.submit(1, vec![1]).unwrap();
                assert!(a.clone().finish().is_none());
                assert!(a.finish_concat().is_none());
            }

            #[test]
            fn refuses_duplicates_and_out_of_range() {
                let mut a = Aggregator::new(2).unwrap();
                a.submit(0, b"first".to_vec()).unwrap();
                assert_eq!(
                    a.submit(0, b"late".to_vec()),
                    Err(AggregateError::Duplicate)
                );
                assert_eq!(a.submit(2, vec![]), Err(AggregateError::OutOfRange));
                assert_eq!(
                    a.submit(usize::MAX, vec![]),
                    Err(AggregateError::OutOfRange)
                );
                a.submit(1, vec![]).unwrap();
                // the first accepted result is the one kept
                assert_eq!(a.finish_concat().unwrap(), b"first");
            }

            #[test]
            fn empty_results_are_valid() {
                let mut a = Aggregator::new(1).unwrap();
                a.submit(0, vec![]).unwrap();
                assert_eq!(a.finish().unwrap(), vec![Vec::<u8>::new()]);
            }

            #[test]
            fn errors_display() {
                assert!(AggregateError::Duplicate.to_string().contains("already"));
            }
        }
    }
    pub mod dispatch {
        //! Fault-tolerant task dispatch with checkpoints.

        use crate::mesh::nodes::{NodeId, NodeTable};
        use std::collections::BTreeMap;

        /// Task id.
        pub type TaskId = u64;

        /// Task state.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Status {
            /// Waiting for a node.
            Pending,
            /// Running on a node.
            Running(NodeId),
            /// Finished with a result.
            Done(Vec<u8>),
            /// Gave up after too many attempts.
            Failed,
        }

        /// A unit of offloaded work.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Task {
            /// Id.
            pub id: TaskId,
            /// Needs a GPU node.
            pub gpu: bool,
            /// Opaque job description.
            pub payload: Vec<u8>,
            /// Latest checkpoint reported by a worker (resume point).
            pub checkpoint: Option<Vec<u8>>,
            /// Progress in percent at the checkpoint.
            pub progress: u8,
            /// Assignments made so far.
            pub attempts: u32,
            /// Nodes that already failed this task.
            pub failed_on: Vec<NodeId>,
            /// State.
            pub status: Status,
        }

        /// An assignment to send to a node.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Assignment {
            /// Task.
            pub task: TaskId,
            /// Node.
            pub node: NodeId,
            /// Resume from this checkpoint, if any.
            pub resume: Option<Vec<u8>>,
        }

        /// Dispatcher.
        #[derive(Debug, Clone)]
        pub struct Dispatcher {
            tasks: BTreeMap<TaskId, Task>,
            next: TaskId,
            /// Attempts before a task is marked failed.
            pub max_attempts: u32,
        }

        impl Default for Dispatcher {
            fn default() -> Self {
                Self::new(3)
            }
        }

        impl Dispatcher {
            /// Dispatcher allowing `max_attempts` per task.
            #[must_use]
            pub fn new(max_attempts: u32) -> Self {
                Self {
                    tasks: BTreeMap::new(),
                    next: 1,
                    max_attempts: max_attempts.max(1),
                }
            }

            /// Submit work.
            pub fn submit(&mut self, gpu: bool, payload: Vec<u8>) -> TaskId {
                let id = self.next;
                self.next += 1;
                self.tasks.insert(
                    id,
                    Task {
                        id,
                        gpu,
                        payload,
                        checkpoint: None,
                        progress: 0,
                        attempts: 0,
                        failed_on: Vec::new(),
                        status: Status::Pending,
                    },
                );
                id
            }

            /// Requeue tasks on dead nodes, then assign pending tasks to live nodes.
            pub fn schedule(&mut self, nodes: &mut NodeTable, now: u64) -> Vec<Assignment> {
                for t in self.tasks.values_mut() {
                    if let Status::Running(n) = t.status {
                        if !nodes.alive(n, now) {
                            nodes.add_load(n, -1);
                            t.failed_on.push(n);
                            t.status = Status::Pending;
                        }
                    }
                }
                let mut out = Vec::new();
                for t in self.tasks.values_mut() {
                    if t.status != Status::Pending {
                        continue;
                    }
                    if t.attempts >= self.max_attempts {
                        t.status = Status::Failed;
                        continue;
                    }
                    let node = nodes
                        .pick(t.gpu, now, &t.failed_on)
                        .or_else(|| nodes.pick(t.gpu, now, &[]));
                    if let Some(n) = node {
                        nodes.add_load(n, 1);
                        t.attempts += 1;
                        t.status = Status::Running(n);
                        out.push(Assignment {
                            task: t.id,
                            node: n,
                            resume: t.checkpoint.clone(),
                        });
                    }
                }
                out
            }

            /// A worker reports progress. Ignored unless `node` owns the task.
            pub fn checkpoint(
                &mut self,
                id: TaskId,
                node: NodeId,
                progress: u8,
                state: Vec<u8>,
            ) -> bool {
                match self.tasks.get_mut(&id) {
                    Some(t) if t.status == Status::Running(node) => {
                        t.progress = progress.min(100);
                        t.checkpoint = Some(state);
                        true
                    }
                    _ => false,
                }
            }

            /// A worker finished. Ignored unless `node` owns the task.
            pub fn complete(
                &mut self,
                nodes: &mut NodeTable,
                id: TaskId,
                node: NodeId,
                result: Vec<u8>,
            ) -> bool {
                match self.tasks.get_mut(&id) {
                    Some(t) if t.status == Status::Running(node) => {
                        nodes.add_load(node, -1);
                        t.progress = 100;
                        t.status = Status::Done(result);
                        true
                    }
                    _ => false,
                }
            }

            /// A worker failed the task; it will be retried elsewhere.
            pub fn fail(&mut self, nodes: &mut NodeTable, id: TaskId, node: NodeId) -> bool {
                match self.tasks.get_mut(&id) {
                    Some(t) if t.status == Status::Running(node) => {
                        nodes.add_load(node, -1);
                        t.failed_on.push(node);
                        t.status = Status::Pending;
                        true
                    }
                    _ => false,
                }
            }

            /// Look up a task.
            #[must_use]
            pub fn get(&self, id: TaskId) -> Option<&Task> {
                self.tasks.get(&id)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn reassigns_from_dead_node_with_checkpoint() {
                let mut nodes = NodeTable::new(10);
                nodes.join(1, "laptop", true, 1, 0);
                nodes.join(2, "desktop", true, 1, 0);
                let mut d = Dispatcher::new(3);
                let t = d.submit(true, b"render".to_vec());
                let a = d.schedule(&mut nodes, 0);
                assert_eq!(a.len(), 1);
                let first = a[0].node;
                assert!(d.checkpoint(t, first, 40, b"frame-40".to_vec()));
                // First node stops sending heartbeats; the other keeps going.
                let other = if first == 1 { 2 } else { 1 };
                nodes.heartbeat(other, 20);
                let a = d.schedule(&mut nodes, 20);
                assert_eq!(a[0].node, other);
                assert_eq!(a[0].resume.as_deref(), Some(&b"frame-40"[..]));
                // A stale report from the dead node is ignored.
                assert!(!d.complete(&mut nodes, t, first, b"late".to_vec()));
                assert!(d.complete(&mut nodes, t, other, b"ok".to_vec()));
                assert_eq!(d.get(t).unwrap().status, Status::Done(b"ok".to_vec()));
                assert_eq!(nodes.get(other).unwrap().load, 0);
            }

            #[test]
            fn gives_up_after_max_attempts() {
                let mut nodes = NodeTable::new(100);
                nodes.join(1, "phone", false, 1, 0);
                let mut d = Dispatcher::new(2);
                let t = d.submit(false, vec![]);
                for _ in 0..2 {
                    let a = d.schedule(&mut nodes, 0);
                    assert!(d.fail(&mut nodes, t, a[0].node));
                }
                assert!(d.schedule(&mut nodes, 0).is_empty());
                assert_eq!(d.get(t).unwrap().status, Status::Failed);
                // GPU work waits when no GPU node exists.
                let g = d.submit(true, vec![]);
                assert!(d.schedule(&mut nodes, 0).is_empty());
                assert_eq!(d.get(g).unwrap().status, Status::Pending);
            }
        }
    }
    pub mod files {
        //! Content-addressed file sync.
        //!
        //! Files are split into fixed-size chunks named by their SHA-256; a
        //! manifest lists the chunk hashes and its own hash identifies the file
        //! version. Version vectors (one counter per device) tell whether one
        //! version descends from another or the two were edited concurrently.

        use crate::mesh::nodes::NodeId;
        use sha2::{Digest, Sha256};
        use std::cmp::Ordering;
        use std::collections::BTreeMap;

        /// 32-byte content hash.
        pub type Hash = [u8; 32];

        /// Chunk list of one file version.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Manifest {
            /// Chunk size used.
            pub chunk_size: usize,
            /// Total length in bytes.
            pub len: u64,
            /// Chunk hashes in order.
            pub chunks: Vec<Hash>,
        }

        impl Manifest {
            /// Split `data` into chunks; returns the manifest and the chunks by hash.
            #[must_use]
            pub fn build(data: &[u8], chunk_size: usize) -> (Self, BTreeMap<Hash, Vec<u8>>) {
                let chunk_size = chunk_size.max(1);
                let mut store = BTreeMap::new();
                let chunks = data
                    .chunks(chunk_size)
                    .map(|c| {
                        let h: Hash = Sha256::digest(c).into();
                        store.entry(h).or_insert_with(|| c.to_vec());
                        h
                    })
                    .collect();
                (
                    Self {
                        chunk_size,
                        len: data.len() as u64,
                        chunks,
                    },
                    store,
                )
            }

            /// Hash identifying this version.
            #[must_use]
            pub fn id(&self) -> Hash {
                let mut h = Sha256::new();
                h.update((self.chunk_size as u64).to_le_bytes());
                h.update(self.len.to_le_bytes());
                for c in &self.chunks {
                    h.update(c);
                }
                h.finalize().into()
            }

            /// Chunks of `self` not present in `have` (what must be transferred).
            #[must_use]
            pub fn missing<'a>(&'a self, have: &BTreeMap<Hash, Vec<u8>>) -> Vec<&'a Hash> {
                let mut seen = std::collections::BTreeSet::new();
                self.chunks
                    .iter()
                    .filter(|h| !have.contains_key(*h) && seen.insert(**h))
                    .collect()
            }

            /// Chunks of `self` that are present in `store` but do not match their hash.
            #[must_use]
            pub fn corrupt<'a>(&'a self, store: &BTreeMap<Hash, Vec<u8>>) -> Vec<&'a Hash> {
                let mut seen = std::collections::BTreeSet::new();
                self.chunks
                    .iter()
                    .filter(|h| {
                        seen.insert(**h)
                            && store
                                .get(*h)
                                .is_some_and(|c| Sha256::digest(c).as_slice() != h.as_slice())
                    })
                    .collect()
            }

            /// Fix `store` for this manifest: drop corrupt chunks and refill every missing or
            /// corrupt chunk from the first replica holding a copy that matches its hash.
            /// Replica chunks are verified before use, so a corrupt replica cannot spread damage.
            pub fn repair(
                &self,
                store: &mut BTreeMap<Hash, Vec<u8>>,
                replicas: &[&BTreeMap<Hash, Vec<u8>>],
            ) -> RepairReport {
                let bad: Vec<Hash> = self.corrupt(store).into_iter().copied().collect();
                for h in &bad {
                    store.remove(h);
                }
                let mut report = RepairReport::default();
                for h in self.missing(store).into_iter().copied().collect::<Vec<_>>() {
                    let good = replicas
                        .iter()
                        .filter_map(|r| r.get(&h))
                        .find(|c| Sha256::digest(c).as_slice() == h.as_slice());
                    match good {
                        Some(c) => {
                            store.insert(h, c.clone());
                            report.repaired += 1;
                        }
                        None => report.unrecoverable.push(h),
                    }
                }
                report.corrupt_dropped = bad.len();
                report
            }

            /// Reassemble and verify. `None` if a chunk is missing or corrupt.
            #[must_use]
            pub fn assemble(&self, store: &BTreeMap<Hash, Vec<u8>>) -> Option<Vec<u8>> {
                let mut out = Vec::with_capacity(usize::try_from(self.len).ok()?);
                for h in &self.chunks {
                    let c = store.get(h)?;
                    let actual: Hash = Sha256::digest(c).into();
                    if &actual != h {
                        return None;
                    }
                    out.extend_from_slice(c);
                }
                (out.len() as u64 == self.len).then_some(out)
            }
        }

        /// Outcome of [`Manifest::repair`].
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct RepairReport {
            /// Corrupt local chunks that were dropped.
            pub corrupt_dropped: usize,
            /// Chunks restored from a replica.
            pub repaired: usize,
            /// Chunks no replica could supply intact.
            pub unrecoverable: Vec<Hash>,
        }

        /// Per-device edit counters.
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct VersionVector(BTreeMap<NodeId, u64>);

        /// Relation between two versions.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Causality {
            /// Identical history.
            Equal,
            /// Left descends from right.
            After,
            /// Right descends from left.
            Before,
            /// Edited independently: a conflict to resolve.
            Concurrent,
        }

        impl VersionVector {
            /// Empty vector.
            #[must_use]
            pub fn new() -> Self {
                Self::default()
            }

            /// Record an edit on `node`.
            pub fn bump(&mut self, node: NodeId) {
                *self.0.entry(node).or_insert(0) += 1;
            }

            /// Component-wise maximum (after resolving a conflict).
            pub fn merge(&mut self, other: &Self) {
                for (&n, &c) in &other.0 {
                    let e = self.0.entry(n).or_insert(0);
                    *e = (*e).max(c);
                }
            }

            /// Compare histories.
            #[must_use]
            pub fn compare(&self, other: &Self) -> Causality {
                let mut less = false;
                let mut greater = false;
                for n in self.0.keys().chain(other.0.keys()) {
                    let a = self.0.get(n).copied().unwrap_or(0);
                    let b = other.0.get(n).copied().unwrap_or(0);
                    match a.cmp(&b) {
                        Ordering::Less => less = true,
                        Ordering::Greater => greater = true,
                        Ordering::Equal => {}
                    }
                }
                match (less, greater) {
                    (false, false) => Causality::Equal,
                    (false, true) => Causality::After,
                    (true, false) => Causality::Before,
                    (true, true) => Causality::Concurrent,
                }
            }
        }

        /// Choose up to `copies` replica holders from `live` nodes, preferring
        /// nodes that already hold the file, then by id. Deterministic per file.
        #[must_use]
        pub fn place_replicas(
            file: &Hash,
            live: &[NodeId],
            holders: &[NodeId],
            copies: usize,
        ) -> Vec<NodeId> {
            let mut v: Vec<NodeId> = live.to_vec();
            v.sort_by_key(|n| {
                // Rendezvous hashing: stable spread of files across nodes.
                let mut h = Sha256::new();
                h.update(file);
                h.update(n.to_le_bytes());
                let d: Hash = h.finalize().into();
                (!holders.contains(n), d)
            });
            v.dedup();
            v.truncate(copies);
            v
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn chunk_round_trip_and_dedup() {
                let data = b"abcdabcdabXY".to_vec();
                let (m, store) = Manifest::build(&data, 4);
                assert_eq!(m.chunks.len(), 3);
                assert_eq!(store.len(), 2, "repeated chunk stored once");
                assert_eq!(m.assemble(&store).unwrap(), data);
                let (m2, _) = Manifest::build(b"abcdabcdabXZ", 4);
                assert_ne!(m.id(), m2.id());
                assert_eq!(m2.missing(&store).len(), 1);
            }

            #[test]
            fn corrupt_chunk_rejected() {
                let (m, mut store) = Manifest::build(b"hello world", 4);
                let k = m.chunks[1];
                store.get_mut(&k).unwrap()[0] ^= 1;
                assert!(m.assemble(&store).is_none());
            }

            #[test]
            fn corrupt_lists_only_damaged_chunks() {
                let (m, mut store) = Manifest::build(b"aaaabbbbcccc", 4);
                assert!(m.corrupt(&store).is_empty());
                let k = m.chunks[1];
                store.get_mut(&k).unwrap()[3] ^= 0x80;
                assert_eq!(m.corrupt(&store), vec![&k]);
                store.remove(&m.chunks[2]); // missing is not corrupt
                assert_eq!(m.corrupt(&store).len(), 1);
            }

            #[test]
            fn repair_restores_from_a_good_replica() {
                let data = b"the quick brown fox jumps over".to_vec();
                let (m, good) = Manifest::build(&data, 5);
                let mut local = good.clone();
                let (first, second, third) = (m.chunks[0], m.chunks[2], m.chunks[4]);
                local.get_mut(&first).unwrap()[0] ^= 1; // corrupt
                local.remove(&second); // missing
                                       // replica A has a corrupt copy of `first`, replica B a good one
                let mut a = good.clone();
                a.get_mut(&first).unwrap()[1] ^= 1;
                a.remove(&third);
                let b = good.clone();
                assert!(m.assemble(&local).is_none());
                let r = m.repair(&mut local, &[&a, &b]);
                assert_eq!(
                    r,
                    RepairReport {
                        corrupt_dropped: 1,
                        repaired: 2,
                        unrecoverable: vec![]
                    }
                );
                assert_eq!(m.assemble(&local).unwrap(), data);
            }

            #[test]
            fn repair_reports_what_it_cannot_fix() {
                let (m, good) = Manifest::build(b"aaaabbbb", 4);
                let mut local = BTreeMap::new();
                let mut bad_replica = good.clone();
                bad_replica.get_mut(&m.chunks[0]).unwrap()[0] ^= 1;
                let r = m.repair(&mut local, &[&bad_replica]);
                assert_eq!(r.repaired, 1);
                assert_eq!(r.unrecoverable, vec![m.chunks[0]]);
                assert!(m.assemble(&local).is_none());
                assert_eq!(m.repair(&mut BTreeMap::new(), &[]).unrecoverable.len(), 2);
            }

            #[test]
            fn repair_of_a_healthy_store_changes_nothing() {
                let (m, good) = Manifest::build(b"healthy data here", 6);
                let mut local = good.clone();
                assert_eq!(m.repair(&mut local, &[]), RepairReport::default());
                assert_eq!(local, good);
            }

            #[test]
            fn repeated_chunks_are_repaired_once() {
                let (m, good) = Manifest::build(b"abcdabcdabcd", 4);
                let mut local = BTreeMap::new();
                let r = m.repair(&mut local, &[&good]);
                assert_eq!(r.repaired, 1);
                assert_eq!(m.assemble(&local).unwrap(), b"abcdabcdabcd");
            }

            #[test]
            fn version_vectors() {
                let mut a = VersionVector::new();
                a.bump(1);
                let mut b = a.clone();
                assert_eq!(a.compare(&b), Causality::Equal);
                b.bump(2);
                assert_eq!(b.compare(&a), Causality::After);
                assert_eq!(a.compare(&b), Causality::Before);
                a.bump(1);
                assert_eq!(a.compare(&b), Causality::Concurrent);
                a.merge(&b);
                a.bump(1);
                assert_eq!(a.compare(&b), Causality::After);
            }

            #[test]
            fn replica_placement() {
                let f = [7u8; 32];
                let p = place_replicas(&f, &[1, 2, 3, 4], &[3], 2);
                assert_eq!(p.len(), 2);
                assert_eq!(p[0], 3, "existing holder kept first");
                assert_eq!(place_replicas(&f, &[1, 2, 3, 4], &[3], 2), p);
                assert!(place_replicas(&f, &[], &[], 3).is_empty());
            }
        }
    }
    pub mod nodes {
        //! Mesh membership.

        use std::collections::BTreeMap;

        /// Node id.
        pub type NodeId = u32;

        /// A mesh member.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Node {
            /// Id.
            pub id: NodeId,
            /// Display name.
            pub name: String,
            /// Has a usable GPU.
            pub gpu: bool,
            /// Tasks currently assigned.
            pub load: u32,
            /// Max concurrent tasks.
            pub slots: u32,
            /// Last heartbeat time.
            pub last_seen: u64,
        }

        /// Nodes and liveness.
        #[derive(Debug, Clone)]
        pub struct NodeTable {
            nodes: BTreeMap<NodeId, Node>,
            /// Seconds without a heartbeat before a node counts as down.
            pub timeout: u64,
        }

        impl NodeTable {
            /// Table with heartbeat timeout `timeout` seconds.
            #[must_use]
            pub fn new(timeout: u64) -> Self {
                Self {
                    nodes: BTreeMap::new(),
                    timeout,
                }
            }

            /// Add or refresh a node.
            pub fn join(&mut self, id: NodeId, name: &str, gpu: bool, slots: u32, now: u64) {
                let n = self.nodes.entry(id).or_insert_with(|| Node {
                    id,
                    name: name.to_owned(),
                    gpu,
                    load: 0,
                    slots,
                    last_seen: now,
                });
                n.gpu = gpu;
                n.slots = slots;
                n.last_seen = now;
            }

            /// Record a heartbeat. Returns false for unknown nodes.
            pub fn heartbeat(&mut self, id: NodeId, now: u64) -> bool {
                match self.nodes.get_mut(&id) {
                    Some(n) => {
                        n.last_seen = n.last_seen.max(now);
                        true
                    }
                    None => false,
                }
            }

            /// Node is alive at `now`.
            #[must_use]
            pub fn alive(&self, id: NodeId, now: u64) -> bool {
                self.nodes
                    .get(&id)
                    .is_some_and(|n| now.saturating_sub(n.last_seen) <= self.timeout)
            }

            /// Ids of nodes that are down at `now`.
            #[must_use]
            pub fn down(&self, now: u64) -> Vec<NodeId> {
                self.nodes
                    .keys()
                    .copied()
                    .filter(|&id| !self.alive(id, now))
                    .collect()
            }

            /// Least-loaded live node with a free slot (and a GPU if required).
            #[must_use]
            pub fn pick(&self, need_gpu: bool, now: u64, exclude: &[NodeId]) -> Option<NodeId> {
                self.nodes
                    .values()
                    .filter(|n| {
                        self.alive(n.id, now)
                            && n.load < n.slots
                            && (!need_gpu || n.gpu)
                            && !exclude.contains(&n.id)
                    })
                    .min_by(|a, b| {
                        // Compare load / slots without floats: a.load * b.slots vs b.load * a.slots.
                        (u64::from(a.load) * u64::from(b.slots))
                            .cmp(&(u64::from(b.load) * u64::from(a.slots)))
                            .then(a.id.cmp(&b.id))
                    })
                    .map(|n| n.id)
            }

            /// Change a node's load by `delta`.
            pub fn add_load(&mut self, id: NodeId, delta: i32) {
                if let Some(n) = self.nodes.get_mut(&id) {
                    n.load = n.load.saturating_add_signed(delta);
                }
            }

            /// Look up a node.
            #[must_use]
            pub fn get(&self, id: NodeId) -> Option<&Node> {
                self.nodes.get(&id)
            }

            /// All live node ids at `now`.
            #[must_use]
            pub fn live(&self, now: u64) -> Vec<NodeId> {
                self.nodes
                    .keys()
                    .copied()
                    .filter(|&id| self.alive(id, now))
                    .collect()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn liveness_and_pick() {
                let mut t = NodeTable::new(10);
                t.join(1, "laptop", true, 2, 0);
                t.join(2, "phone", false, 4, 0);
                assert_eq!(t.pick(true, 5, &[]), Some(1));
                t.add_load(2, 1);
                assert_eq!(t.pick(false, 5, &[]), Some(1));
                t.add_load(1, 1);
                // laptop 1/2 = 0.5, phone 1/4 = 0.25
                assert_eq!(t.pick(false, 5, &[]), Some(2));
                assert_eq!(t.down(11), vec![1, 2]);
                assert!(t.heartbeat(1, 11));
                assert_eq!(t.down(11), vec![2]);
                assert!(!t.heartbeat(9, 11));
                assert_eq!(t.pick(false, 11, &[1]), None);
            }
        }
    }
}

#[cfg(feature = "auth")]
#[allow(clippy::module_inception)]
pub mod auth {
    //! MOULI-AUTH core: the enterprise authority for identity, tenancy, authorisation,
    //! approvals and audit, rebuilt in Rust from the MOULI-AUTH specification and the MOULI-AUTH/AgentBox
    //! boundary document.
    //!
    //! The MOULI-AUTH repository (`CMKIXVI/aeonone`) itself was not available (the source tree holds only specifications),
    //! so nothing here is derived from its code and **nothing is verified against the real MOULI-AUTH**.
    //! In particular the envelope scheme is HMAC-SHA256, not the JWKS-style signature MOULI-AUTH is
    //! documented to use; see [`envelope`].
    //!
    //! - [`org`]: tenant > company > plant hierarchy.
    //! - [`principal`]: people, services and agents.
    //! - [`policy`]: deny-by-default RBAC with attribute, scope and amount conditions.
    //! - [`makerchecker`]: tiered approvals with segregation of duties and escalation.
    //! - [`envelope`]: signed, short-lived, audience-bound authorisation envelopes with replay and
    //!   revocation checks.
    //! - [`authority`]: [`Authority`], which decides, audits and issues envelopes.
    //!
    //! Not here: login, MFA, SSO, sessions, persistence (`mouli-data`), policy lifecycle and
    //! administrative APIs, and any network service.

    pub mod authority {
        //! The authority: one object that decides, audits and issues envelopes. Consumers enforce what
        //! it returns; they never decide for themselves.

        use crate::auth::envelope::{sign, Claims, MAX_TTL_SECS, MIN_KEY_LEN};
        use crate::auth::org::Org;
        use crate::auth::policy::{evaluate, Decision, Request, Rule};
        use crate::auth::principal::Directory;
        use mouli_interface::audit::{AuditLog, AuditRecord};
        use mouli_interface::ids::IdGen;
        use mouli_interface::{Error, Result};
        use std::collections::{BTreeMap, BTreeSet};

        /// An authorisation result with its id.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Authorization {
            /// Decision id (quote it in audit and envelopes).
            pub id: String,
            /// The decision.
            pub decision: Decision,
        }

        struct Granted {
            subject: String,
            tenant: String,
            node: String,
            action: String,
        }

        /// The enterprise authority.
        pub struct Authority {
            /// Tenant and org hierarchy.
            pub org: Org,
            /// Principals.
            pub directory: Directory,
            rules: Vec<Rule>,
            ids: IdGen,
            audit: AuditLog,
            granted: BTreeMap<String, Granted>,
            revoked: BTreeSet<String>,
            key: Vec<u8>,
        }

        impl Authority {
            /// Create an authority that signs envelopes with `key`.
            ///
            /// # Errors
            /// [`Error::Invalid`] when the key is shorter than 32 bytes.
            pub fn new(key: &[u8]) -> Result<Self> {
                if key.len() < MIN_KEY_LEN {
                    return Err(Error::Invalid(format!(
                        "signing key must be at least {MIN_KEY_LEN} bytes"
                    )));
                }
                Ok(Self {
                    org: Org::new(),
                    directory: Directory::new(),
                    rules: Vec::new(),
                    ids: IdGen::new(),
                    audit: AuditLog::new(),
                    granted: BTreeMap::new(),
                    revoked: BTreeSet::new(),
                    key: key.to_vec(),
                })
            }

            /// Add a policy rule.
            ///
            /// # Errors
            /// [`Error::Invalid`] when the rule's tenant or scope node does not exist, or the scope
            /// belongs to another tenant.
            pub fn add_rule(&mut self, rule: Rule, now: u64) -> Result<()> {
                if self.org.tenant_of(&rule.tenant) != Some(rule.tenant.as_str()) {
                    return Err(Error::Invalid(format!("unknown tenant {}", rule.tenant)));
                }
                if let Some(s) = &rule.scope {
                    if self.org.tenant_of(s) != Some(rule.tenant.as_str()) {
                        return Err(Error::Invalid(format!(
                            "scope {s} is not in tenant {}",
                            rule.tenant
                        )));
                    }
                }
                self.audit
                    .append(now, "authority", "policy.add", &rule.id, "ok");
                self.rules.push(rule);
                Ok(())
            }

            /// Decide a request and write the audit record.
            pub fn authorize(&mut self, req: &Request, now: u64) -> Authorization {
                let decision = evaluate(&self.rules, &self.org, &self.directory, req);
                let id = self.ids.next("dec");
                let outcome = format!(
                    "{}: {}",
                    if decision.allowed { "allow" } else { "deny" },
                    decision.reason
                );
                self.audit.append(
                    now,
                    &req.principal,
                    &req.action,
                    &req.resource.node,
                    &format!("{id} {outcome}"),
                );
                if decision.allowed {
                    self.granted.insert(
                        id.clone(),
                        Granted {
                            subject: req.principal.clone(),
                            tenant: req.resource.tenant.clone(),
                            node: req.resource.node.clone(),
                            action: req.action.clone(),
                        },
                    );
                }
                Authorization { id, decision }
            }

            /// Issue a signed envelope for an allowed decision, to be accepted only by `audience`.
            ///
            /// # Errors
            /// [`Error::NotFound`] for an unknown or denied decision, [`Error::Denied`] when it was
            /// revoked or the principal has since been deactivated, [`Error::Invalid`] for a bad
            /// lifetime or fields.
            pub fn issue_envelope(
                &mut self,
                decision_id: &str,
                audience: &str,
                correlation_id: &str,
                ttl_secs: u64,
                now: u64,
            ) -> Result<String> {
                let g = self
                    .granted
                    .get(decision_id)
                    .ok_or_else(|| Error::NotFound(format!("allowed decision {decision_id}")))?;
                if self.revoked.contains(decision_id) {
                    return Err(Error::Denied("the decision was revoked".into()));
                }
                if !self
                    .directory
                    .get(&g.tenant, &g.subject)
                    .is_some_and(|p| p.active)
                {
                    return Err(Error::Denied("the principal is no longer active".into()));
                }
                if ttl_secs == 0 || ttl_secs > MAX_TTL_SECS {
                    return Err(Error::Invalid(format!(
                        "ttl must be 1..={MAX_TTL_SECS} seconds"
                    )));
                }
                let claims = Claims {
                    subject: g.subject.clone(),
                    tenant: g.tenant.clone(),
                    node: g.node.clone(),
                    scopes: vec![g.action.clone()],
                    audience: audience.into(),
                    decision_id: decision_id.into(),
                    correlation_id: correlation_id.into(),
                    token_id: self.ids.next("jti"),
                    issued_at: now,
                    expires_at: now + ttl_secs,
                };
                let token = sign(&self.key, &claims)?;
                let subject = claims.subject.clone();
                self.audit.append(
                    now,
                    &subject,
                    "envelope.issue",
                    decision_id,
                    &format!("audience {audience}, ttl {ttl_secs}s"),
                );
                Ok(token)
            }

            /// Revoke a decision. Pass the id to every verifier (`Verifier::revoke`).
            pub fn revoke(&mut self, decision_id: &str, now: u64) {
                self.revoked.insert(decision_id.into());
                self.audit
                    .append(now, "authority", "decision.revoke", decision_id, "ok");
            }

            /// Audit records, hash-chained.
            #[must_use]
            pub fn audit(&self) -> &[AuditRecord] {
                self.audit.records()
            }

            /// Verify the audit chain.
            ///
            /// # Errors
            /// Index of the first bad record.
            pub fn verify_audit(&self) -> std::result::Result<(), usize> {
                self.audit.verify()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::auth::envelope::Verifier;
            use crate::auth::org::Level;
            use crate::auth::policy::{Cond, Effect, Resource};
            use crate::auth::principal::Kind;

            const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

            fn authority() -> Authority {
                let mut a = Authority::new(KEY).unwrap();
                a.org.create_tenant("acme").unwrap();
                a.org.add("co", Level::Company, "acme").unwrap();
                a.org.add("pune", Level::Plant, "co").unwrap();
                a.directory
                    .create("ann", "acme", Kind::Human, &["clerk"], &[])
                    .unwrap();
                a.add_rule(
                    Rule {
                        id: "agents".into(),
                        tenant: "acme".into(),
                        effect: Effect::Allow,
                        role: "clerk".into(),
                        action: "agent.run".into(),
                        resource_kind: "agent".into(),
                        scope: Some("co".into()),
                        conditions: vec![Cond::AmountAtMost(0)],
                    },
                    1,
                )
                .unwrap();
                a
            }

            fn req(action: &str) -> Request {
                Request {
                    principal: "ann".into(),
                    action: action.into(),
                    resource: Resource {
                        tenant: "acme".into(),
                        node: "pune".into(),
                        kind: "agent".into(),
                        attrs: BTreeMap::new(),
                        amount: Some(0),
                    },
                }
            }

            #[test]
            fn decide_issue_verify_end_to_end() {
                let mut a = authority();
                let auth = a.authorize(&req("agent.run"), 10);
                assert!(auth.decision.allowed, "{}", auth.decision.reason);
                let token = a
                    .issue_envelope(&auth.id, "agentbox", "req-1", 300, 10)
                    .unwrap();
                let mut v = Verifier::new(KEY, "agentbox").unwrap();
                let c = v.verify(&token, 20, "acme", "agent.run").unwrap();
                assert_eq!(
                    (c.subject.as_str(), c.node.as_str(), c.decision_id.as_str()),
                    ("ann", "pune", auth.id.as_str())
                );
                assert!(v.verify(&token, 21, "acme", "agent.run").is_err(), "replay");
                assert!(a.verify_audit().is_ok());
                assert!(a.audit().iter().any(|r| r.action == "envelope.issue"));
            }

            #[test]
            fn denied_decisions_cannot_get_envelopes() {
                let mut a = authority();
                let auth = a.authorize(&req("agent.delete"), 10);
                assert!(!auth.decision.allowed);
                assert!(matches!(
                    a.issue_envelope(&auth.id, "agentbox", "r", 60, 10),
                    Err(Error::NotFound(_))
                ));
                assert!(matches!(
                    a.issue_envelope("dec-forged", "agentbox", "r", 60, 10),
                    Err(Error::NotFound(_))
                ));
                assert!(a.audit().last().unwrap().outcome.contains("deny"));
            }

            #[test]
            fn revocation_and_deactivation_stop_issuing() {
                let mut a = authority();
                let auth = a.authorize(&req("agent.run"), 10);
                a.revoke(&auth.id, 11);
                assert!(matches!(
                    a.issue_envelope(&auth.id, "agentbox", "r", 60, 12),
                    Err(Error::Denied(_))
                ));
                let second = a.authorize(&req("agent.run"), 13);
                a.directory.deactivate("acme", "ann").unwrap();
                assert!(matches!(
                    a.issue_envelope(&second.id, "agentbox", "r", 60, 14),
                    Err(Error::Denied(_))
                ));
                assert!(!a.authorize(&req("agent.run"), 15).decision.allowed);
            }

            #[test]
            fn ttl_and_key_and_rule_validation() {
                let mut a = authority();
                let auth = a.authorize(&req("agent.run"), 10);
                assert!(a.issue_envelope(&auth.id, "agentbox", "r", 0, 10).is_err());
                assert!(a
                    .issue_envelope(&auth.id, "agentbox", "r", MAX_TTL_SECS + 1, 10)
                    .is_err());
                assert!(Authority::new(b"short").is_err());
                let mut bad = Rule {
                    id: "x".into(),
                    tenant: "nope".into(),
                    effect: Effect::Allow,
                    role: "*".into(),
                    action: "*".into(),
                    resource_kind: "*".into(),
                    scope: None,
                    conditions: vec![],
                };
                assert!(a.add_rule(bad.clone(), 1).is_err());
                bad.tenant = "acme".into();
                a.org.create_tenant("beta").unwrap();
                bad.scope = Some("beta".into());
                assert!(a.add_rule(bad, 1).is_err(), "scope in another tenant");
            }
        }
    }
    pub mod envelope {
        //! Short-lived, audience-bound authorisation envelopes: the handoff from the authority to a
        //! consumer such as AgentBox (see `docs/AEONONE_AGENTBOX_BOUNDARY` in the C tree).
        //!
        //! An envelope binds subject, tenant, org node, scopes, audience, decision id, correlation id,
        //! issue and expiry times and a unique token id, and is signed with HMAC-SHA256.
        //!
        //! Deviation from the boundary document, stated plainly: MOULI-AUTH's real contract is JWKS-style
        //! (asymmetric) and was not available to read. HMAC means the verifier holds the signing key, so
        //! this suits one trust domain (the local machine, one deployment) and must not be exposed to
        //! parties that should only verify. Swapping in an asymmetric scheme needs a reviewed crypto
        //! dependency, which the port rules forbid adding silently.

        use mouli_interface::{Error, Result};
        use sha2::{Digest, Sha256};
        use std::collections::{BTreeMap, BTreeSet};

        /// Longest lifetime an envelope may have, in seconds.
        pub const MAX_TTL_SECS: u64 = 900;
        /// Shortest accepted signing key.
        pub const MIN_KEY_LEN: usize = 32;

        /// What an envelope asserts.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Claims {
            /// Principal the decision was made for.
            pub subject: String,
            /// Tenant.
            pub tenant: String,
            /// Company, plant or other org node the decision covers.
            pub node: String,
            /// Permitted actions.
            pub scopes: Vec<String>,
            /// The only service that may accept it.
            pub audience: String,
            /// Authorisation decision id, for audit correlation and revocation.
            pub decision_id: String,
            /// Request correlation id.
            pub correlation_id: String,
            /// Unique token id, spent once.
            pub token_id: String,
            /// Issue time (seconds).
            pub issued_at: u64,
            /// Expiry time (seconds).
            pub expires_at: u64,
        }

        /// HMAC-SHA256 (RFC 2104).
        #[must_use]
        pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
            let mut k = [0u8; 64];
            if key.len() > 64 {
                k[..32].copy_from_slice(&Sha256::digest(key));
            } else {
                k[..key.len()].copy_from_slice(key);
            }
            let mut inner = Sha256::new();
            inner.update(k.map(|b| b ^ 0x36));
            inner.update(msg);
            let mut outer = Sha256::new();
            outer.update(k.map(|b| b ^ 0x5c));
            outer.update(inner.finalize());
            outer.finalize().into()
        }

        fn ct_eq(a: &[u8], b: &[u8]) -> bool {
            a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
        }

        fn hex(b: &[u8]) -> String {
            use std::fmt::Write;
            b.iter().fold(String::new(), |mut s, x| {
                let _ = write!(s, "{x:02x}");
                s
            })
        }

        fn unhex(s: &str) -> Option<Vec<u8>> {
            if !s.len().is_multiple_of(2) || !s.is_ascii() {
                return None;
            }
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
                .collect()
        }

        fn field(name: &str, v: &str) -> Result<()> {
            if v.is_empty() || v.contains('\n') {
                return Err(Error::Invalid(format!(
                    "claim {name} must be non-empty and single-line"
                )));
            }
            Ok(())
        }

        fn canonical(c: &Claims) -> Result<String> {
            for (n, v) in [
                ("sub", &c.subject),
                ("ten", &c.tenant),
                ("node", &c.node),
                ("aud", &c.audience),
                ("dec", &c.decision_id),
                ("cor", &c.correlation_id),
                ("jti", &c.token_id),
            ] {
                field(n, v)?;
            }
            if c.scopes.is_empty()
                || c.scopes
                    .iter()
                    .any(|s| s.is_empty() || s.contains([',', '\n']))
            {
                return Err(Error::Invalid(
                    "scopes must be non-empty names without commas".into(),
                ));
            }
            Ok(format!(
                "sub={}\nten={}\nnode={}\nscp={}\naud={}\ndec={}\ncor={}\njti={}\niat={}\nexp={}",
                c.subject,
                c.tenant,
                c.node,
                c.scopes.join(","),
                c.audience,
                c.decision_id,
                c.correlation_id,
                c.token_id,
                c.issued_at,
                c.expires_at
            ))
        }

        fn parse(text: &str) -> Option<Claims> {
            let m: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
            let g = |k: &str| m.get(k).map(|v| (*v).to_string());
            Some(Claims {
                subject: g("sub")?,
                tenant: g("ten")?,
                node: g("node")?,
                scopes: g("scp")?.split(',').map(str::to_string).collect(),
                audience: g("aud")?,
                decision_id: g("dec")?,
                correlation_id: g("cor")?,
                token_id: g("jti")?,
                issued_at: g("iat")?.parse().ok()?,
                expires_at: g("exp")?.parse().ok()?,
            })
        }

        /// Sign claims into a token `v1.<claims hex>.<mac hex>`.
        ///
        /// # Errors
        /// [`Error::Invalid`] for a short key, bad fields, or a lifetime that is zero or above
        /// [`MAX_TTL_SECS`].
        pub fn sign(key: &[u8], c: &Claims) -> Result<String> {
            if key.len() < MIN_KEY_LEN {
                return Err(Error::Invalid(format!(
                    "signing key must be at least {MIN_KEY_LEN} bytes"
                )));
            }
            if c.expires_at <= c.issued_at || c.expires_at - c.issued_at > MAX_TTL_SECS {
                return Err(Error::Invalid(format!(
                    "lifetime must be 1..={MAX_TTL_SECS} seconds"
                )));
            }
            let text = canonical(c)?;
            Ok(format!(
                "v1.{}.{}",
                hex(text.as_bytes()),
                hex(&hmac_sha256(key, text.as_bytes()))
            ))
        }

        /// Consumer-side verifier: one audience, replay protection and revocation.
        pub struct Verifier {
            key: Vec<u8>,
            audience: String,
            used: BTreeMap<String, u64>,
            revoked: BTreeSet<String>,
        }

        impl Verifier {
            /// Create a verifier that accepts only tokens for `audience`.
            ///
            /// # Errors
            /// [`Error::Invalid`] for a short key or blank audience.
            pub fn new(key: &[u8], audience: &str) -> Result<Self> {
                if key.len() < MIN_KEY_LEN || audience.is_empty() {
                    return Err(Error::Invalid(
                        "a 32-byte key and an audience are required".into(),
                    ));
                }
                Ok(Self {
                    key: key.to_vec(),
                    audience: audience.into(),
                    used: BTreeMap::new(),
                    revoked: BTreeSet::new(),
                })
            }

            /// Refuse every envelope issued for this decision from now on.
            pub fn revoke(&mut self, decision_id: &str) {
                self.revoked.insert(decision_id.into());
            }

            /// Verify a token for `tenant` and `scope` at time `now`, and spend its id.
            ///
            /// # Errors
            /// [`Error::Denied`] for a bad signature, wrong audience, tenant or scope, expiry, or
            /// revocation; [`Error::Invalid`] for malformed tokens; [`Error::Conflict`] for a replay.
            pub fn verify(
                &mut self,
                token: &str,
                now: u64,
                tenant: &str,
                scope: &str,
            ) -> Result<Claims> {
                let mut parts = token.split('.');
                let (Some("v1"), Some(body), Some(mac), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return Err(Error::Invalid("malformed envelope".into()));
                };
                let (Some(body), Some(mac)) = (unhex(body), unhex(mac)) else {
                    return Err(Error::Invalid("malformed envelope encoding".into()));
                };
                if !ct_eq(&hmac_sha256(&self.key, &body), &mac) {
                    return Err(Error::Denied("bad envelope signature".into()));
                }
                let claims = String::from_utf8(body)
                    .ok()
                    .and_then(|t| parse(&t))
                    .ok_or_else(|| Error::Invalid("malformed envelope claims".into()))?;
                if claims.audience != self.audience {
                    return Err(Error::Denied("envelope is for another audience".into()));
                }
                if now < claims.issued_at || now >= claims.expires_at {
                    return Err(Error::Denied("envelope is expired or not yet valid".into()));
                }
                if claims.expires_at - claims.issued_at > MAX_TTL_SECS {
                    return Err(Error::Denied(
                        "envelope lifetime exceeds the maximum".into(),
                    ));
                }
                if claims.tenant != tenant {
                    return Err(Error::Denied("envelope is for another tenant".into()));
                }
                if !claims.scopes.iter().any(|s| s == scope) {
                    return Err(Error::Denied(format!("envelope does not grant {scope}")));
                }
                if self.revoked.contains(&claims.decision_id) {
                    return Err(Error::Denied(
                        "the authorisation decision was revoked".into(),
                    ));
                }
                self.used.retain(|_, exp| *exp > now);
                if self.used.contains_key(&claims.token_id) {
                    return Err(Error::Conflict("envelope was already used".into()));
                }
                self.used.insert(claims.token_id.clone(), claims.expires_at);
                Ok(claims)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

            fn claims() -> Claims {
                Claims {
                    subject: "ann".into(),
                    tenant: "acme".into(),
                    node: "pune".into(),
                    scopes: vec!["agent.run".into()],
                    audience: "agentbox".into(),
                    decision_id: "dec-1".into(),
                    correlation_id: "req-9".into(),
                    token_id: "jti-1".into(),
                    issued_at: 1000,
                    expires_at: 1300,
                }
            }

            #[test]
            fn hmac_matches_rfc_4231() {
                assert_eq!(
                    hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
                    "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
                );
                assert_eq!(
                    hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
                    "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
                );
                let long = [0xaa; 131];
                assert_eq!(
                    hex(&hmac_sha256(
                        &long,
                        b"Test Using Larger Than Block-Size Key - Hash Key First"
                    )),
                    "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
                );
            }

            #[test]
            fn round_trip_then_replay_is_refused() {
                let t = sign(KEY, &claims()).unwrap();
                let mut v = Verifier::new(KEY, "agentbox").unwrap();
                assert_eq!(v.verify(&t, 1100, "acme", "agent.run").unwrap(), claims());
                assert!(matches!(
                    v.verify(&t, 1101, "acme", "agent.run"),
                    Err(Error::Conflict(_))
                ));
            }

            #[test]
            fn every_binding_is_enforced() {
                let t = sign(KEY, &claims()).unwrap();
                let fresh = || Verifier::new(KEY, "agentbox").unwrap();
                assert!(
                    matches!(
                        fresh().verify(&t, 1100, "beta", "agent.run"),
                        Err(Error::Denied(_))
                    ),
                    "tenant"
                );
                assert!(
                    matches!(
                        fresh().verify(&t, 1100, "acme", "agent.delete"),
                        Err(Error::Denied(_))
                    ),
                    "scope"
                );
                assert!(
                    matches!(
                        Verifier::new(KEY, "other")
                            .unwrap()
                            .verify(&t, 1100, "acme", "agent.run"),
                        Err(Error::Denied(_))
                    ),
                    "audience"
                );
                assert!(
                    matches!(
                        fresh().verify(&t, 1300, "acme", "agent.run"),
                        Err(Error::Denied(_))
                    ),
                    "expired"
                );
                assert!(
                    matches!(
                        fresh().verify(&t, 999, "acme", "agent.run"),
                        Err(Error::Denied(_))
                    ),
                    "not yet valid"
                );
                let mut rev = fresh();
                rev.revoke("dec-1");
                assert!(
                    matches!(
                        rev.verify(&t, 1100, "acme", "agent.run"),
                        Err(Error::Denied(_))
                    ),
                    "revoked"
                );
                let other =
                    Verifier::new(b"a-different-key-of-32-bytes-long!", "agentbox").unwrap();
                assert!(
                    matches!(
                        {
                            let mut o = other;
                            o.verify(&t, 1100, "acme", "agent.run")
                        },
                        Err(Error::Denied(_))
                    ),
                    "wrong key"
                );
            }

            #[test]
            fn tampering_and_malformed_tokens() {
                let t = sign(KEY, &claims()).unwrap();
                let mut v = Verifier::new(KEY, "agentbox").unwrap();
                let parts: Vec<&str> = t.split('.').collect();
                let forged = claims_with(|c| c.tenant = "beta".into());
                let fparts: Vec<String> = forged.split('.').map(str::to_string).collect();
                let spliced = format!("v1.{}.{}", fparts[1], parts[2]);
                assert!(matches!(
                    v.verify(&spliced, 1100, "beta", "agent.run"),
                    Err(Error::Denied(_))
                ));
                for bad in [
                    "",
                    "v1.zz.zz",
                    "v2.aa.bb",
                    "v1.aa",
                    "v1.aa.bb.cc",
                    "v1.abc.def",
                ] {
                    assert!(
                        matches!(
                            v.verify(bad, 1100, "acme", "agent.run"),
                            Err(Error::Invalid(_))
                        ),
                        "{bad}"
                    );
                }
            }

            fn claims_with(f: impl FnOnce(&mut Claims)) -> String {
                let mut c = claims();
                f(&mut c);
                sign(KEY, &c).unwrap()
            }

            #[test]
            fn signing_rules() {
                assert!(sign(b"short", &claims()).is_err());
                let mut c = claims();
                c.expires_at = c.issued_at + MAX_TTL_SECS + 1;
                assert!(sign(KEY, &c).is_err());
                c.expires_at = c.issued_at;
                assert!(sign(KEY, &c).is_err());
                let mut c = claims();
                c.subject = "a\nb".into();
                assert!(sign(KEY, &c).is_err());
                let mut c = claims();
                c.scopes = vec![];
                assert!(sign(KEY, &c).is_err());
                assert!(Verifier::new(b"short", "x").is_err() && Verifier::new(KEY, "").is_err());
            }

            #[test]
            fn spent_ids_are_pruned_after_expiry() {
                let mut v = Verifier::new(KEY, "agentbox").unwrap();
                v.verify(&sign(KEY, &claims()).unwrap(), 1100, "acme", "agent.run")
                    .unwrap();
                let mut later = claims();
                later.token_id = "jti-2".into();
                later.issued_at = 5000;
                later.expires_at = 5100;
                v.verify(&sign(KEY, &later).unwrap(), 5001, "acme", "agent.run")
                    .unwrap();
                assert_eq!(v.used.len(), 1);
            }
        }
    }
    pub mod makerchecker {
        //! Maker-checker approvals with amount tiers, segregation of duties and escalation.

        use crate::auth::principal::Principal;
        use mouli_interface::ids::IdGen;
        use mouli_interface::{Error, Result};
        use std::collections::BTreeMap;

        /// One approval tier. Amounts are integer minor units.
        #[derive(Debug, Clone)]
        pub struct Tier {
            /// Highest amount this tier covers; `None` means no upper bound (the last tier).
            pub up_to: Option<u64>,
            /// Role whose holders may approve at this tier (higher tiers' roles may too).
            pub role: String,
            /// Distinct approvals needed.
            pub approvals: u8,
        }

        /// State of a case.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CaseState {
            /// Waiting for approvals.
            Open,
            /// Enough approvals.
            Approved,
            /// Rejected by an eligible checker.
            Rejected,
        }

        /// A change awaiting checking.
        #[derive(Debug, Clone)]
        pub struct Case {
            /// Id.
            pub id: String,
            /// Tenant.
            pub tenant: String,
            /// Who made the change.
            pub maker: String,
            /// Action to be performed once approved.
            pub action: String,
            /// Amount in minor units.
            pub amount: u64,
            /// Current tier index.
            pub tier: usize,
            /// Time of submission or last escalation.
            pub since: u64,
            /// Approvers so far.
            pub approved_by: Vec<String>,
            /// State.
            pub state: CaseState,
        }

        /// The workflow engine.
        pub struct MakerChecker {
            tiers: Vec<Tier>,
            escalate_after: u64,
            cases: BTreeMap<String, Case>,
        }

        impl MakerChecker {
            /// Create an engine. Tiers must ascend by `up_to`, the last must be unbounded, and every
            /// tier needs at least one approval.
            ///
            /// # Errors
            /// [`Error::Invalid`] when those rules are broken.
            pub fn new(tiers: Vec<Tier>, escalate_after: u64) -> Result<Self> {
                let ascending = tiers.windows(2).all(|w| {
                    matches!((w[0].up_to, w[1].up_to), (Some(a), Some(b)) if a < b)
                        || matches!((w[0].up_to, w[1].up_to), (Some(_), None))
                });
                if tiers.is_empty()
                    || !ascending
                    || tiers.last().is_some_and(|t| t.up_to.is_some())
                    || tiers.iter().any(|t| t.approvals == 0)
                {
                    return Err(Error::Invalid(
                        "tiers must ascend, end unbounded and need approvals".into(),
                    ));
                }
                Ok(Self {
                    tiers,
                    escalate_after,
                    cases: BTreeMap::new(),
                })
            }

            /// Submit a change for checking.
            pub fn submit(
                &mut self,
                tenant: &str,
                maker: &str,
                action: &str,
                amount: u64,
                now: u64,
                ids: &mut IdGen,
            ) -> String {
                let tier = self
                    .tiers
                    .iter()
                    .position(|t| t.up_to.is_none_or(|m| amount <= m))
                    .unwrap_or(self.tiers.len() - 1);
                let id = ids.next("case");
                self.cases.insert(
                    id.clone(),
                    Case {
                        id: id.clone(),
                        tenant: tenant.into(),
                        maker: maker.into(),
                        action: action.into(),
                        amount,
                        tier,
                        since: now,
                        approved_by: vec![],
                        state: CaseState::Open,
                    },
                );
                id
            }

            /// A case, if it belongs to `tenant`.
            #[must_use]
            pub fn get(&self, tenant: &str, id: &str) -> Option<&Case> {
                self.cases.get(id).filter(|c| c.tenant == tenant)
            }

            fn escalate(&mut self, id: &str, now: u64) {
                let last = self.tiers.len() - 1;
                if let Some(c) = self.cases.get_mut(id) {
                    if c.state == CaseState::Open
                        && c.tier < last
                        && now.saturating_sub(c.since) > self.escalate_after
                    {
                        c.tier += 1;
                        c.since = now;
                        c.approved_by.clear();
                    }
                }
            }

            fn check_eligible(&self, c: &Case, who: &Principal) -> Result<()> {
                if who.tenant != c.tenant || !who.active {
                    return Err(Error::Denied(
                        "approver is not an active member of this tenant".into(),
                    ));
                }
                if who.id == c.maker {
                    return Err(Error::Denied(
                        "the maker cannot check their own change".into(),
                    ));
                }
                if !self.tiers[c.tier..]
                    .iter()
                    .any(|t| who.roles.contains(&t.role))
                {
                    return Err(Error::Denied(format!(
                        "approver lacks the {} role or above",
                        self.tiers[c.tier].role
                    )));
                }
                Ok(())
            }

            /// Record an approval. Returns the state afterwards.
            ///
            /// # Errors
            /// [`Error::NotFound`] (also for another tenant's case), [`Error::State`] when not open,
            /// [`Error::Denied`] for ineligible approvers, [`Error::Conflict`] for a second approval by
            /// the same person.
            pub fn approve(
                &mut self,
                tenant: &str,
                id: &str,
                who: &Principal,
                now: u64,
            ) -> Result<CaseState> {
                self.get(tenant, id)
                    .ok_or_else(|| Error::NotFound(format!("case {id}")))?;
                self.escalate(id, now);
                let c = &self.cases[id];
                if c.state != CaseState::Open {
                    return Err(Error::State(format!("case {id} is not open")));
                }
                self.check_eligible(c, who)?;
                if c.approved_by.contains(&who.id) {
                    return Err(Error::Conflict("this person already approved".into()));
                }
                let need = usize::from(self.tiers[c.tier].approvals);
                let c = self
                    .cases
                    .get_mut(id)
                    .ok_or_else(|| Error::NotFound(format!("case {id}")))?;
                c.approved_by.push(who.id.clone());
                if c.approved_by.len() >= need {
                    c.state = CaseState::Approved;
                }
                Ok(c.state)
            }

            /// Reject a case.
            ///
            /// # Errors
            /// As [`MakerChecker::approve`], except duplicates.
            pub fn reject(
                &mut self,
                tenant: &str,
                id: &str,
                who: &Principal,
                now: u64,
            ) -> Result<()> {
                self.get(tenant, id)
                    .ok_or_else(|| Error::NotFound(format!("case {id}")))?;
                self.escalate(id, now);
                let c = &self.cases[id];
                if c.state != CaseState::Open {
                    return Err(Error::State(format!("case {id} is not open")));
                }
                self.check_eligible(c, who)?;
                if let Some(c) = self.cases.get_mut(id) {
                    c.state = CaseState::Rejected;
                }
                Ok(())
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::auth::principal::{Directory, Kind};

            fn tiers() -> Vec<Tier> {
                vec![
                    Tier {
                        up_to: Some(1_000),
                        role: "supervisor".into(),
                        approvals: 1,
                    },
                    Tier {
                        up_to: Some(100_000),
                        role: "manager".into(),
                        approvals: 1,
                    },
                    Tier {
                        up_to: None,
                        role: "director".into(),
                        approvals: 2,
                    },
                ]
            }

            fn people() -> Directory {
                let mut d = Directory::new();
                for (id, role) in [
                    ("maker", "clerk"),
                    ("sup", "supervisor"),
                    ("mgr", "manager"),
                    ("dir1", "director"),
                    ("dir2", "director"),
                ] {
                    d.create(id, "acme", Kind::Human, &[role], &[]).unwrap();
                }
                d.create("other", "beta", Kind::Human, &["director"], &[])
                    .unwrap();
                d
            }

            #[test]
            fn tier_selection_and_role_hierarchy() {
                let (d, mut ids) = (people(), IdGen::new());
                let mut m = MakerChecker::new(tiers(), 3600).unwrap();
                let small = m.submit("acme", "maker", "erp.pay", 500, 0, &mut ids);
                let mid = m.submit("acme", "maker", "erp.pay", 50_000, 0, &mut ids);
                assert_eq!(m.get("acme", &small).unwrap().tier, 0);
                assert_eq!(m.get("acme", &mid).unwrap().tier, 1);
                assert!(
                    matches!(
                        m.approve("acme", &mid, d.get("acme", "sup").unwrap(), 1),
                        Err(Error::Denied(_))
                    ),
                    "supervisor below tier"
                );
                assert_eq!(
                    m.approve("acme", &small, d.get("acme", "mgr").unwrap(), 1)
                        .unwrap(),
                    CaseState::Approved,
                    "higher role may approve"
                );
                assert!(matches!(
                    m.approve("acme", &small, d.get("acme", "mgr").unwrap(), 2),
                    Err(Error::State(_))
                ));
            }

            #[test]
            fn segregation_of_duties_and_dual_approval() {
                let (d, mut ids) = (people(), IdGen::new());
                let mut m = MakerChecker::new(tiers(), 3600).unwrap();
                let big = m.submit("acme", "dir1", "erp.pay", 5_000_000, 0, &mut ids);
                assert!(
                    matches!(
                        m.approve("acme", &big, d.get("acme", "dir1").unwrap(), 1),
                        Err(Error::Denied(_))
                    ),
                    "maker is a director"
                );
                assert_eq!(
                    m.approve("acme", &big, d.get("acme", "dir2").unwrap(), 1)
                        .unwrap(),
                    CaseState::Open
                );
                assert!(matches!(
                    m.approve("acme", &big, d.get("acme", "dir2").unwrap(), 2),
                    Err(Error::Conflict(_))
                ));
                assert!(matches!(
                    m.approve("acme", &big, d.get("acme", "mgr").unwrap(), 2),
                    Err(Error::Denied(_))
                ));
                let c3 = m.submit("acme", "maker", "erp.pay", 5_000_000, 0, &mut ids);
                m.approve("acme", &c3, d.get("acme", "dir1").unwrap(), 1)
                    .unwrap();
                assert_eq!(
                    m.approve("acme", &c3, d.get("acme", "dir2").unwrap(), 2)
                        .unwrap(),
                    CaseState::Approved
                );
            }

            #[test]
            fn tenant_isolation_and_inactive_approvers() {
                let (mut d, mut ids) = (people(), IdGen::new());
                let mut m = MakerChecker::new(tiers(), 3600).unwrap();
                let c = m.submit("acme", "maker", "erp.pay", 10, 0, &mut ids);
                assert!(matches!(
                    m.approve("beta", &c, d.get("beta", "other").unwrap(), 1),
                    Err(Error::NotFound(_))
                ));
                assert!(matches!(
                    m.approve("acme", &c, d.get("beta", "other").unwrap(), 1),
                    Err(Error::Denied(_))
                ));
                d.deactivate("acme", "sup").unwrap();
                assert!(matches!(
                    m.approve("acme", &c, d.get("acme", "sup").unwrap(), 1),
                    Err(Error::Denied(_))
                ));
            }

            #[test]
            fn escalation_moves_to_the_next_tier() {
                let (d, mut ids) = (people(), IdGen::new());
                let mut m = MakerChecker::new(tiers(), 100).unwrap();
                let c = m.submit("acme", "maker", "erp.pay", 500, 0, &mut ids);
                assert_eq!(
                    m.approve("acme", &c, d.get("acme", "sup").unwrap(), 50)
                        .unwrap(),
                    CaseState::Approved,
                    "in time"
                );
                let c2 = m.submit("acme", "maker", "erp.pay", 500, 0, &mut ids);
                assert!(
                    matches!(
                        m.approve("acme", &c2, d.get("acme", "sup").unwrap(), 101),
                        Err(Error::Denied(_))
                    ),
                    "escalated past supervisor"
                );
                assert_eq!(m.get("acme", &c2).unwrap().tier, 1);
                assert_eq!(
                    m.approve("acme", &c2, d.get("acme", "mgr").unwrap(), 102)
                        .unwrap(),
                    CaseState::Approved
                );
            }

            #[test]
            fn rejection_and_config_validation() {
                let (d, mut ids) = (people(), IdGen::new());
                let mut m = MakerChecker::new(tiers(), 3600).unwrap();
                let c = m.submit("acme", "maker", "erp.pay", 10, 0, &mut ids);
                assert!(m
                    .reject("acme", &c, d.get("acme", "maker").unwrap(), 1)
                    .is_err());
                m.reject("acme", &c, d.get("acme", "sup").unwrap(), 1)
                    .unwrap();
                assert_eq!(m.get("acme", &c).unwrap().state, CaseState::Rejected);
                assert!(m
                    .approve("acme", &c, d.get("acme", "mgr").unwrap(), 2)
                    .is_err());
                assert!(MakerChecker::new(vec![], 1).is_err());
                assert!(
                    MakerChecker::new(
                        vec![Tier {
                            up_to: Some(5),
                            role: "r".into(),
                            approvals: 1
                        }],
                        1
                    )
                    .is_err(),
                    "last must be unbounded"
                );
                assert!(MakerChecker::new(
                    vec![Tier {
                        up_to: None,
                        role: "r".into(),
                        approvals: 0
                    }],
                    1
                )
                .is_err());
                let desc = vec![
                    Tier {
                        up_to: Some(10),
                        role: "a".into(),
                        approvals: 1,
                    },
                    Tier {
                        up_to: Some(5),
                        role: "b".into(),
                        approvals: 1,
                    },
                    Tier {
                        up_to: None,
                        role: "c".into(),
                        approvals: 1,
                    },
                ];
                assert!(MakerChecker::new(desc, 1).is_err());
            }
        }
    }
    pub mod org {
        //! Tenant and organisation hierarchy: tenant > institution > company > legal entity > branch >
        //! plant > department. Every node belongs to exactly one tenant.

        use mouli_interface::{Error, Result};
        use std::collections::BTreeMap;

        /// Depth of a node. A child must be strictly deeper than its parent.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Level {
            /// Tenant root.
            Tenant,
            /// Institution or group.
            Institution,
            /// Company.
            Company,
            /// Legal entity.
            LegalEntity,
            /// Branch.
            Branch,
            /// Plant.
            Plant,
            /// Department.
            Department,
        }

        #[derive(Debug, Clone)]
        struct Node {
            tenant: String,
            level: Level,
            parent: Option<String>,
        }

        /// The hierarchy of all tenants.
        #[derive(Debug, Default)]
        pub struct Org {
            nodes: BTreeMap<String, Node>,
        }

        impl Org {
            /// Empty hierarchy.
            #[must_use]
            pub fn new() -> Self {
                Self::default()
            }

            /// Create a tenant. The root node has the tenant's own id.
            ///
            /// # Errors
            /// [`Error::Invalid`] for a blank id, [`Error::Conflict`] when it exists.
            pub fn create_tenant(&mut self, id: &str) -> Result<()> {
                self.insert(id, id, Level::Tenant, None)
            }

            /// Add a node under `parent`.
            ///
            /// # Errors
            /// [`Error::NotFound`] for an unknown parent, [`Error::Invalid`] when the level is not
            /// deeper than the parent's, [`Error::Conflict`] when the id exists anywhere.
            pub fn add(&mut self, id: &str, level: Level, parent: &str) -> Result<()> {
                let p = self
                    .nodes
                    .get(parent)
                    .ok_or_else(|| Error::NotFound(format!("node {parent}")))?;
                if level <= p.level {
                    return Err(Error::Invalid(format!(
                        "{level:?} cannot sit under {:?}",
                        p.level
                    )));
                }
                let tenant = p.tenant.clone();
                self.insert(id, &tenant, level, Some(parent.to_string()))
            }

            fn insert(
                &mut self,
                id: &str,
                tenant: &str,
                level: Level,
                parent: Option<String>,
            ) -> Result<()> {
                if id.trim().is_empty() {
                    return Err(Error::Invalid("node id must not be blank".into()));
                }
                if self.nodes.contains_key(id) {
                    return Err(Error::Conflict(format!("node {id} already exists")));
                }
                self.nodes.insert(
                    id.into(),
                    Node {
                        tenant: tenant.into(),
                        level,
                        parent,
                    },
                );
                Ok(())
            }

            /// Tenant that owns a node.
            #[must_use]
            pub fn tenant_of(&self, node: &str) -> Option<&str> {
                self.nodes.get(node).map(|n| n.tenant.as_str())
            }

            /// Level of a node.
            #[must_use]
            pub fn level_of(&self, node: &str) -> Option<Level> {
                self.nodes.get(node).map(|n| n.level)
            }

            /// Whether `node` is `ancestor` or below it.
            #[must_use]
            pub fn within(&self, node: &str, ancestor: &str) -> bool {
                let mut cur = Some(node);
                while let Some(id) = cur {
                    if id == ancestor {
                        return true;
                    }
                    cur = self.nodes.get(id).and_then(|n| n.parent.as_deref());
                }
                false
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn org() -> Org {
                let mut o = Org::new();
                o.create_tenant("acme").unwrap();
                o.add("acme-co", Level::Company, "acme").unwrap();
                o.add("pune", Level::Plant, "acme-co").unwrap();
                o.add("press", Level::Department, "pune").unwrap();
                o.create_tenant("beta").unwrap();
                o.add("beta-co", Level::Company, "beta").unwrap();
                o
            }

            #[test]
            fn hierarchy_rules() {
                let mut o = org();
                assert_eq!(o.tenant_of("press"), Some("acme"));
                assert!(
                    o.within("press", "acme-co")
                        && o.within("press", "press")
                        && o.within("pune", "acme")
                );
                assert!(!o.within("acme-co", "pune") && !o.within("press", "beta-co"));
                assert!(
                    matches!(o.add("x", Level::Company, "pune"), Err(Error::Invalid(_))),
                    "company under plant"
                );
                assert!(matches!(
                    o.add("x", Level::Plant, "nope"),
                    Err(Error::NotFound(_))
                ));
                assert!(
                    matches!(
                        o.add("pune", Level::Branch, "acme-co"),
                        Err(Error::Conflict(_))
                    ),
                    "ids are global"
                );
                assert!(o.create_tenant(" ").is_err());
                assert_eq!(o.level_of("pune"), Some(Level::Plant));
                assert_eq!(o.tenant_of("ghost"), None);
            }
        }
    }
    pub mod policy {
        //! Deny-by-default policy evaluation (RBAC with ABAC conditions and org scope).
        //!
        //! A request is allowed only when the principal is active, the resource sits in the principal's
        //! tenant, at least one allow rule matches, and no deny rule matches. Deny always wins.

        use crate::auth::org::Org;
        use crate::auth::principal::Directory;
        use std::collections::BTreeMap;

        /// Rule effect.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Effect {
            /// Permit.
            Allow,
            /// Forbid (overrides any allow).
            Deny,
        }

        /// Extra condition on a rule.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Cond {
            /// Principal attribute equals value.
            PrincipalAttr(String, String),
            /// Resource attribute equals value.
            ResourceAttr(String, String),
            /// The resource amount (integer minor units) is present and at most this.
            AmountAtMost(u64),
        }

        /// A policy rule, always inside one tenant.
        #[derive(Debug, Clone)]
        pub struct Rule {
            /// Rule id, quoted in decisions.
            pub id: String,
            /// Tenant the rule belongs to.
            pub tenant: String,
            /// Allow or deny.
            pub effect: Effect,
            /// Role the principal must hold, or `*`.
            pub role: String,
            /// Action: exact, `app.*`, or `*`.
            pub action: String,
            /// Resource kind or `*`.
            pub resource_kind: String,
            /// Org node the rule is limited to (covers its descendants); `None` means the whole tenant.
            pub scope: Option<String>,
            /// All must hold.
            pub conditions: Vec<Cond>,
        }

        /// What is being acted on.
        #[derive(Debug, Clone)]
        pub struct Resource {
            /// Tenant the caller claims the resource is in.
            pub tenant: String,
            /// Org node the resource belongs to.
            pub node: String,
            /// Kind, for example `invoice`.
            pub kind: String,
            /// Attributes.
            pub attrs: BTreeMap<String, String>,
            /// Amount in minor units, for limit checks.
            pub amount: Option<u64>,
        }

        /// An authorisation question.
        #[derive(Debug, Clone)]
        pub struct Request {
            /// Who.
            pub principal: String,
            /// What they want to do.
            pub action: String,
            /// To what.
            pub resource: Resource,
        }

        /// The answer.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Decision {
            /// Whether the action may proceed.
            pub allowed: bool,
            /// Human-readable reason.
            pub reason: String,
            /// Rule that decided, if any.
            pub rule: Option<String>,
        }

        fn action_matches(pattern: &str, action: &str) -> bool {
            pattern == "*"
                || pattern == action
                || pattern.strip_suffix(".*").is_some_and(|app| {
                    action
                        .strip_prefix(app)
                        .is_some_and(|rest| rest.starts_with('.'))
                })
        }

        fn deny(reason: &str, rule: Option<&str>) -> Decision {
            Decision {
                allowed: false,
                reason: reason.into(),
                rule: rule.map(str::to_string),
            }
        }

        /// Evaluate `req` against `rules`.
        #[must_use]
        pub fn evaluate(rules: &[Rule], org: &Org, dir: &Directory, req: &Request) -> Decision {
            let tenant = &req.resource.tenant;
            let Some(p) = dir.get(tenant, &req.principal) else {
                return deny("unknown principal in this tenant", None);
            };
            if !p.active {
                return deny("principal is inactive", None);
            }
            if org.tenant_of(&req.resource.node) != Some(tenant.as_str()) {
                return deny("resource node is not in the principal's tenant", None);
            }
            let mut allow: Option<&Rule> = None;
            for r in rules.iter().filter(|r| &r.tenant == tenant) {
                let hit = (r.role == "*" || p.roles.contains(&r.role))
                    && action_matches(&r.action, &req.action)
                    && (r.resource_kind == "*" || r.resource_kind == req.resource.kind)
                    && r.scope
                        .as_deref()
                        .is_none_or(|s| org.within(&req.resource.node, s))
                    && r.conditions.iter().all(|c| match c {
                        Cond::PrincipalAttr(k, v) => p.attrs.get(k) == Some(v),
                        Cond::ResourceAttr(k, v) => req.resource.attrs.get(k) == Some(v),
                        Cond::AmountAtMost(max) => req.resource.amount.is_some_and(|a| a <= *max),
                    });
                if !hit {
                    continue;
                }
                if r.effect == Effect::Deny {
                    return deny(&format!("denied by rule {}", r.id), Some(&r.id));
                }
                allow.get_or_insert(r);
            }
            match allow {
                Some(r) => Decision {
                    allowed: true,
                    reason: format!("allowed by rule {}", r.id),
                    rule: Some(r.id.clone()),
                },
                None => deny("no rule allows this action", None),
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::auth::org::Level;
            use crate::auth::principal::Kind;

            fn world() -> (Org, Directory) {
                let mut o = Org::new();
                o.create_tenant("acme").unwrap();
                o.add("co", Level::Company, "acme").unwrap();
                o.add("pune", Level::Plant, "co").unwrap();
                o.add("goa", Level::Plant, "co").unwrap();
                o.create_tenant("beta").unwrap();
                o.add("beta-co", Level::Company, "beta").unwrap();
                let mut d = Directory::new();
                d.create("ann", "acme", Kind::Human, &["clerk"], &[("dept", "press")])
                    .unwrap();
                d.create("bob", "beta", Kind::Human, &["clerk"], &[])
                    .unwrap();
                (o, d)
            }

            fn rule(id: &str, effect: Effect, action: &str) -> Rule {
                Rule {
                    id: id.into(),
                    tenant: "acme".into(),
                    effect,
                    role: "clerk".into(),
                    action: action.into(),
                    resource_kind: "*".into(),
                    scope: None,
                    conditions: vec![],
                }
            }

            fn req(
                who: &str,
                tenant: &str,
                action: &str,
                node: &str,
                amount: Option<u64>,
            ) -> Request {
                Request {
                    principal: who.into(),
                    action: action.into(),
                    resource: Resource {
                        tenant: tenant.into(),
                        node: node.into(),
                        kind: "invoice".into(),
                        attrs: BTreeMap::new(),
                        amount,
                    },
                }
            }

            #[test]
            fn default_deny_and_wildcards() {
                let (o, d) = world();
                let rules = [rule("r1", Effect::Allow, "erp.*")];
                assert!(
                    evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "acme", "erp.read", "pune", None)
                    )
                    .allowed
                );
                assert!(
                    !evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "acme", "mail.send", "pune", None)
                    )
                    .allowed
                );
                assert!(
                    !evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "acme", "erpx.read", "pune", None)
                    )
                    .allowed,
                    "prefix is not a wildcard"
                );
                assert!(
                    !evaluate(&[], &o, &d, &req("ann", "acme", "erp.read", "pune", None)).allowed
                );
            }

            #[test]
            fn deny_overrides_and_scope_limits() {
                let (o, d) = world();
                let mut scoped = rule("pune-only", Effect::Allow, "*");
                scoped.scope = Some("pune".into());
                let block = rule("no-delete", Effect::Deny, "erp.delete");
                let rules = [scoped, block, rule("all", Effect::Allow, "erp.delete")];
                assert!(
                    evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "acme", "erp.read", "pune", None)
                    )
                    .allowed
                );
                let r = evaluate(
                    &rules,
                    &o,
                    &d,
                    &req("ann", "acme", "erp.delete", "pune", None),
                );
                assert!(!r.allowed && r.rule.as_deref() == Some("no-delete"));
                let only = [rules[0].clone()];
                assert!(
                    !evaluate(&only, &o, &d, &req("ann", "acme", "erp.read", "goa", None)).allowed,
                    "other plant"
                );
            }

            #[test]
            fn conditions_and_amount_limits() {
                let (o, d) = world();
                let mut r = rule("small", Effect::Allow, "erp.pay");
                r.conditions = vec![
                    Cond::AmountAtMost(10_000),
                    Cond::PrincipalAttr("dept".into(), "press".into()),
                ];
                let rules = [r];
                assert!(
                    evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "acme", "erp.pay", "pune", Some(10_000))
                    )
                    .allowed
                );
                assert!(
                    !evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "acme", "erp.pay", "pune", Some(10_001))
                    )
                    .allowed
                );
                assert!(
                    !evaluate(&rules, &o, &d, &req("ann", "acme", "erp.pay", "pune", None)).allowed,
                    "no amount, no allow"
                );
            }

            #[test]
            fn tenant_isolation_and_inactive() {
                let (o, mut d) = world();
                let rules = [rule("r", Effect::Allow, "*")];
                assert!(
                    !evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("bob", "acme", "erp.read", "pune", None)
                    )
                    .allowed,
                    "bob is beta"
                );
                assert!(
                    !evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "acme", "erp.read", "beta-co", None)
                    )
                    .allowed,
                    "node of another tenant"
                );
                assert!(
                    !evaluate(
                        &rules,
                        &o,
                        &d,
                        &req("ann", "beta", "erp.read", "beta-co", None)
                    )
                    .allowed,
                    "claimed wrong tenant"
                );
                d.deactivate("acme", "ann").unwrap();
                let r = evaluate(
                    &rules,
                    &o,
                    &d,
                    &req("ann", "acme", "erp.read", "pune", None),
                );
                assert!(!r.allowed && r.reason.contains("inactive"));
            }
        }
    }
    pub mod principal {
        //! People, service accounts and agents.

        use mouli_interface::{Error, Result};
        use std::collections::{BTreeMap, BTreeSet};

        /// What kind of actor.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Kind {
            /// A person.
            Human,
            /// A service or integration account.
            Service,
            /// An AI agent.
            Agent,
        }

        /// An identity inside one tenant.
        #[derive(Debug, Clone)]
        pub struct Principal {
            /// Globally unique id.
            pub id: String,
            /// Owning tenant.
            pub tenant: String,
            /// Kind of actor.
            pub kind: Kind,
            /// Roles held.
            pub roles: BTreeSet<String>,
            /// Attributes for policy conditions (department, clearance, ...).
            pub attrs: BTreeMap<String, String>,
            /// Inactive principals are always denied.
            pub active: bool,
        }

        /// All principals.
        #[derive(Debug, Default)]
        pub struct Directory {
            items: BTreeMap<String, Principal>,
        }

        impl Directory {
            /// Empty directory.
            #[must_use]
            pub fn new() -> Self {
                Self::default()
            }

            /// Create an active principal.
            ///
            /// # Errors
            /// [`Error::Invalid`] for blank ids, [`Error::Conflict`] when the id exists.
            pub fn create(
                &mut self,
                id: &str,
                tenant: &str,
                kind: Kind,
                roles: &[&str],
                attrs: &[(&str, &str)],
            ) -> Result<()> {
                if id.trim().is_empty() || tenant.trim().is_empty() {
                    return Err(Error::Invalid(
                        "principal id and tenant are required".into(),
                    ));
                }
                if self.items.contains_key(id) {
                    return Err(Error::Conflict(format!("principal {id} already exists")));
                }
                self.items.insert(
                    id.into(),
                    Principal {
                        id: id.into(),
                        tenant: tenant.into(),
                        kind,
                        roles: roles.iter().map(|r| (*r).to_string()).collect(),
                        attrs: attrs
                            .iter()
                            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                            .collect(),
                        active: true,
                    },
                );
                Ok(())
            }

            /// Look up a principal inside `tenant`. Another tenant's principal looks missing.
            #[must_use]
            pub fn get(&self, tenant: &str, id: &str) -> Option<&Principal> {
                self.items.get(id).filter(|p| p.tenant == tenant)
            }

            /// Deactivate a principal.
            ///
            /// # Errors
            /// [`Error::NotFound`] when absent from `tenant`.
            pub fn deactivate(&mut self, tenant: &str, id: &str) -> Result<()> {
                let p = self
                    .items
                    .get_mut(id)
                    .filter(|p| p.tenant == tenant)
                    .ok_or_else(|| Error::NotFound(format!("principal {id}")))?;
                p.active = false;
                Ok(())
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn tenant_scoped_lookup_and_deactivation() {
                let mut d = Directory::new();
                d.create("ann", "acme", Kind::Human, &["clerk"], &[("dept", "press")])
                    .unwrap();
                assert!(d.create("ann", "beta", Kind::Human, &[], &[]).is_err());
                assert!(d.create("", "acme", Kind::Agent, &[], &[]).is_err());
                assert!(d.get("acme", "ann").unwrap().active);
                assert!(d.get("beta", "ann").is_none());
                assert!(d.deactivate("beta", "ann").is_err());
                d.deactivate("acme", "ann").unwrap();
                assert!(!d.get("acme", "ann").unwrap().active);
            }
        }
    }

    pub use authority::{Authority, Authorization};
    pub use envelope::{Claims, Verifier};
}

#[cfg(feature = "security")]
#[allow(clippy::module_inception)]
pub mod security {
    //! MOULI-SECURITY core logic: pure, deterministic building blocks with no database, network or
    //! clock. Callers pass timestamps (seconds since the Unix epoch) and persist results themselves.
    //!
    //! * [`rules`]: boolean rule trees over JSON events (and / or / not plus 13 comparisons).
    //! * [`detect`]: threshold, velocity, sequence, state-transition and statistical (z-score, MAD)
    //!   anomaly detectors.
    //! * [`scoring`]: inherent and residual risk scores and risk levels.
    //! * [`cases`]: the case and investigation state machine with a tamper-evident event history.
    //! * [`evidence`]: canonical hashing, integrity checks, bundles and legal-hold-aware retention.
    //!
    //! The ideas come from an earlier Python prototype; this is a clean re-implementation with the
    //! prototype's defects removed, not a translation. MOULI-AUTH remains the identity and
    //! authorization authority: nothing here decides who may act.
    #![forbid(unsafe_code)]

    pub mod cases {
        //! Security cases: a strict state machine, deterministic triage and escalation, merging, and a
        //! hash-chained history so any later edit of the record is detectable.

        use crate::security::scoring::Level;
        use sha2::{Digest, Sha256};
        use std::fmt;

        /// Where a case is in its life.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Status {
            /// Just raised.
            New,
            /// Accepted for handling.
            Open,
            /// Priority has been set.
            Triaged,
            /// Owned by someone.
            Assigned,
            /// Being investigated.
            Investigating,
            /// Waiting for more information.
            PendingInformation,
            /// Waiting for an approval.
            PendingApproval,
            /// Fixes are being applied.
            Remediating,
            /// Resolved, awaiting closure.
            Resolved,
            /// Closed.
            Closed,
            /// Reopened after resolution or closure.
            Reopened,
            /// Cancelled (final).
            Cancelled,
            /// Merged into another case (final).
            Duplicate,
        }

        impl Status {
            /// The states this one may move to.
            pub fn next(self) -> &'static [Status] {
                use Status::*;
                match self {
                    New => &[Open, Triaged, Cancelled, Duplicate],
                    Open => &[Triaged, Assigned, Cancelled, Duplicate],
                    Triaged => &[Assigned, Open, PendingApproval, Cancelled],
                    Assigned => &[
                        Investigating,
                        PendingInformation,
                        PendingApproval,
                        Remediating,
                        Cancelled,
                    ],
                    Investigating => &[PendingInformation, Remediating, Resolved, Cancelled],
                    PendingInformation => &[Investigating, Assigned],
                    PendingApproval => &[Remediating, Assigned, Resolved],
                    Remediating => &[Resolved, Closed, PendingApproval],
                    Resolved => &[Closed, Reopened],
                    Closed => &[Reopened],
                    Reopened => &[Open, Assigned, Investigating],
                    Cancelled | Duplicate => &[],
                }
            }

            /// Whether this state may move to `to`.
            pub fn can_move_to(self, to: Status) -> bool {
                self.next().contains(&to)
            }

            /// True for states that accept no further changes at all.
            pub fn is_final(self) -> bool {
                matches!(self, Status::Cancelled | Status::Duplicate)
            }

            /// True when work on the case has ended (final, or closed).
            pub fn is_ended(self) -> bool {
                self.is_final() || self == Status::Closed
            }
        }

        /// What went wrong.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum CaseError {
            /// A required text field is empty.
            EmptyField(&'static str),
            /// The state machine does not allow this move.
            InvalidTransition {
                /// Current state.
                from: Status,
                /// Requested state.
                to: Status,
            },
            /// This move needs a written reason.
            ReasonRequired,
            /// This move needs the case to have an assignee.
            NeedsAssignee,
            /// The case has ended and cannot take this action.
            Ended(Status),
            /// A case cannot be merged into itself.
            SameCase,
        }

        impl fmt::Display for CaseError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    CaseError::EmptyField(n) => write!(f, "{n} must not be empty"),
                    CaseError::InvalidTransition { from, to } => {
                        write!(f, "cannot move a case from {from:?} to {to:?}")
                    }
                    CaseError::ReasonRequired => write!(f, "a reason is required for this change"),
                    CaseError::NeedsAssignee => write!(f, "the case needs an assignee first"),
                    CaseError::Ended(s) => write!(f, "the case has ended ({s:?})"),
                    CaseError::SameCase => write!(f, "a case cannot be merged into itself"),
                }
            }
        }

        impl std::error::Error for CaseError {}

        /// What a history entry records.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum EventKind {
            /// The case was created.
            Created,
            /// The state changed.
            StatusChanged,
            /// An assignee was set.
            Assigned,
            /// Priority was set by triage.
            Triaged,
            /// Priority was raised to critical.
            Escalated,
            /// Cases were merged.
            Merged,
            /// A comment was added.
            Commented,
        }

        /// One tamper-evident history entry.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Event {
            /// Position in the history, from 1.
            pub seq: u64,
            /// When it happened (epoch seconds).
            pub at: i64,
            /// Who did it.
            pub actor: String,
            /// What kind of entry.
            pub kind: EventKind,
            /// Details.
            pub detail: String,
            /// Hash of the previous entry (empty for the first).
            pub prev_hash: String,
            /// Hash of this entry, covering all other fields and `prev_hash`.
            pub hash: String,
        }

        fn entry_hash(
            prev: &str,
            seq: u64,
            at: i64,
            actor: &str,
            kind: EventKind,
            detail: &str,
        ) -> String {
            let mut h = Sha256::new();
            // Length-prefixed fields so no field can bleed into the next.
            for part in [
                prev,
                &seq.to_string(),
                &at.to_string(),
                actor,
                &format!("{kind:?}"),
                detail,
            ] {
                h.update((part.len() as u64).to_be_bytes());
                h.update(part.as_bytes());
            }
            crate::security::evidence::hex(&h.finalize())
        }

        /// How two cases relate.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Relation {
            /// This case was merged into, or received, the other.
            MergedWith(String),
        }

        /// A security case.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Case {
            /// Case identifier.
            pub id: String,
            /// Short title.
            pub title: String,
            /// How bad the underlying problem is.
            pub severity: Level,
            /// How urgently it is handled.
            pub priority: Level,
            /// Current state.
            pub status: Status,
            /// Who owns it.
            pub assignee: Option<String>,
            /// Links to other cases.
            pub related: Vec<Relation>,
            events: Vec<Event>,
        }

        fn need(text: &str, name: &'static str) -> Result<(), CaseError> {
            if text.trim().is_empty() {
                Err(CaseError::EmptyField(name))
            } else {
                Ok(())
            }
        }

        impl Case {
            /// Opens a case in state [`Status::New`] with Medium priority.
            pub fn new(
                id: &str,
                title: &str,
                severity: Level,
                actor: &str,
                now: i64,
            ) -> Result<Case, CaseError> {
                need(id, "id")?;
                need(title, "title")?;
                need(actor, "actor")?;
                let mut c = Case {
                    id: id.to_string(),
                    title: title.to_string(),
                    severity,
                    priority: Level::Medium,
                    status: Status::New,
                    assignee: None,
                    related: Vec::new(),
                    events: Vec::new(),
                };
                c.record(
                    now,
                    actor,
                    EventKind::Created,
                    format!("created with severity {severity:?}"),
                );
                Ok(c)
            }

            /// Opens a case from a detection signal: critical signals start at High priority.
            pub fn from_signal(
                id: &str,
                source: &str,
                title: &str,
                severity: Level,
                actor: &str,
                now: i64,
            ) -> Result<Case, CaseError> {
                need(source, "source")?;
                let mut c = Case::new(id, title, severity, actor, now)?;
                if severity == Level::Critical {
                    c.priority = Level::High;
                }
                c.record(
                    now,
                    actor,
                    EventKind::Created,
                    format!("raised from signal source {source}"),
                );
                Ok(c)
            }

            fn record(&mut self, at: i64, actor: &str, kind: EventKind, detail: String) {
                let seq = self.events.len() as u64 + 1;
                let prev = self.events.last().map_or(String::new(), |e| e.hash.clone());
                let hash = entry_hash(&prev, seq, at, actor, kind, &detail);
                self.events.push(Event {
                    seq,
                    at,
                    actor: actor.to_string(),
                    kind,
                    detail,
                    prev_hash: prev,
                    hash,
                });
            }

            /// The history, oldest first.
            pub fn events(&self) -> &[Event] {
                &self.events
            }

            /// Checks the history chain. On failure returns the `seq` of the first bad entry.
            pub fn verify_history(&self) -> Result<(), u64> {
                let mut prev = String::new();
                for (i, e) in self.events.iter().enumerate() {
                    let ok = e.seq == i as u64 + 1
                        && e.prev_hash == prev
                        && e.hash
                            == entry_hash(&e.prev_hash, e.seq, e.at, &e.actor, e.kind, &e.detail);
                    if !ok {
                        return Err(e.seq);
                    }
                    prev = e.hash.clone();
                }
                Ok(())
            }

            fn guard_open(&self) -> Result<(), CaseError> {
                if self.status.is_ended() {
                    Err(CaseError::Ended(self.status))
                } else {
                    Ok(())
                }
            }

            /// Moves the case to `to`. Cancelling or reopening needs a reason; moving into Assigned,
            /// Investigating or Remediating needs an assignee.
            pub fn change_status(
                &mut self,
                to: Status,
                actor: &str,
                reason: &str,
                now: i64,
            ) -> Result<(), CaseError> {
                need(actor, "actor")?;
                if !self.status.can_move_to(to) {
                    return Err(CaseError::InvalidTransition {
                        from: self.status,
                        to,
                    });
                }
                if matches!(to, Status::Cancelled | Status::Reopened) && reason.trim().is_empty() {
                    return Err(CaseError::ReasonRequired);
                }
                if matches!(
                    to,
                    Status::Assigned | Status::Investigating | Status::Remediating
                ) && self.assignee.is_none()
                {
                    return Err(CaseError::NeedsAssignee);
                }
                let from = self.status;
                self.status = to;
                self.record(
                    now,
                    actor,
                    EventKind::StatusChanged,
                    format!("{from:?} -> {to:?}: {}", reason.trim()),
                );
                Ok(())
            }

            /// Sets the assignee and moves the case to Assigned (when the state machine allows it).
            pub fn assign(
                &mut self,
                assignee: &str,
                actor: &str,
                now: i64,
            ) -> Result<(), CaseError> {
                need(assignee, "assignee")?;
                need(actor, "actor")?;
                if !self.status.can_move_to(Status::Assigned) {
                    return Err(CaseError::InvalidTransition {
                        from: self.status,
                        to: Status::Assigned,
                    });
                }
                self.assignee = Some(assignee.to_string());
                self.record(
                    now,
                    actor,
                    EventKind::Assigned,
                    format!("assigned to {assignee}"),
                );
                self.change_status(Status::Assigned, actor, "assigned", now)
            }

            /// Deterministic triage: High and Critical severities lift the priority to at least their own
            /// level; lower severities leave the priority alone.
            pub fn triage(&mut self, actor: &str, now: i64) -> Result<Level, CaseError> {
                need(actor, "actor")?;
                self.guard_open()?;
                if self.severity >= Level::High && self.priority < self.severity {
                    self.priority = self.severity;
                }
                self.record(
                    now,
                    actor,
                    EventKind::Triaged,
                    format!("priority {:?}", self.priority),
                );
                Ok(self.priority)
            }

            /// Raises the priority to Critical. Needs a reason, and an open case.
            pub fn escalate(
                &mut self,
                reason: &str,
                actor: &str,
                now: i64,
            ) -> Result<(), CaseError> {
                need(actor, "actor")?;
                if reason.trim().is_empty() {
                    return Err(CaseError::ReasonRequired);
                }
                self.guard_open()?;
                self.priority = Level::Critical;
                self.record(now, actor, EventKind::Escalated, reason.trim().to_string());
                Ok(())
            }

            /// Adds a comment to the history.
            pub fn comment(&mut self, text: &str, actor: &str, now: i64) -> Result<(), CaseError> {
                need(text, "comment")?;
                need(actor, "actor")?;
                self.record(now, actor, EventKind::Commented, text.trim().to_string());
                Ok(())
            }

            /// Merges this case into `target`: this case becomes a Duplicate (only possible while the
            /// state machine allows it) and both link to each other. `target` must still be open.
            pub fn merge_into(
                &mut self,
                target: &mut Case,
                actor: &str,
                now: i64,
            ) -> Result<(), CaseError> {
                need(actor, "actor")?;
                if self.id == target.id {
                    return Err(CaseError::SameCase);
                }
                target.guard_open()?;
                if !self.status.can_move_to(Status::Duplicate) {
                    return Err(CaseError::InvalidTransition {
                        from: self.status,
                        to: Status::Duplicate,
                    });
                }
                self.change_status(
                    Status::Duplicate,
                    actor,
                    &format!("merged into {}", target.id),
                    now,
                )?;
                self.related.push(Relation::MergedWith(target.id.clone()));
                target.related.push(Relation::MergedWith(self.id.clone()));
                let src = self.id.clone();
                self.record(
                    now,
                    actor,
                    EventKind::Merged,
                    format!("merged into {}", target.id),
                );
                target.record(now, actor, EventKind::Merged, format!("merged from {src}"));
                Ok(())
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use Status::*;

            const ALL: [Status; 13] = [
                New,
                Open,
                Triaged,
                Assigned,
                Investigating,
                PendingInformation,
                PendingApproval,
                Remediating,
                Resolved,
                Closed,
                Reopened,
                Cancelled,
                Duplicate,
            ];

            fn case(sev: Level) -> Case {
                Case::new("C-1", "Suspicious login", sev, "sec-bot", 100).unwrap()
            }

            #[test]
            fn transition_table_is_exactly_as_specified() {
                let allowed = |s: Status| s.next().to_vec();
                assert_eq!(allowed(New), vec![Open, Triaged, Cancelled, Duplicate]);
                assert_eq!(allowed(Resolved), vec![Closed, Reopened]);
                assert_eq!(allowed(Closed), vec![Reopened]);
                assert!(allowed(Cancelled).is_empty() && allowed(Duplicate).is_empty());
                let edges: usize = ALL.iter().map(|s| s.next().len()).sum();
                assert_eq!(edges, 4 + 4 + 4 + 5 + 4 + 2 + 3 + 3 + 2 + 1 + 3);
            }

            #[test]
            fn no_state_moves_to_itself_or_lists_a_target_twice() {
                for s in ALL {
                    assert!(!s.can_move_to(s), "{s:?}");
                    let mut seen = std::collections::HashSet::new();
                    assert!(s.next().iter().all(|t| seen.insert(*t)), "{s:?}");
                }
            }

            #[test]
            fn every_state_is_reachable_from_new() {
                let mut seen = std::collections::HashSet::from([New]);
                let mut stack = vec![New];
                while let Some(s) = stack.pop() {
                    for t in s.next() {
                        if seen.insert(*t) {
                            stack.push(*t);
                        }
                    }
                }
                assert_eq!(seen.len(), ALL.len());
            }

            #[test]
            fn illegal_moves_are_refused_and_change_nothing() {
                let mut c = case(Level::Low);
                let before = c.clone();
                assert_eq!(
                    c.change_status(Closed, "a", "x", 1),
                    Err(CaseError::InvalidTransition {
                        from: New,
                        to: Closed
                    })
                );
                assert_eq!(c, before);
            }

            #[test]
            fn assignee_is_required_for_working_states() {
                let mut c = case(Level::Low);
                c.change_status(Open, "a", "", 1).unwrap();
                assert_eq!(
                    c.change_status(Assigned, "a", "", 2),
                    Err(CaseError::NeedsAssignee)
                );
                c.assign("alice", "a", 3).unwrap();
                assert_eq!((c.status, c.assignee.as_deref()), (Assigned, Some("alice")));
                c.change_status(Investigating, "alice", "", 4).unwrap();
            }

            #[test]
            fn full_lifecycle_with_reopen() {
                let mut c = case(Level::High);
                c.change_status(Open, "a", "", 1).unwrap();
                c.assign("alice", "a", 2).unwrap();
                c.change_status(Investigating, "alice", "", 3).unwrap();
                c.change_status(Resolved, "alice", "fixed", 4).unwrap();
                assert_eq!(
                    c.change_status(Reopened, "bob", "  ", 5),
                    Err(CaseError::ReasonRequired)
                );
                c.change_status(Closed, "alice", "", 5).unwrap();
                assert!(c.status.is_ended() && !c.status.is_final());
                c.change_status(Reopened, "bob", "recurred", 6).unwrap();
                c.change_status(Investigating, "alice", "", 7).unwrap();
                assert_eq!(c.verify_history(), Ok(()));
                assert_eq!(c.events().len(), 1 + 1 + 2 + 1 + 1 + 1 + 1 + 1);
            }

            #[test]
            fn cancel_needs_a_reason_and_is_final() {
                let mut c = case(Level::Low);
                assert_eq!(
                    c.change_status(Cancelled, "a", "", 1),
                    Err(CaseError::ReasonRequired)
                );
                c.change_status(Cancelled, "a", "false alarm", 1).unwrap();
                for to in ALL {
                    assert!(c.change_status(to, "a", "r", 2).is_err());
                }
                assert_eq!(c.triage("a", 3), Err(CaseError::Ended(Cancelled)));
                assert_eq!(c.escalate("why", "a", 3), Err(CaseError::Ended(Cancelled)));
            }

            #[test]
            fn triage_lifts_only_high_severity() {
                let mut crit = case(Level::Critical);
                assert_eq!(crit.triage("a", 1), Ok(Level::Critical));
                let mut high = case(Level::High);
                assert_eq!(high.triage("a", 1), Ok(Level::High));
                let mut med = case(Level::Medium);
                assert_eq!(med.triage("a", 1), Ok(Level::Medium));
                let mut low = case(Level::Low);
                assert_eq!(low.triage("a", 1), Ok(Level::Medium));
                // Never lowers an already-raised priority.
                let mut raised = case(Level::High);
                raised.escalate("boss said so", "a", 1).unwrap();
                assert_eq!(raised.triage("a", 2), Ok(Level::Critical));
            }

            #[test]
            fn escalation_needs_a_reason_and_sets_critical() {
                let mut c = case(Level::Low);
                assert_eq!(c.escalate(" ", "a", 1), Err(CaseError::ReasonRequired));
                c.escalate("data leaving the network", "a", 1).unwrap();
                assert_eq!(c.priority, Level::Critical);
                assert_eq!(c.events().last().unwrap().kind, EventKind::Escalated);
            }

            #[test]
            fn signals_start_at_the_right_priority() {
                let crit = Case::from_signal(
                    "C-2",
                    "anomaly-engine",
                    "Odd payment",
                    Level::Critical,
                    "bot",
                    1,
                )
                .unwrap();
                assert_eq!(
                    (crit.priority, crit.severity),
                    (Level::High, Level::Critical)
                );
                let low =
                    Case::from_signal("C-3", "anomaly-engine", "Odd payment", Level::Low, "bot", 1)
                        .unwrap();
                assert_eq!(low.priority, Level::Medium);
                assert_eq!(
                    Case::from_signal("C-4", " ", "t", Level::Low, "bot", 1),
                    Err(CaseError::EmptyField("source"))
                );
            }

            #[test]
            fn empty_fields_are_refused() {
                assert_eq!(
                    Case::new("", "t", Level::Low, "a", 1),
                    Err(CaseError::EmptyField("id"))
                );
                assert_eq!(
                    Case::new("i", " ", Level::Low, "a", 1),
                    Err(CaseError::EmptyField("title"))
                );
                assert_eq!(
                    Case::new("i", "t", Level::Low, "", 1),
                    Err(CaseError::EmptyField("actor"))
                );
                let mut c = case(Level::Low);
                assert_eq!(c.comment("", "a", 1), Err(CaseError::EmptyField("comment")));
                assert_eq!(c.assign("", "a", 1), Err(CaseError::EmptyField("assignee")));
            }

            #[test]
            fn merge_links_both_cases_and_marks_the_source() {
                let mut a = Case::new("A", "dup", Level::Low, "x", 1).unwrap();
                let mut b = Case::new("B", "main", Level::Low, "x", 1).unwrap();
                a.merge_into(&mut b, "x", 2).unwrap();
                assert_eq!(a.status, Duplicate);
                assert_eq!(a.related, vec![Relation::MergedWith("B".into())]);
                assert_eq!(b.related, vec![Relation::MergedWith("A".into())]);
                assert_eq!(b.status, New);
                assert_eq!(a.verify_history(), Ok(()));
                assert_eq!(b.verify_history(), Ok(()));
            }

            #[test]
            fn merge_refusals_leave_both_cases_untouched() {
                let mut a = Case::new("A", "x", Level::Low, "x", 1).unwrap();
                let mut same = a.clone();
                assert_eq!(a.merge_into(&mut same, "x", 2), Err(CaseError::SameCase));
                let mut deep = Case::new("D", "x", Level::Low, "x", 1).unwrap();
                deep.change_status(Open, "x", "", 1).unwrap();
                deep.assign("al", "x", 2).unwrap();
                let mut b = Case::new("B", "x", Level::Low, "x", 1).unwrap();
                let (deep0, b0) = (deep.clone(), b.clone());
                // Assigned cases can no longer be marked Duplicate.
                assert!(matches!(
                    deep.merge_into(&mut b, "x", 3),
                    Err(CaseError::InvalidTransition { .. })
                ));
                assert_eq!((&deep, &b), (&deep0, &b0));
                let mut closed = Case::new("Z", "x", Level::Low, "x", 1).unwrap();
                closed.change_status(Cancelled, "x", "no", 1).unwrap();
                let mut src = Case::new("S", "x", Level::Low, "x", 1).unwrap();
                assert_eq!(
                    src.merge_into(&mut closed, "x", 2),
                    Err(CaseError::Ended(Cancelled))
                );
                assert_eq!(src.status, New);
            }

            #[test]
            fn history_chain_detects_every_kind_of_tampering() {
                let mut c = case(Level::Low);
                c.change_status(Open, "a", "", 1).unwrap();
                c.comment("looked at logs", "a", 2).unwrap();
                c.comment("nothing found", "a", 3).unwrap();
                assert_eq!(c.verify_history(), Ok(()));
                let orig = c.clone();
                let mut edited = orig.clone();
                edited.events[1].detail = "Open -> Closed".into();
                assert_eq!(edited.verify_history(), Err(2));
                let mut actor = orig.clone();
                actor.events[2].actor = "mallory".into();
                assert_eq!(actor.verify_history(), Err(3));
                let mut time = orig.clone();
                time.events[3].at += 1;
                assert_eq!(time.verify_history(), Err(4));
                let mut removed = orig.clone();
                removed.events.remove(1);
                // Entry 3 is now the first one that does not follow its predecessor.
                assert_eq!(removed.verify_history(), Err(3));
                let mut reordered = orig.clone();
                reordered.events.swap(1, 2);
                assert!(reordered.verify_history().is_err());
                let mut truncated_tail_ok = orig.clone();
                truncated_tail_ok.events.pop();
                // Dropping the newest entries is not detectable from the chain alone.
                assert_eq!(truncated_tail_ok.verify_history(), Ok(()));
            }

            #[test]
            fn identical_histories_hash_identically_and_fields_cannot_bleed() {
                let a = Case::new("C", "t", Level::Low, "ab", 5).unwrap();
                let b = Case::new("C", "t", Level::Low, "ab", 5).unwrap();
                assert_eq!(a.events()[0].hash, b.events()[0].hash);
                assert_ne!(
                    entry_hash("", 1, 1, "ab", EventKind::Commented, "c"),
                    entry_hash("", 1, 1, "a", EventKind::Commented, "bc")
                );
            }
        }
    }
    pub mod detect {
        //! Anomaly detectors. All are pure functions over values the caller supplies.

        use std::collections::{HashMap, HashSet};

        /// Spread used instead of zero so a flat history still flags any change (finite, deterministic).
        const MIN_SPREAD: f64 = 1e-6;
        /// Default z-score alarm level.
        pub const Z_THRESHOLD: f64 = 3.0;
        /// Default modified-z (MAD) alarm level (Iglewicz and Hoaglin).
        pub const MAD_THRESHOLD: f64 = 3.5;

        /// A comparison for [`check_thresholds`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Cmp {
            /// Greater than.
            Gt,
            /// Greater than or equal.
            Gte,
            /// Less than.
            Lt,
            /// Less than or equal.
            Lte,
            /// Equal.
            Eq,
            /// Not equal.
            Neq,
        }

        /// A metric limit with a severity label.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Threshold {
            /// Metric the limit applies to.
            pub metric: String,
            /// Comparison that signals a breach.
            pub cmp: Cmp,
            /// Limit value.
            pub value: f64,
            /// Severity label reported on breach.
            pub severity: String,
        }

        /// A breached threshold.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Breach {
            /// The threshold that was breached.
            pub threshold: Threshold,
            /// The observed value.
            pub actual: f64,
            /// Human-readable reason.
            pub explanation: String,
        }

        /// Returns every threshold for `metric` that `actual` breaches. A non-finite `actual` breaches
        /// nothing.
        pub fn check_thresholds(
            thresholds: &[Threshold],
            metric: &str,
            actual: f64,
        ) -> Vec<Breach> {
            if !actual.is_finite() {
                return Vec::new();
            }
            thresholds
                .iter()
                .filter(|t| t.metric == metric)
                .filter(|t| match t.cmp {
                    Cmp::Gt => actual > t.value,
                    Cmp::Gte => actual >= t.value,
                    Cmp::Lt => actual < t.value,
                    Cmp::Lte => actual <= t.value,
                    Cmp::Eq => actual == t.value,
                    Cmp::Neq => actual != t.value,
                })
                .map(|t| Breach {
                    explanation: format!(
                        "{} breached: actual {actual}, limit {:?} {}",
                        t.metric, t.cmp, t.value
                    ),
                    threshold: t.clone(),
                    actual,
                })
                .collect()
        }

        /// Too many events inside a time window.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Velocity {
            /// Events found in the window.
            pub count: usize,
            /// Count that triggers the alarm.
            pub threshold: usize,
            /// Window length in seconds.
            pub window_secs: i64,
            /// Human-readable reason.
            pub explanation: String,
        }

        /// Counts `timestamps` (epoch seconds) in `[now - window_secs, now]`; alarms at `threshold` or
        /// more. Timestamps in the future are ignored. A zero threshold or non-positive window never
        /// alarms.
        pub fn velocity(
            timestamps: &[i64],
            now: i64,
            window_secs: i64,
            threshold: usize,
        ) -> Option<Velocity> {
            if threshold == 0 || window_secs <= 0 {
                return None;
            }
            let start = now.saturating_sub(window_secs);
            let count = timestamps
                .iter()
                .filter(|t| **t >= start && **t <= now)
                .count();
            (count >= threshold).then(|| Velocity {
                count,
                threshold,
                window_secs,
                explanation: format!(
                    "{count} events within {window_secs}s (threshold {threshold})"
                ),
            })
        }

        /// A step that appeared before a step that should have preceded it.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct OrderViolation {
            /// The step seen too early.
            pub early: String,
            /// The step that should have come first.
            pub should_precede: String,
            /// Human-readable reason.
            pub explanation: String,
        }

        /// Compares an observed sequence with the expected order. Each out-of-order pair is reported
        /// once. Steps that are not in `expected` are ignored; repeats use their first occurrence.
        pub fn check_sequence(expected: &[&str], actual: &[&str]) -> Vec<OrderViolation> {
            let rank = |s: &str| expected.iter().position(|e| *e == s);
            let mut seen: Vec<(&str, usize)> = Vec::new();
            let mut out = Vec::new();
            let mut reported: HashSet<(String, String)> = HashSet::new();
            for step in actual {
                let Some(r) = rank(step) else { continue };
                if seen.iter().any(|(s, _)| s == step) {
                    continue;
                }
                for (prev, prev_rank) in &seen {
                    if *prev_rank > r && reported.insert(((*prev).to_string(), (*step).to_string()))
                    {
                        out.push(OrderViolation {
                            early: (*prev).to_string(),
                            should_precede: (*step).to_string(),
                            explanation: format!(
                                "{prev} appeared before {step}, but {step} should come first"
                            ),
                        });
                    }
                }
                seen.push((step, r));
            }
            out
        }

        /// Which state changes are allowed, per state.
        #[derive(Debug, Clone, Default)]
        pub struct TransitionRules {
            allowed: HashMap<String, HashSet<String>>,
        }

        /// An unexpected state change.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct BadTransition {
            /// Previous state.
            pub from: String,
            /// New state.
            pub to: String,
            /// Human-readable reason.
            pub explanation: String,
        }

        impl TransitionRules {
            /// An empty rule set (every known-state change is then a violation).
            pub fn new() -> Self {
                Self::default()
            }

            /// Allows `from` to move to each state in `to`.
            pub fn allow(mut self, from: &str, to: &[&str]) -> Self {
                self.allowed
                    .entry(from.to_string())
                    .or_default()
                    .extend(to.iter().map(|s| (*s).to_string()));
                self
            }

            /// Checks one change. No change, or a change out of a state the rules do not mention, is
            /// not judged (the rules say nothing about it).
            pub fn check(&self, from: &str, to: &str) -> Option<BadTransition> {
                if from == to {
                    return None;
                }
                let allowed = self.allowed.get(from)?;
                (!allowed.contains(to)).then(|| BadTransition {
                    from: from.to_string(),
                    to: to.to_string(),
                    explanation: format!("invalid state transition {from} -> {to}"),
                })
            }
        }

        /// A value far from its history.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Deviation {
            /// `"z_score"` or `"mad"`.
            pub method: &'static str,
            /// The observed value.
            pub actual: f64,
            /// The history's centre (mean or median).
            pub expected: f64,
            /// Signed score (z or modified z).
            pub score: f64,
            /// The alarm level that was exceeded.
            pub threshold: f64,
            /// Human-readable reason.
            pub explanation: String,
        }

        fn clean(history: &[f64]) -> Vec<f64> {
            history.iter().copied().filter(|v| v.is_finite()).collect()
        }

        fn median(sorted: &[f64]) -> f64 {
            let n = sorted.len();
            match n {
                0 => 0.0,
                _ if n % 2 == 1 => sorted.get(n / 2).copied().unwrap_or(0.0),
                _ => {
                    let (a, b) = (sorted.get(n / 2 - 1), sorted.get(n / 2));
                    (a.copied().unwrap_or(0.0) + b.copied().unwrap_or(0.0)) / 2.0
                }
            }
        }

        /// Z-score against the sample mean and standard deviation of `history`. Needs at least 2 finite
        /// history values and a finite `current`; alarms when `|z| > threshold`.
        pub fn z_score(history: &[f64], current: f64, threshold: f64) -> Option<Deviation> {
            let h = clean(history);
            if h.len() < 2 || !current.is_finite() {
                return None;
            }
            let n = h.len() as f64;
            let mean = h.iter().sum::<f64>() / n;
            let var = h.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
            let sd = var.sqrt().max(MIN_SPREAD);
            let z = (current - mean) / sd;
            (z.abs() > threshold).then(|| Deviation {
                method: "z_score",
                actual: current,
                expected: mean,
                score: z,
                threshold,
                explanation: format!("z-score {z:.2} exceeds {threshold}; mean {mean:.2}"),
            })
        }

        /// Modified z-score from the median absolute deviation (robust to outliers in `history`). Needs
        /// at least 3 finite history values and a finite `current`; alarms when `|score| > threshold`.
        pub fn mad(history: &[f64], current: f64, threshold: f64) -> Option<Deviation> {
            let mut h = clean(history);
            if h.len() < 3 || !current.is_finite() {
                return None;
            }
            h.sort_by(f64::total_cmp);
            let med = median(&h);
            let mut devs: Vec<f64> = h.iter().map(|v| (v - med).abs()).collect();
            devs.sort_by(f64::total_cmp);
            let spread = median(&devs).max(MIN_SPREAD);
            let score = 0.6745 * (current - med) / spread;
            (score.abs() > threshold).then(|| Deviation {
                method: "mad",
                actual: current,
                expected: med,
                score,
                threshold,
                explanation: format!("modified z {score:.2} exceeds {threshold}; median {med:.2}"),
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn th(cmp: Cmp, value: f64) -> Threshold {
                Threshold {
                    metric: "amount".into(),
                    cmp,
                    value,
                    severity: "HIGH".into(),
                }
            }

            #[test]
            fn thresholds_each_comparison_at_the_boundary() {
                let ts = [
                    th(Cmp::Gt, 10.0),
                    th(Cmp::Gte, 10.0),
                    th(Cmp::Lt, 10.0),
                    th(Cmp::Lte, 10.0),
                    th(Cmp::Eq, 10.0),
                    th(Cmp::Neq, 10.0),
                ];
                let at = |v: f64| {
                    check_thresholds(&ts, "amount", v)
                        .iter()
                        .map(|b| b.threshold.cmp)
                        .collect::<Vec<_>>()
                };
                assert_eq!(at(10.0), vec![Cmp::Gte, Cmp::Lte, Cmp::Eq]);
                assert_eq!(at(11.0), vec![Cmp::Gt, Cmp::Gte, Cmp::Neq]);
                assert_eq!(at(9.0), vec![Cmp::Lt, Cmp::Lte, Cmp::Neq]);
            }

            #[test]
            fn thresholds_ignore_other_metrics_and_nonfinite_values() {
                let ts = [th(Cmp::Gt, 1.0)];
                assert!(check_thresholds(&ts, "other", 99.0).is_empty());
                assert!(check_thresholds(&ts, "amount", f64::NAN).is_empty());
                assert!(check_thresholds(&ts, "amount", f64::INFINITY).is_empty());
                let b = check_thresholds(&ts, "amount", 5.0);
                assert_eq!(b[0].actual, 5.0);
                assert!(b[0].explanation.contains("amount breached"));
            }

            #[test]
            fn velocity_window_edges_are_inclusive() {
                let now = 1_000;
                let stamps = [1_000 - 60, 1_000 - 59, 1_000, 1_000 + 1, 1_000 - 61];
                // In [940, 1000]: 940, 941, 1000 are in; 1001 (future) and 939 are out.
                let v = velocity(&stamps, now, 60, 3).unwrap();
                assert_eq!(v.count, 3);
                assert!(velocity(&stamps, now, 60, 4).is_none());
                assert_eq!(velocity(&stamps, now, 60, 1).unwrap().count, 3);
            }

            #[test]
            fn velocity_degenerate_settings_never_alarm() {
                assert!(velocity(&[1, 2, 3], 3, 60, 0).is_none());
                assert!(velocity(&[1, 2, 3], 3, 0, 1).is_none());
                assert!(velocity(&[1, 2, 3], 3, -5, 1).is_none());
                assert!(velocity(&[], 3, 60, 1).is_none());
                assert!(velocity(&[i64::MIN], i64::MIN, 60, 1).is_some());
            }

            const PO: [&str; 5] = [
                "CREATE_PO",
                "APPROVE_PO",
                "RECEIVE_GOODS",
                "INVOICE",
                "PAYMENT",
            ];

            #[test]
            fn sequence_in_order_and_with_gaps_is_fine() {
                assert!(check_sequence(&PO, &PO).is_empty());
                assert!(check_sequence(&PO, &["CREATE_PO", "INVOICE"]).is_empty());
                assert!(check_sequence(&PO, &[]).is_empty());
                assert!(check_sequence(&PO, &["UNRELATED", "CREATE_PO"]).is_empty());
            }

            #[test]
            fn sequence_inversions_are_reported_once_per_pair() {
                let v = check_sequence(&PO, &["CREATE_PO", "PAYMENT", "APPROVE_PO", "INVOICE"]);
                let pairs: Vec<_> = v
                    .iter()
                    .map(|x| (x.early.as_str(), x.should_precede.as_str()))
                    .collect();
                assert_eq!(
                    pairs,
                    vec![("PAYMENT", "APPROVE_PO"), ("PAYMENT", "INVOICE")]
                );
                let rep = check_sequence(&PO, &["PAYMENT", "CREATE_PO", "PAYMENT", "CREATE_PO"]);
                assert_eq!(rep.len(), 1);
                assert!(rep[0]
                    .explanation
                    .contains("PAYMENT appeared before CREATE_PO"));
            }

            #[test]
            fn transitions_judge_only_known_states() {
                let rules = TransitionRules::new()
                    .allow("ACTIVE", &["BLOCKED", "REVIEW"])
                    .allow("BLOCKED", &["REVIEW", "TERMINATED"]);
                assert!(rules.check("ACTIVE", "BLOCKED").is_none());
                assert!(rules.check("BLOCKED", "REVIEW").is_none());
                let bad = rules.check("BLOCKED", "ACTIVE").unwrap();
                assert_eq!((bad.from.as_str(), bad.to.as_str()), ("BLOCKED", "ACTIVE"));
                assert!(rules.check("ACTIVE", "TERMINATED").is_some());
                assert!(rules.check("ACTIVE", "ACTIVE").is_none());
                assert!(rules.check("MYSTERY", "ACTIVE").is_none());
                assert!(TransitionRules::new().check("A", "B").is_none());
            }

            #[test]
            fn z_score_needs_history_and_flags_outliers() {
                let h = [10.0, 11.0, 9.0, 10.0, 10.5, 9.5];
                assert!(z_score(&h, 10.2, Z_THRESHOLD).is_none());
                let d = z_score(&h, 40.0, Z_THRESHOLD).unwrap();
                assert_eq!(d.method, "z_score");
                assert!(d.score > 3.0 && (d.expected - 10.0).abs() < 1e-9);
                assert!(z_score(&h, -20.0, Z_THRESHOLD).unwrap().score < -3.0);
                assert!(z_score(&[5.0], 99.0, Z_THRESHOLD).is_none());
                assert!(z_score(&h, f64::NAN, Z_THRESHOLD).is_none());
            }

            #[test]
            fn z_score_exact_value() {
                // mean 5, sample sd = sqrt(((-1)^2 + 0 + 1^2) / 2) = 1; z = (9 - 5) / 1 = 4.
                let d = z_score(&[4.0, 5.0, 6.0], 9.0, 3.0).unwrap();
                assert!((d.score - 4.0).abs() < 1e-12 && (d.expected - 5.0).abs() < 1e-12);
            }

            #[test]
            fn flat_history_flags_any_change_but_not_sameness() {
                let flat = [7.0, 7.0, 7.0, 7.0];
                assert!(z_score(&flat, 7.0, Z_THRESHOLD).is_none());
                assert!(z_score(&flat, 7.5, Z_THRESHOLD).is_some());
                assert!(mad(&flat, 7.0, MAD_THRESHOLD).is_none());
                assert!(mad(&flat, 7.5, MAD_THRESHOLD).is_some());
            }

            #[test]
            fn nonfinite_history_values_are_dropped() {
                let h = [10.0, f64::NAN, 11.0, f64::INFINITY, 9.0, 10.0];
                assert!(z_score(&h, 10.0, Z_THRESHOLD).is_none());
                assert!(z_score(&[f64::NAN, 1.0, f64::NAN], 5.0, 3.0).is_none());
            }

            #[test]
            fn mad_resists_outliers_that_fool_the_z_score() {
                // One huge past value inflates the standard deviation, hiding a moderate spike.
                let h = [10.0, 10.0, 11.0, 9.0, 10.0, 10.0, 1000.0];
                assert!(z_score(&h, 40.0, Z_THRESHOLD).is_none());
                let d = mad(&h, 40.0, MAD_THRESHOLD).unwrap();
                assert_eq!(d.method, "mad");
                assert!((d.expected - 10.0).abs() < 1e-12);
            }

            #[test]
            fn mad_exact_value_and_minimum_history() {
                // median 5, deviations [1, 0, 1, 4] -> sorted [0, 1, 1, 4], MAD 1; score = 0.6745 * 10 / 1.
                let d = mad(&[4.0, 5.0, 6.0, 9.0], 15.0, 3.5).unwrap();
                assert!((d.expected - 5.5).abs() < 1e-12 || (d.expected - 5.0).abs() < 1e-12);
                assert!(mad(&[1.0, 2.0], 99.0, 3.5).is_none());
                assert!(mad(&[1.0, 2.0, 3.0], f64::INFINITY, 3.5).is_none());
            }

            #[test]
            fn detectors_never_panic_on_extremes() {
                let h = [f64::MAX, f64::MIN, 0.0, f64::MAX];
                let _ = z_score(&h, f64::MAX, 3.0);
                let _ = mad(&h, f64::MIN, 3.5);
                let _ = velocity(&[i64::MAX, i64::MIN], i64::MAX, i64::MAX, 1);
                let _ = check_sequence(&[], &["A", "B"]);
            }
        }
    }
    pub mod evidence {
        //! Evidence: canonical JSON hashing, integrity checks, bundles, and retention that respects
        //! legal holds. Hashes prove a record is unchanged; they are not signatures and do not prove who
        //! created it.

        use serde_json::Value;
        use sha2::{Digest, Sha256};
        use std::fmt;

        /// Lower-case hex of `bytes`.
        pub fn hex(bytes: &[u8]) -> String {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            let mut s = String::with_capacity(bytes.len() * 2);
            for b in bytes {
                s.push(char::from(DIGITS[usize::from(b >> 4)]));
                s.push(char::from(DIGITS[usize::from(b & 15)]));
            }
            s
        }

        /// SHA-256 of `text` as lower-case hex.
        pub fn sha256_hex(text: &str) -> String {
            hex(&Sha256::digest(text.as_bytes()))
        }

        /// JSON with object keys sorted and no extra whitespace, so equal data always hashes equally
        /// (independent of key order or how the JSON was formatted).
        pub fn canonical_json(v: &Value) -> String {
            match v {
                Value::Object(m) => {
                    let mut keys: Vec<&String> = m.keys().collect();
                    keys.sort();
                    let parts: Vec<String> = keys
                        .into_iter()
                        .map(|k| {
                            format!(
                                "{}:{}",
                                Value::String(k.clone()),
                                m.get(k).map_or("null".to_string(), canonical_json)
                            )
                        })
                        .collect();
                    format!("{{{}}}", parts.join(","))
                }
                Value::Array(a) => format!(
                    "[{}]",
                    a.iter().map(canonical_json).collect::<Vec<_>>().join(",")
                ),
                other => other.to_string(),
            }
        }

        /// Why evidence could not be created.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum EvidenceError {
            /// A required text field is empty.
            EmptyField(&'static str),
            /// A bundle must contain at least one item.
            EmptyBundle,
            /// The same evidence id was listed twice.
            DuplicateId(String),
        }

        impl fmt::Display for EvidenceError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    EvidenceError::EmptyField(n) => write!(f, "{n} must not be empty"),
                    EvidenceError::EmptyBundle => {
                        write!(f, "a bundle needs at least one evidence item")
                    }
                    EvidenceError::DuplicateId(i) => write!(f, "evidence '{i}' is listed twice"),
                }
            }
        }

        impl std::error::Error for EvidenceError {}

        /// Result of an integrity check.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Integrity {
            /// The content still matches its recorded hash.
            Valid,
            /// The content no longer matches.
            Mismatch {
                /// Hash recorded when the evidence was collected.
                stored: String,
                /// Hash of the content as it is now.
                computed: String,
            },
        }

        /// A piece of evidence with a fingerprint of its content.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Evidence {
            /// Evidence identifier.
            pub id: String,
            /// Kind, for example `log_extract` or `screenshot`.
            pub kind: String,
            /// The entity it concerns, if any.
            pub entity: Option<String>,
            /// When it was collected (epoch seconds).
            pub collected_at: i64,
            content: Value,
            content_hash: String,
        }

        impl Evidence {
            /// Records new evidence, hashing its canonical content.
            pub fn new(
                id: &str,
                kind: &str,
                content: Value,
                entity: Option<&str>,
                collected_at: i64,
            ) -> Result<Evidence, EvidenceError> {
                if id.trim().is_empty() {
                    return Err(EvidenceError::EmptyField("id"));
                }
                if kind.trim().is_empty() {
                    return Err(EvidenceError::EmptyField("kind"));
                }
                let content_hash = sha256_hex(&canonical_json(&content));
                Ok(Evidence {
                    id: id.to_string(),
                    kind: kind.to_string(),
                    entity: entity.map(str::to_string),
                    collected_at,
                    content,
                    content_hash,
                })
            }

            /// Rebuilds evidence loaded from storage, keeping the stored hash so [`Evidence::verify`]
            /// can notice if the stored content was altered.
            pub fn restore(
                id: &str,
                kind: &str,
                content: Value,
                stored_hash: &str,
                entity: Option<&str>,
                collected_at: i64,
            ) -> Evidence {
                Evidence {
                    id: id.to_string(),
                    kind: kind.to_string(),
                    entity: entity.map(str::to_string),
                    collected_at,
                    content,
                    content_hash: stored_hash.to_string(),
                }
            }

            /// The evidence content.
            pub fn content(&self) -> &Value {
                &self.content
            }

            /// The recorded content hash.
            pub fn stored_hash(&self) -> &str {
                &self.content_hash
            }

            /// Recomputes the content hash and compares it with the recorded one.
            pub fn verify(&self) -> Integrity {
                let computed = sha256_hex(&canonical_json(&self.content));
                if computed == self.content_hash {
                    Integrity::Valid
                } else {
                    Integrity::Mismatch {
                        stored: self.content_hash.clone(),
                        computed,
                    }
                }
            }
        }

        /// A problem found when re-checking a bundle.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum BundleProblem {
            /// The bundle's own recorded hash no longer matches its contents list.
            BundleAltered,
            /// A listed item was not supplied.
            Missing(String),
            /// A listed item's content hash differs from the one recorded in the bundle.
            Changed(String),
            /// A listed item's content no longer matches its own recorded hash.
            ContentAltered(String),
        }

        /// A named, hash-sealed set of evidence for a purpose such as an investigation.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Bundle {
            /// Bundle name.
            pub name: String,
            /// What the bundle is for.
            pub purpose: String,
            items: Vec<(String, String)>,
            hash: String,
        }

        fn bundle_hash(name: &str, purpose: &str, items: &[(String, String)]) -> String {
            let list: Vec<Value> = items
                .iter()
                .map(|(id, h)| serde_json::json!({"id": id, "hash": h}))
                .collect();
            sha256_hex(&canonical_json(
                &serde_json::json!({"name": name, "purpose": purpose, "items": list}),
            ))
        }

        impl Bundle {
            /// Seals `evidence` (any order) into a bundle. Ids must be unique and the list non-empty.
            pub fn new(
                name: &str,
                purpose: &str,
                evidence: &[&Evidence],
            ) -> Result<Bundle, EvidenceError> {
                if name.trim().is_empty() {
                    return Err(EvidenceError::EmptyField("name"));
                }
                if purpose.trim().is_empty() {
                    return Err(EvidenceError::EmptyField("purpose"));
                }
                if evidence.is_empty() {
                    return Err(EvidenceError::EmptyBundle);
                }
                let mut items: Vec<(String, String)> = evidence
                    .iter()
                    .map(|e| (e.id.clone(), e.content_hash.clone()))
                    .collect();
                items.sort();
                if let Some(w) = items.windows(2).find(|w| w[0].0 == w[1].0) {
                    return Err(EvidenceError::DuplicateId(w[0].0.clone()));
                }
                let hash = bundle_hash(name, purpose, &items);
                Ok(Bundle {
                    name: name.to_string(),
                    purpose: purpose.to_string(),
                    items,
                    hash,
                })
            }

            /// The bundle's seal.
            pub fn hash(&self) -> &str {
                &self.hash
            }

            /// Ids of the sealed items, sorted.
            pub fn ids(&self) -> Vec<&str> {
                self.items.iter().map(|(i, _)| i.as_str()).collect()
            }

            /// Re-checks the bundle against `current` evidence. An empty result means everything is
            /// intact. Extra evidence in `current` that the bundle does not list is ignored.
            pub fn verify(&self, current: &[&Evidence]) -> Vec<BundleProblem> {
                let mut out = Vec::new();
                if bundle_hash(&self.name, &self.purpose, &self.items) != self.hash {
                    out.push(BundleProblem::BundleAltered);
                }
                for (id, recorded) in &self.items {
                    match current.iter().find(|e| &e.id == id) {
                        None => out.push(BundleProblem::Missing(id.clone())),
                        Some(e) => {
                            if &e.content_hash != recorded {
                                out.push(BundleProblem::Changed(id.clone()));
                            } else if e.verify() != Integrity::Valid {
                                out.push(BundleProblem::ContentAltered(id.clone()));
                            }
                        }
                    }
                }
                out
            }
        }

        /// What a legal hold covers.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum HoldScope {
            /// Everything.
            All,
            /// Every item about this entity.
            Entity(String),
            /// This one item.
            Evidence(String),
        }

        /// A legal hold that stops evidence from expiring while it is active.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct LegalHold {
            /// Hold identifier.
            pub id: String,
            /// What it covers.
            pub scope: HoldScope,
            /// Whether it is in force.
            pub active: bool,
        }

        impl LegalHold {
            fn covers(&self, e: &Evidence) -> bool {
                self.active
                    && match &self.scope {
                        HoldScope::All => true,
                        HoldScope::Entity(x) => e.entity.as_deref() == Some(x.as_str()),
                        HoldScope::Evidence(id) => &e.id == id,
                    }
            }
        }

        /// The retention rule for an item.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Policy {
            /// Keep indefinitely.
            Keep,
            /// Move to archive.
            Archive,
            /// A person must review it.
            Review,
            /// May expire this many days after collection.
            ExpireAfterDays(u32),
        }

        /// The retention verdict. It is advice only: nothing here deletes anything.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Decision {
            /// Protected by this legal hold.
            Held(String),
            /// Keep.
            Keep,
            /// Archive.
            Archive,
            /// Needs human review.
            Review,
            /// Past its retention period and unprotected.
            Expired,
        }

        /// Decides what retention allows for `e` at `now`. An active hold that covers the item always
        /// wins, whatever the policy.
        pub fn evaluate_retention(
            e: &Evidence,
            policy: Policy,
            holds: &[LegalHold],
            now: i64,
        ) -> Decision {
            if let Some(h) = holds.iter().find(|h| h.covers(e)) {
                return Decision::Held(h.id.clone());
            }
            match policy {
                Policy::Keep => Decision::Keep,
                Policy::Archive => Decision::Archive,
                Policy::Review => Decision::Review,
                Policy::ExpireAfterDays(d) => {
                    let due = e
                        .collected_at
                        .saturating_add(i64::from(d).saturating_mul(86_400));
                    if now >= due {
                        Decision::Expired
                    } else {
                        Decision::Keep
                    }
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use serde_json::json;

            fn ev(id: &str, content: Value) -> Evidence {
                Evidence::new(id, "log_extract", content, Some("vendor:7"), 1_000).unwrap()
            }

            #[test]
            fn sha256_known_vectors() {
                assert_eq!(
                    sha256_hex(""),
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                );
                assert_eq!(
                    sha256_hex("abc"),
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                );
                assert_eq!(hex(&[0, 15, 16, 255]), "000f10ff");
            }

            #[test]
            fn canonical_json_ignores_key_order_and_spacing() {
                let a: Value =
                    serde_json::from_str(r#"{"b": 1, "a": {"y": [1, 2], "x": null}}"#).unwrap();
                let b: Value = serde_json::from_str(r#"{"a":{"x":null,"y":[1,2]},"b":1}"#).unwrap();
                assert_eq!(canonical_json(&a), r#"{"a":{"x":null,"y":[1,2]},"b":1}"#);
                assert_eq!(canonical_json(&a), canonical_json(&b));
                // Array order matters.
                assert_ne!(
                    canonical_json(&json!([1, 2])),
                    canonical_json(&json!([2, 1]))
                );
                assert_eq!(canonical_json(&json!("a\"b")), r#""a\"b""#);
            }

            #[test]
            fn same_content_same_hash_different_content_different_hash() {
                let a = ev("E1", json!({"user": "bob", "n": 1}));
                let b = ev("E2", json!({"n": 1, "user": "bob"}));
                let c = ev("E3", json!({"user": "bob", "n": 2}));
                assert_eq!(a.stored_hash(), b.stored_hash());
                assert_ne!(a.stored_hash(), c.stored_hash());
                assert_eq!(a.verify(), Integrity::Valid);
            }

            #[test]
            fn tampering_with_stored_content_is_detected() {
                let a = ev("E1", json!({"amount": 100}));
                let loaded = Evidence::restore(
                    "E1",
                    "log_extract",
                    json!({"amount": 900}),
                    a.stored_hash(),
                    Some("vendor:7"),
                    1_000,
                );
                match loaded.verify() {
                    Integrity::Mismatch { stored, computed } => {
                        assert_eq!(stored, a.stored_hash());
                        assert_ne!(computed, stored);
                    }
                    Integrity::Valid => panic!("tampering not detected"),
                }
                let untouched = Evidence::restore(
                    "E1",
                    "log_extract",
                    json!({"amount": 100}),
                    a.stored_hash(),
                    None,
                    1_000,
                );
                assert_eq!(untouched.verify(), Integrity::Valid);
            }

            #[test]
            fn evidence_needs_id_and_kind() {
                assert_eq!(
                    Evidence::new(" ", "k", json!(1), None, 0),
                    Err(EvidenceError::EmptyField("id"))
                );
                assert_eq!(
                    Evidence::new("i", "", json!(1), None, 0),
                    Err(EvidenceError::EmptyField("kind"))
                );
                assert!(Evidence::new("i", "k", Value::Null, None, 0).is_ok());
            }

            #[test]
            fn bundle_hash_is_independent_of_input_order() {
                let (a, b) = (ev("E1", json!(1)), ev("E2", json!(2)));
                let x = Bundle::new("case-9", "fraud review", &[&a, &b]).unwrap();
                let y = Bundle::new("case-9", "fraud review", &[&b, &a]).unwrap();
                assert_eq!(x.hash(), y.hash());
                assert_eq!(x.ids(), vec!["E1", "E2"]);
                assert_ne!(
                    x.hash(),
                    Bundle::new("case-9", "other purpose", &[&a, &b])
                        .unwrap()
                        .hash()
                );
                assert_ne!(
                    x.hash(),
                    Bundle::new("case-9", "fraud review", &[&a]).unwrap().hash()
                );
            }

            #[test]
            fn bundle_validation() {
                let a = ev("E1", json!(1));
                assert_eq!(Bundle::new("n", "p", &[]), Err(EvidenceError::EmptyBundle));
                assert_eq!(
                    Bundle::new("", "p", &[&a]),
                    Err(EvidenceError::EmptyField("name"))
                );
                assert_eq!(
                    Bundle::new("n", " ", &[&a]),
                    Err(EvidenceError::EmptyField("purpose"))
                );
                let dup = ev("E1", json!(2));
                assert_eq!(
                    Bundle::new("n", "p", &[&a, &dup]),
                    Err(EvidenceError::DuplicateId("E1".into()))
                );
            }

            #[test]
            fn bundle_verify_finds_each_kind_of_problem() {
                let (a, b) = (ev("E1", json!(1)), ev("E2", json!(2)));
                let bundle = Bundle::new("n", "p", &[&a, &b]).unwrap();
                assert!(bundle.verify(&[&a, &b]).is_empty());
                let extra = ev("E9", json!(9));
                assert!(bundle.verify(&[&b, &extra, &a]).is_empty());
                assert_eq!(
                    bundle.verify(&[&a]),
                    vec![BundleProblem::Missing("E2".into())]
                );
                let replaced = ev("E2", json!(3));
                assert_eq!(
                    bundle.verify(&[&a, &replaced]),
                    vec![BundleProblem::Changed("E2".into())]
                );
                let forged =
                    Evidence::restore("E2", "log_extract", json!(3), b.stored_hash(), None, 1_000);
                assert_eq!(
                    bundle.verify(&[&a, &forged]),
                    vec![BundleProblem::ContentAltered("E2".into())]
                );
                let mut altered = bundle.clone();
                altered.purpose = "something else".into();
                assert_eq!(
                    altered.verify(&[&a, &b]),
                    vec![BundleProblem::BundleAltered]
                );
                assert_eq!(bundle.verify(&[]).len(), 2);
            }

            fn hold(id: &str, scope: HoldScope, active: bool) -> LegalHold {
                LegalHold {
                    id: id.into(),
                    scope,
                    active,
                }
            }

            #[test]
            fn holds_respect_their_scope() {
                let e = ev("E1", json!(1));
                let day = 86_400;
                let past = 1_000 + 400 * day;
                let p = Policy::ExpireAfterDays(365);
                assert_eq!(evaluate_retention(&e, p, &[], past), Decision::Expired);
                assert_eq!(
                    evaluate_retention(&e, p, &[hold("H1", HoldScope::All, true)], past),
                    Decision::Held("H1".into())
                );
                assert_eq!(
                    evaluate_retention(
                        &e,
                        p,
                        &[hold("H2", HoldScope::Entity("vendor:7".into()), true)],
                        past
                    ),
                    Decision::Held("H2".into())
                );
                assert_eq!(
                    evaluate_retention(
                        &e,
                        p,
                        &[hold("H3", HoldScope::Evidence("E1".into()), true)],
                        past
                    ),
                    Decision::Held("H3".into())
                );
                // A hold for something else does NOT protect this item (the prototype protected everything).
                assert_eq!(
                    evaluate_retention(
                        &e,
                        p,
                        &[hold("H4", HoldScope::Entity("vendor:8".into()), true)],
                        past
                    ),
                    Decision::Expired
                );
                assert_eq!(
                    evaluate_retention(
                        &e,
                        p,
                        &[hold("H5", HoldScope::Evidence("E2".into()), true)],
                        past
                    ),
                    Decision::Expired
                );
                // A released hold protects nothing.
                assert_eq!(
                    evaluate_retention(&e, p, &[hold("H6", HoldScope::All, false)], past),
                    Decision::Expired
                );
                // The first covering hold is named.
                let holds = [
                    hold("A", HoldScope::Entity("x".into()), true),
                    hold("B", HoldScope::All, true),
                    hold("C", HoldScope::All, true),
                ];
                assert_eq!(
                    evaluate_retention(&e, p, &holds, past),
                    Decision::Held("B".into())
                );
            }

            #[test]
            fn expiry_boundary_and_other_policies() {
                let e = ev("E1", json!(1));
                let due = 1_000 + 30 * 86_400;
                let p = Policy::ExpireAfterDays(30);
                assert_eq!(evaluate_retention(&e, p, &[], due - 1), Decision::Keep);
                assert_eq!(evaluate_retention(&e, p, &[], due), Decision::Expired);
                assert_eq!(
                    evaluate_retention(&e, Policy::Keep, &[], i64::MAX),
                    Decision::Keep
                );
                assert_eq!(
                    evaluate_retention(&e, Policy::Archive, &[], 0),
                    Decision::Archive
                );
                assert_eq!(
                    evaluate_retention(&e, Policy::Review, &[], 0),
                    Decision::Review
                );
                // Held beats every policy, including Archive.
                assert!(matches!(
                    evaluate_retention(&e, Policy::Archive, &[hold("H", HoldScope::All, true)], 0),
                    Decision::Held(_)
                ));
                // Overflow-safe.
                let late = Evidence::new("L", "k", json!(1), None, i64::MAX).unwrap();
                assert_eq!(
                    evaluate_retention(&late, Policy::ExpireAfterDays(u32::MAX), &[], i64::MAX),
                    Decision::Expired
                );
            }
        }
    }
    pub mod rules {
        //! Boolean rule trees evaluated against a JSON event.
        //!
        //! A rule is a JSON object. A leaf is `{"field": "payload.amount", "operator": "gt", "value": 100}`;
        //! `{"operator": "and"|"or", "conditions": [...]}` and `{"operator": "not", "condition": {...}}`
        //! combine rules. Fields are dot paths; numeric segments index arrays.

        use serde_json::Value;
        use std::fmt;

        /// Deepest nesting of and / or / not accepted (guards against hostile rules).
        pub const MAX_DEPTH: usize = 32;

        /// Why a rule could not be evaluated.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum RuleError {
            /// The rule (or a part of it) is not a JSON object.
            NotAnObject,
            /// The `operator` is missing or unknown.
            UnknownOperator(String),
            /// A leaf has no `field`.
            MissingField,
            /// An `and` / `or` has no `conditions` array, or `not` has no `condition`.
            MissingConditions,
            /// The rule nests deeper than [`MAX_DEPTH`].
            TooDeep,
        }

        impl fmt::Display for RuleError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    RuleError::NotAnObject => write!(f, "rule must be a JSON object"),
                    RuleError::UnknownOperator(o) => write!(f, "unknown operator '{o}'"),
                    RuleError::MissingField => write!(f, "leaf rule needs a 'field'"),
                    RuleError::MissingConditions => {
                        write!(f, "combinator rule needs its conditions")
                    }
                    RuleError::TooDeep => write!(f, "rule nests deeper than {MAX_DEPTH} levels"),
                }
            }
        }

        impl std::error::Error for RuleError {}

        /// The result of evaluating a rule.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Outcome {
            /// Whether the rule matched.
            pub matched: bool,
            /// Human-readable reason, naming the values that decided it.
            pub explanation: String,
            /// Event fields that took part (in first-use order, without repeats).
            pub fields: Vec<String>,
        }

        /// Looks a dot path up in a JSON value; numeric segments index arrays.
        pub fn resolve<'a>(event: &'a Value, path: &str) -> Option<&'a Value> {
            let mut cur = event;
            for part in path.split('.') {
                cur = match cur {
                    Value::Object(m) => m.get(part)?,
                    Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
                    _ => return None,
                };
            }
            Some(cur)
        }

        fn push_unique(into: &mut Vec<String>, from: Vec<String>) {
            for f in from {
                if !into.contains(&f) {
                    into.push(f);
                }
            }
        }

        /// Evaluates `rule` against `event`.
        pub fn evaluate(rule: &Value, event: &Value) -> Result<Outcome, RuleError> {
            eval(rule, event, 0)
        }

        fn eval(rule: &Value, event: &Value, depth: usize) -> Result<Outcome, RuleError> {
            if depth > MAX_DEPTH {
                return Err(RuleError::TooDeep);
            }
            let obj = rule.as_object().ok_or(RuleError::NotAnObject)?;
            let op = obj
                .get("operator")
                .and_then(Value::as_str)
                .ok_or_else(|| RuleError::UnknownOperator(String::new()))?;
            match op {
                "and" | "or" => {
                    let subs = obj
                        .get("conditions")
                        .and_then(Value::as_array)
                        .ok_or(RuleError::MissingConditions)?;
                    let mut fields = Vec::new();
                    let mut results = Vec::new();
                    for s in subs {
                        let o = eval(s, event, depth + 1)?;
                        push_unique(&mut fields, o.fields);
                        results.push(o.matched);
                    }
                    let matched = if op == "and" {
                        results.iter().all(|r| *r)
                    } else {
                        results.iter().any(|r| *r)
                    };
                    let hits = results.iter().filter(|r| **r).count();
                    Ok(Outcome {
                        matched,
                        explanation: format!(
                            "{} of {} conditions matched ({op})",
                            hits,
                            results.len()
                        ),
                        fields,
                    })
                }
                "not" => {
                    let sub = obj.get("condition").ok_or(RuleError::MissingConditions)?;
                    let o = eval(sub, event, depth + 1)?;
                    Ok(Outcome {
                        matched: !o.matched,
                        explanation: format!("not ({})", o.explanation),
                        fields: o.fields,
                    })
                }
                _ => leaf(op, obj, event),
            }
        }

        /// The comparison operators a leaf rule may use.
        const LEAF_OPS: [&str; 13] = [
            "eq",
            "neq",
            "gt",
            "gte",
            "lt",
            "lte",
            "in",
            "not_in",
            "contains",
            "not_contains",
            "exists",
            "not_exists",
            "between",
        ];

        fn leaf(
            op: &str,
            obj: &serde_json::Map<String, Value>,
            event: &Value,
        ) -> Result<Outcome, RuleError> {
            if !LEAF_OPS.contains(&op) {
                return Err(RuleError::UnknownOperator(op.to_string()));
            }
            let field = obj
                .get("field")
                .and_then(Value::as_str)
                .ok_or(RuleError::MissingField)?;
            let target = obj.get("value").unwrap_or(&Value::Null);
            let actual = resolve(event, field);
            let matched = compare(op, actual, target)?;
            let shown = actual.map_or("<missing>".to_string(), |v| v.to_string());
            Ok(Outcome {
                matched,
                explanation: format!("{field} {op} {target}: actual {shown}"),
                fields: vec![field.to_string()],
            })
        }

        fn num(v: &Value) -> Option<f64> {
            v.as_f64().filter(|n| n.is_finite())
        }

        fn ordering(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
            match (num(a), num(b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y),
                _ => match (a.as_str(), b.as_str()) {
                    (Some(x), Some(y)) => Some(x.cmp(y)),
                    _ => None,
                },
            }
        }

        fn same(a: &Value, b: &Value) -> bool {
            match (num(a), num(b)) {
                (Some(x), Some(y)) => x == y,
                _ => a == b,
            }
        }

        fn contains(haystack: &Value, needle: &Value) -> bool {
            match haystack {
                Value::String(s) => needle.as_str().is_some_and(|n| s.contains(n)),
                Value::Array(a) => a.iter().any(|x| same(x, needle)),
                Value::Object(m) => needle.as_str().is_some_and(|k| m.contains_key(k)),
                _ => false,
            }
        }

        fn compare(op: &str, actual: Option<&Value>, target: &Value) -> Result<bool, RuleError> {
            use std::cmp::Ordering::{Equal, Greater, Less};
            let exists = actual.is_some_and(|v| !v.is_null());
            Ok(match op {
                "exists" => exists,
                "not_exists" => !exists,
                // Every other operator needs a present value; a missing one never matches, except the
                // negative forms, which hold because the value is not there.
                "neq" => actual.is_none_or(|a| !same(a, target)),
                "not_in" => actual.is_none_or(|a| {
                    !target
                        .as_array()
                        .is_some_and(|t| t.iter().any(|x| same(x, a)))
                }),
                "not_contains" => actual.is_none_or(|a| !contains(a, target)),
                "eq" | "gt" | "gte" | "lt" | "lte" | "in" | "contains" | "between" => {
                    let Some(a) = actual else { return Ok(false) };
                    match op {
                        "eq" => same(a, target),
                        "gt" => ordering(a, target) == Some(Greater),
                        "gte" => matches!(ordering(a, target), Some(Greater | Equal)),
                        "lt" => ordering(a, target) == Some(Less),
                        "lte" => matches!(ordering(a, target), Some(Less | Equal)),
                        "in" => target
                            .as_array()
                            .is_some_and(|t| t.iter().any(|x| same(x, a))),
                        "contains" => contains(a, target),
                        _ => match target.as_array().map(Vec::as_slice) {
                            Some([lo, hi]) => {
                                matches!(ordering(a, lo), Some(Greater | Equal))
                                    && matches!(ordering(a, hi), Some(Less | Equal))
                            }
                            _ => false,
                        },
                    }
                }
                other => return Err(RuleError::UnknownOperator(other.to_string())),
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use serde_json::json;

            fn ev() -> Value {
                json!({"type": "PAYMENT", "payload": {"amount": 250, "currency": "USD", "tags": ["wire", "new"]},
                       "lines": [{"sku": "A"}, {"sku": "B"}], "note": null})
            }

            fn leaf(op: &str, field: &str, value: Value) -> Value {
                json!({"field": field, "operator": op, "value": value})
            }

            fn hit(rule: Value) -> bool {
                evaluate(&rule, &ev()).unwrap().matched
            }

            #[test]
            fn numeric_comparisons_use_values_not_text() {
                assert!(hit(leaf("gt", "payload.amount", json!(100))));
                assert!(!hit(leaf("gt", "payload.amount", json!(250))));
                assert!(hit(leaf("gte", "payload.amount", json!(250))));
                assert!(hit(leaf("lt", "payload.amount", json!(251))));
                assert!(hit(leaf("lte", "payload.amount", json!(250))));
                assert!(hit(leaf("eq", "payload.amount", json!(250.0))));
                // 9 < 10 numerically, although "9" > "10" as text.
                assert!(hit(leaf("lt", "payload.amount", json!(1000))));
            }

            #[test]
            fn mismatched_types_never_order() {
                assert!(!hit(leaf("gt", "payload.amount", json!("100"))));
                assert!(!hit(leaf("lt", "payload.currency", json!(5))));
            }

            #[test]
            fn membership_and_containment() {
                assert!(hit(leaf("in", "payload.currency", json!(["USD", "EUR"]))));
                assert!(hit(leaf("not_in", "payload.currency", json!(["GBP"]))));
                assert!(!hit(leaf("in", "payload.currency", json!("USD"))));
                assert!(hit(leaf("contains", "payload.tags", json!("wire"))));
                assert!(hit(leaf("contains", "type", json!("PAY"))));
                assert!(hit(leaf("not_contains", "payload.tags", json!("cash"))));
                assert!(hit(leaf("contains", "payload", json!("amount"))));
            }

            #[test]
            fn between_is_inclusive_and_needs_two_bounds() {
                assert!(hit(leaf("between", "payload.amount", json!([250, 300]))));
                assert!(hit(leaf("between", "payload.amount", json!([200, 250]))));
                assert!(!hit(leaf("between", "payload.amount", json!([251, 300]))));
                assert!(!hit(leaf("between", "payload.amount", json!([1, 2, 3]))));
                assert!(!hit(leaf("between", "payload.amount", json!(5))));
            }

            #[test]
            fn missing_and_null_fields() {
                assert!(!hit(leaf("eq", "nope.here", json!(1))));
                assert!(hit(leaf("neq", "nope.here", json!(1))));
                assert!(hit(leaf("not_exists", "nope", json!(null))));
                assert!(hit(leaf("not_exists", "note", json!(null))));
                assert!(hit(leaf("exists", "payload.amount", json!(null))));
                assert!(hit(leaf("not_in", "nope", json!(["a"]))));
            }

            #[test]
            fn array_indexing_in_paths() {
                assert!(hit(leaf("eq", "lines.1.sku", json!("B"))));
                assert!(!hit(leaf("eq", "lines.2.sku", json!("B"))));
                assert!(!hit(leaf("eq", "lines.x.sku", json!("B"))));
            }

            #[test]
            fn combinators_and_explanations() {
                let rule = json!({"operator": "and", "conditions": [
                    leaf("gt", "payload.amount", json!(100)),
                    {"operator": "or", "conditions": [leaf("eq", "type", json!("REFUND")), leaf("eq", "payload.currency", json!("USD"))]},
                    {"operator": "not", "condition": leaf("exists", "payload.blocked", json!(null))}]});
                let o = evaluate(&rule, &ev()).unwrap();
                assert!(o.matched);
                assert_eq!(
                    o.fields,
                    vec![
                        "payload.amount",
                        "type",
                        "payload.currency",
                        "payload.blocked"
                    ]
                );
                assert!(o.explanation.contains("3 of 3"));
                let failing = json!({"operator": "and", "conditions": [leaf("gt", "payload.amount", json!(999))]});
                let o = evaluate(&failing, &ev()).unwrap();
                assert!(!o.matched && o.explanation.contains("0 of 1"));
                assert!(evaluate(&leaf("gt", "payload.amount", json!(5)), &ev())
                    .unwrap()
                    .explanation
                    .contains("actual 250"));
            }

            #[test]
            fn empty_combinators_follow_logic() {
                assert!(hit(json!({"operator": "and", "conditions": []})));
                assert!(!hit(json!({"operator": "or", "conditions": []})));
            }

            #[test]
            fn malformed_rules_are_errors_not_matches() {
                assert_eq!(evaluate(&json!(5), &ev()), Err(RuleError::NotAnObject));
                assert_eq!(
                    evaluate(&json!({"field": "a"}), &ev()),
                    Err(RuleError::UnknownOperator(String::new()))
                );
                assert_eq!(
                    evaluate(&json!({"operator": "xor"}), &ev()),
                    Err(RuleError::UnknownOperator("xor".into()))
                );
                assert_eq!(
                    evaluate(&json!({"operator": "eq"}), &ev()),
                    Err(RuleError::MissingField)
                );
                assert_eq!(
                    evaluate(&json!({"operator": "and"}), &ev()),
                    Err(RuleError::MissingConditions)
                );
                assert_eq!(
                    evaluate(&json!({"operator": "not"}), &ev()),
                    Err(RuleError::MissingConditions)
                );
                // An error inside a combinator is not swallowed.
                let bad = json!({"operator": "or", "conditions": [leaf("eq", "type", json!("PAYMENT")), {"operator": "zz"}]});
                assert!(evaluate(&bad, &ev()).is_err());
            }

            #[test]
            fn hostile_depth_is_refused() {
                let mut rule = leaf("exists", "type", json!(null));
                for _ in 0..(MAX_DEPTH + 5) {
                    rule = json!({"operator": "not", "condition": rule});
                }
                assert_eq!(evaluate(&rule, &ev()), Err(RuleError::TooDeep));
                let mut ok = leaf("exists", "type", json!(null));
                for _ in 0..MAX_DEPTH {
                    ok = json!({"operator": "not", "condition": ok});
                }
                assert!(evaluate(&ok, &ev()).is_ok());
            }
        }
    }
    pub mod scoring {
        //! Risk scoring: inherent score from the factors, residual score after controls, and a level.

        use std::fmt;

        /// Highest value of each 1..=5 scale.
        const SCALE_MAX: f64 = 5.0;
        /// Most the residual can be reduced by controls (a control can never make risk vanish).
        const MAX_CONTROL_EFFECT: f64 = 0.9;
        /// Points added at full compliance exposure.
        const COMPLIANCE_PENALTY: f64 = 10.0;

        /// Relative importance of each factor.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Weights {
            /// Weight of likelihood.
            pub likelihood: f64,
            /// Weight of impact.
            pub impact: f64,
            /// Weight of velocity (how fast the risk is building).
            pub velocity: f64,
            /// Weight of exposure (how much is at stake).
            pub exposure: f64,
        }

        impl Default for Weights {
            fn default() -> Self {
                Self {
                    likelihood: 0.30,
                    impact: 0.30,
                    velocity: 0.15,
                    exposure: 0.10,
                }
            }
        }

        /// Factor ratings on a 0..=5 scale (likelihood and impact are 1..=5).
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Factors {
            /// Chance the risk occurs, 1 (rare) to 5 (almost certain).
            pub likelihood: u8,
            /// Damage if it occurs, 1 (minor) to 5 (severe).
            pub impact: u8,
            /// How fast it is building, 0 (not at all) to 5.
            pub velocity: u8,
            /// How much is exposed, 0 to 5.
            pub exposure: u8,
        }

        /// Why scoring refused its input.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum ScoreError {
            /// A rating is outside its scale.
            OutOfRange(&'static str),
            /// Weights are negative, non-finite or all zero.
            BadWeights,
            /// Level thresholds are not strictly increasing within 0..=100.
            BadAppetite,
        }

        impl fmt::Display for ScoreError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    ScoreError::OutOfRange(n) => write!(f, "{n} is outside its scale"),
                    ScoreError::BadWeights => {
                        write!(f, "weights must be finite, non-negative and not all zero")
                    }
                    ScoreError::BadAppetite => {
                        write!(f, "level thresholds must increase strictly within 0..=100")
                    }
                }
            }
        }

        impl std::error::Error for ScoreError {}

        /// A risk level.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Level {
            /// Below the medium threshold.
            Low,
            /// At or above medium.
            Medium,
            /// At or above high.
            High,
            /// At or above critical.
            Critical,
        }

        /// Score thresholds that define the organisation's risk appetite.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Appetite {
            /// Score from which a risk is Medium.
            pub medium: f64,
            /// Score from which a risk is High.
            pub high: f64,
            /// Score from which a risk is Critical.
            pub critical: f64,
        }

        impl Default for Appetite {
            fn default() -> Self {
                Self {
                    medium: 40.0,
                    high: 70.0,
                    critical: 85.0,
                }
            }
        }

        impl Appetite {
            /// Checks the thresholds are strictly increasing and within 0..=100.
            pub fn validate(&self) -> Result<(), ScoreError> {
                let ok = self.medium.is_finite()
                    && self.high.is_finite()
                    && self.critical.is_finite()
                    && self.medium >= 0.0
                    && self.medium < self.high
                    && self.high < self.critical
                    && self.critical <= 100.0;
                if ok {
                    Ok(())
                } else {
                    Err(ScoreError::BadAppetite)
                }
            }

            /// The level of `score` under these thresholds.
            pub fn level(&self, score: f64) -> Level {
                if score >= self.critical {
                    Level::Critical
                } else if score >= self.high {
                    Level::High
                } else if score >= self.medium {
                    Level::Medium
                } else {
                    Level::Low
                }
            }
        }

        fn round2(x: f64) -> f64 {
            (x * 100.0).round() / 100.0
        }

        /// The weighted average of the factors, 0..=100. Each factor is first scaled to 0..=1, so a
        /// maximal rating on every factor scores exactly 100.
        pub fn inherent(f: Factors, w: Weights) -> Result<f64, ScoreError> {
            if !(1..=5).contains(&f.likelihood) {
                return Err(ScoreError::OutOfRange("likelihood"));
            }
            if !(1..=5).contains(&f.impact) {
                return Err(ScoreError::OutOfRange("impact"));
            }
            if f.velocity > 5 {
                return Err(ScoreError::OutOfRange("velocity"));
            }
            if f.exposure > 5 {
                return Err(ScoreError::OutOfRange("exposure"));
            }
            let ws = [w.likelihood, w.impact, w.velocity, w.exposure];
            let total: f64 = ws.iter().sum();
            if ws.iter().any(|x| !x.is_finite() || *x < 0.0) || total <= 0.0 {
                return Err(ScoreError::BadWeights);
            }
            let weighted = w.likelihood * f64::from(f.likelihood)
                + w.impact * f64::from(f.impact)
                + w.velocity * f64::from(f.velocity)
                + w.exposure * f64::from(f.exposure);
            Ok(round2(
                (weighted / (total * SCALE_MAX) * 100.0).clamp(0.0, 100.0),
            ))
        }

        /// The score left after controls. `control_effectiveness` and `compliance_exposure` are
        /// fractions in 0..=1 (values outside are clamped). Controls cut at most 90% of the inherent
        /// score; compliance exposure adds up to 10 points.
        pub fn residual(
            inherent: f64,
            control_effectiveness: f64,
            compliance_exposure: f64,
        ) -> f64 {
            if !inherent.is_finite() {
                return 0.0;
            }
            let ce = if control_effectiveness.is_finite() {
                control_effectiveness.clamp(0.0, MAX_CONTROL_EFFECT)
            } else {
                0.0
            };
            let co = if compliance_exposure.is_finite() {
                compliance_exposure.clamp(0.0, 1.0)
            } else {
                0.0
            };
            round2(
                (inherent.clamp(0.0, 100.0) * (1.0 - ce) + co * COMPLIANCE_PENALTY)
                    .clamp(0.0, 100.0),
            )
        }

        /// A full assessment.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Assessment {
            /// Score before controls.
            pub inherent: f64,
            /// Score after controls and compliance exposure.
            pub residual: f64,
            /// Level of the residual score.
            pub level: Level,
            /// Human-readable summary.
            pub explanation: String,
        }

        /// Scores a risk end to end.
        pub fn assess(
            f: Factors,
            w: Weights,
            control_effectiveness: f64,
            compliance_exposure: f64,
            appetite: Appetite,
        ) -> Result<Assessment, ScoreError> {
            appetite.validate()?;
            let inh = inherent(f, w)?;
            let res = residual(inh, control_effectiveness, compliance_exposure);
            let level = appetite.level(res);
            Ok(Assessment {
                inherent: inh,
                residual: res,
                level,
                explanation: format!(
                    "inherent {inh}, residual {res} (controls {control_effectiveness}, compliance {compliance_exposure}) => {level:?}"
                ),
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn f(l: u8, i: u8, v: u8, e: u8) -> Factors {
                Factors {
                    likelihood: l,
                    impact: i,
                    velocity: v,
                    exposure: e,
                }
            }

            #[test]
            fn maximal_and_minimal_ratings_hit_the_ends() {
                assert_eq!(inherent(f(5, 5, 5, 5), Weights::default()).unwrap(), 100.0);
                // Minimum allowed: likelihood 1, impact 1, others 0: (0.3 + 0.3) / (0.85 * 5) * 100 = 14.12.
                assert_eq!(inherent(f(1, 1, 0, 0), Weights::default()).unwrap(), 14.12);
            }

            #[test]
            fn exact_mid_value() {
                // (0.3*3 + 0.3*4 + 0.15*2 + 0.1*1) / 4.25 * 100 = 2.5 / 4.25 * 100 = 58.82.
                assert_eq!(inherent(f(3, 4, 2, 1), Weights::default()).unwrap(), 58.82);
            }

            #[test]
            fn raising_any_factor_never_lowers_the_score() {
                let base = inherent(f(2, 2, 2, 2), Weights::default()).unwrap();
                for g in [f(3, 2, 2, 2), f(2, 3, 2, 2), f(2, 2, 3, 2), f(2, 2, 2, 3)] {
                    assert!(inherent(g, Weights::default()).unwrap() > base);
                }
            }

            #[test]
            fn out_of_range_ratings_are_refused() {
                assert_eq!(
                    inherent(f(0, 3, 0, 0), Weights::default()),
                    Err(ScoreError::OutOfRange("likelihood"))
                );
                assert_eq!(
                    inherent(f(6, 3, 0, 0), Weights::default()),
                    Err(ScoreError::OutOfRange("likelihood"))
                );
                assert_eq!(
                    inherent(f(3, 0, 0, 0), Weights::default()),
                    Err(ScoreError::OutOfRange("impact"))
                );
                assert_eq!(
                    inherent(f(3, 3, 6, 0), Weights::default()),
                    Err(ScoreError::OutOfRange("velocity"))
                );
                assert_eq!(
                    inherent(f(3, 3, 0, 6), Weights::default()),
                    Err(ScoreError::OutOfRange("exposure"))
                );
            }

            #[test]
            fn bad_weights_are_refused() {
                let zero = Weights {
                    likelihood: 0.0,
                    impact: 0.0,
                    velocity: 0.0,
                    exposure: 0.0,
                };
                let neg = Weights {
                    likelihood: -1.0,
                    ..Weights::default()
                };
                let nan = Weights {
                    impact: f64::NAN,
                    ..Weights::default()
                };
                for w in [zero, neg, nan] {
                    assert_eq!(inherent(f(3, 3, 3, 3), w), Err(ScoreError::BadWeights));
                }
            }

            #[test]
            fn residual_formula_and_clamps() {
                assert_eq!(residual(80.0, 0.5, 0.0), 40.0);
                assert_eq!(residual(80.0, 0.5, 0.5), 45.0);
                // Controls cannot remove more than 90%.
                assert_eq!(residual(100.0, 1.0, 0.0), 10.0);
                assert_eq!(residual(100.0, 5.0, 0.0), 10.0);
                assert_eq!(residual(100.0, 0.0, 1.0), 100.0);
                assert_eq!(residual(50.0, -3.0, -3.0), 50.0);
                assert_eq!(residual(f64::NAN, 0.5, 0.5), 0.0);
                assert_eq!(residual(50.0, f64::NAN, f64::NAN), 50.0);
            }

            #[test]
            fn levels_at_the_boundaries() {
                let a = Appetite::default();
                assert_eq!(a.level(39.99), Level::Low);
                assert_eq!(a.level(40.0), Level::Medium);
                assert_eq!(a.level(69.99), Level::Medium);
                assert_eq!(a.level(70.0), Level::High);
                assert_eq!(a.level(85.0), Level::Critical);
                assert_eq!(a.level(100.0), Level::Critical);
                assert!(
                    Level::Critical > Level::High
                        && Level::High > Level::Medium
                        && Level::Medium > Level::Low
                );
            }

            #[test]
            fn custom_appetite_changes_levels_and_is_validated() {
                let cautious = Appetite {
                    medium: 20.0,
                    high: 40.0,
                    critical: 60.0,
                };
                assert_eq!(cautious.level(45.0), Level::High);
                assert!(cautious.validate().is_ok());
                for bad in [
                    Appetite {
                        medium: 50.0,
                        high: 40.0,
                        critical: 60.0,
                    },
                    Appetite {
                        medium: 10.0,
                        high: 10.0,
                        critical: 60.0,
                    },
                    Appetite {
                        medium: -1.0,
                        high: 40.0,
                        critical: 60.0,
                    },
                    Appetite {
                        medium: 10.0,
                        high: 40.0,
                        critical: 101.0,
                    },
                    Appetite {
                        medium: f64::NAN,
                        high: 40.0,
                        critical: 60.0,
                    },
                ] {
                    assert_eq!(bad.validate(), Err(ScoreError::BadAppetite));
                    assert!(assess(f(3, 3, 3, 3), Weights::default(), 0.0, 0.0, bad).is_err());
                }
            }

            #[test]
            fn assessment_end_to_end() {
                let a = assess(
                    f(5, 5, 5, 5),
                    Weights::default(),
                    0.8,
                    0.0,
                    Appetite::default(),
                )
                .unwrap();
                assert_eq!((a.inherent, a.residual, a.level), (100.0, 20.0, Level::Low));
                let b = assess(
                    f(5, 5, 5, 5),
                    Weights::default(),
                    0.0,
                    0.0,
                    Appetite::default(),
                )
                .unwrap();
                assert_eq!(b.level, Level::Critical);
                assert!(
                    b.explanation.contains("Critical") && b.explanation.contains("inherent 100")
                );
            }
        }
    }
}

#[cfg(feature = "comms")]
#[allow(clippy::module_inception)]
pub mod comms {
    //! MOULI native communication.
    //!
    //! * [`crypto`]: per-pair session keys (X25519 + HKDF-SHA256) and
    //!   authenticated encryption (ChaCha20-Poly1305) with replay protection.
    //! * [`conversation`]: message ordering (Lamport clocks), de-duplication
    //!   and delivery receipts.
    //! * [`call`]: call signalling state machine for voice/video (media itself
    //!   goes over the transport the call negotiates).
    //!
    //! This is a first version: one static key pair per device, no forward
    //! secrecy ratchet yet. Do not treat it as audited cryptography.
    #![forbid(unsafe_code)]

    pub mod call {
        //! Call signalling state machine (voice and video).
        //!
        //! Signals carry session descriptions and network candidates as opaque
        //! strings; the media engine interprets them. If both sides call each other
        //! at once ("glare"), the side with the lower peer id keeps its offer.

        /// Peer id.
        pub type PeerId = u32;

        /// Media wanted.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Media {
            /// Audio on.
            pub audio: bool,
            /// Video on.
            pub video: bool,
        }

        /// Signalling message.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Signal {
            /// Start a call with a session offer.
            Offer {
                /// Call id.
                call: u64,
                /// Media requested.
                media: Media,
                /// Session description.
                sdp: String,
            },
            /// Accept with a session answer.
            Answer {
                /// Call id.
                call: u64,
                /// Session description.
                sdp: String,
            },
            /// Extra network candidate.
            Candidate {
                /// Call id.
                call: u64,
                /// Candidate line.
                candidate: String,
            },
            /// Refuse or end.
            Hangup {
                /// Call id.
                call: u64,
                /// Reason.
                reason: String,
            },
        }

        /// Call state.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum State {
            /// No call.
            Idle,
            /// We offered, waiting for an answer since `since`.
            Calling {
                /// Call id.
                call: u64,
                /// Start time.
                since: u64,
            },
            /// Peer offered; waiting for the user since `since`.
            Ringing {
                /// Call id.
                call: u64,
                /// Offer description.
                sdp: String,
                /// Requested media.
                media: Media,
                /// Start time.
                since: u64,
            },
            /// Media flowing.
            Connected {
                /// Call id.
                call: u64,
            },
            /// Finished with a reason.
            Ended {
                /// Reason.
                reason: String,
            },
        }

        /// One side of a call with one peer.
        #[derive(Debug, Clone)]
        pub struct CallSession {
            me: PeerId,
            peer: PeerId,
            state: State,
            /// Candidates received for the current call.
            pub candidates: Vec<String>,
            /// Seconds before an unanswered call ends.
            pub ring_timeout: u64,
        }

        impl CallSession {
            /// Idle session between `me` and `peer`.
            #[must_use]
            pub fn new(me: PeerId, peer: PeerId) -> Self {
                Self {
                    me,
                    peer,
                    state: State::Idle,
                    candidates: Vec::new(),
                    ring_timeout: 45,
                }
            }

            /// Current state.
            #[must_use]
            pub fn state(&self) -> &State {
                &self.state
            }

            fn idle(&self) -> bool {
                matches!(self.state, State::Idle | State::Ended { .. })
            }

            fn current(&self) -> Option<u64> {
                match self.state {
                    State::Calling { call, .. }
                    | State::Ringing { call, .. }
                    | State::Connected { call } => Some(call),
                    _ => None,
                }
            }

            /// Place a call; returns the offer to send.
            pub fn dial(&mut self, call: u64, media: Media, sdp: &str, now: u64) -> Option<Signal> {
                if !self.idle() {
                    return None;
                }
                self.candidates.clear();
                self.state = State::Calling { call, since: now };
                Some(Signal::Offer {
                    call,
                    media,
                    sdp: sdp.to_owned(),
                })
            }

            /// User accepts a ringing call; returns the answer to send.
            pub fn accept(&mut self, sdp: &str) -> Option<Signal> {
                let State::Ringing { call, .. } = self.state else {
                    return None;
                };
                self.state = State::Connected { call };
                Some(Signal::Answer {
                    call,
                    sdp: sdp.to_owned(),
                })
            }

            /// Hang up or decline; returns the signal to send.
            pub fn hangup(&mut self, reason: &str) -> Option<Signal> {
                let call = self.current()?;
                self.state = State::Ended {
                    reason: reason.to_owned(),
                };
                Some(Signal::Hangup {
                    call,
                    reason: reason.to_owned(),
                })
            }

            /// Handle a signal from the peer. May return a reply to send.
            pub fn on_signal(&mut self, s: Signal, now: u64) -> Option<Signal> {
                match s {
                    Signal::Offer { call, media, sdp } => match self.state {
                        State::Idle | State::Ended { .. } => {
                            self.candidates.clear();
                            self.state = State::Ringing {
                                call,
                                sdp,
                                media,
                                since: now,
                            };
                            None
                        }
                        // Glare: lower id keeps its own offer; higher id yields.
                        State::Calling { .. } if self.me > self.peer => {
                            self.candidates.clear();
                            self.state = State::Ringing {
                                call,
                                sdp,
                                media,
                                since: now,
                            };
                            None
                        }
                        _ => Some(Signal::Hangup {
                            call,
                            reason: "busy".into(),
                        }),
                    },
                    Signal::Answer { call, .. } => {
                        if matches!(self.state, State::Calling { call: c, .. } if c == call) {
                            self.state = State::Connected { call };
                        }
                        None
                    }
                    Signal::Candidate { call, candidate } => {
                        if self.current() == Some(call) {
                            self.candidates.push(candidate);
                        }
                        None
                    }
                    Signal::Hangup { call, reason } => {
                        if self.current() == Some(call) {
                            self.state = State::Ended { reason };
                        }
                        None
                    }
                }
            }

            /// End unanswered calls older than `ring_timeout`; returns the hangup to send.
            pub fn tick(&mut self, now: u64) -> Option<Signal> {
                let since = match self.state {
                    State::Calling { since, .. } | State::Ringing { since, .. } => since,
                    _ => return None,
                };
                if now.saturating_sub(since) >= self.ring_timeout {
                    self.hangup("no answer")
                } else {
                    None
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            const AV: Media = Media {
                audio: true,
                video: true,
            };

            #[test]
            fn call_flow() {
                let mut a = CallSession::new(1, 2);
                let mut b = CallSession::new(2, 1);
                let offer = a.dial(7, AV, "sdp-a", 0).unwrap();
                assert!(b.on_signal(offer, 0).is_none());
                assert!(matches!(b.state(), State::Ringing { call: 7, .. }));
                b.on_signal(
                    Signal::Candidate {
                        call: 7,
                        candidate: "c1".into(),
                    },
                    1,
                );
                let answer = b.accept("sdp-b").unwrap();
                a.on_signal(answer, 2);
                assert_eq!(a.state(), &State::Connected { call: 7 });
                assert_eq!(b.candidates, vec!["c1".to_string()]);
                let bye = a.hangup("done").unwrap();
                b.on_signal(bye, 3);
                assert!(matches!(b.state(), State::Ended { .. }));
            }

            #[test]
            fn glare_busy_and_timeout() {
                let mut a = CallSession::new(1, 2);
                let mut b = CallSession::new(2, 1);
                let oa = a.dial(10, AV, "a", 0).unwrap();
                let ob = b.dial(20, AV, "b", 0).unwrap();
                // a (lower id) keeps its call and rejects b's offer; b yields to a's.
                assert!(matches!(
                    a.on_signal(ob, 0),
                    Some(Signal::Hangup { call: 20, .. })
                ));
                assert!(b.on_signal(oa, 0).is_none());
                assert!(matches!(b.state(), State::Ringing { call: 10, .. }));

                let mut c = CallSession::new(3, 4);
                c.dial(1, AV, "x", 100).unwrap();
                assert!(c.tick(120).is_none());
                assert!(matches!(c.tick(145), Some(Signal::Hangup { .. })));
                assert!(
                    c.dial(2, AV, "y", 200).is_some(),
                    "can call again after end"
                );
            }
        }
    }
    pub mod conversation {
        //! Ordered, de-duplicated conversations with delivery receipts.
        //!
        //! Each device keeps a Lamport clock. Messages sort by (clock, sender,
        //! id), so every device shows the same order regardless of arrival order.

        use std::collections::BTreeMap;

        /// Device or user id.
        pub type PeerId = u32;

        /// Delivery state, as seen by the sender.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Delivery {
            /// Queued locally.
            Pending,
            /// Accepted by the transport.
            Sent,
            /// Stored on the recipient's device.
            Delivered,
            /// Shown to the recipient.
            Read,
        }

        /// A message.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Message {
            /// Globally unique id (sender, per-sender counter).
            pub id: (PeerId, u64),
            /// Lamport timestamp.
            pub clock: u64,
            /// Author.
            pub sender: PeerId,
            /// Text body.
            pub body: String,
            /// Delivery state (meaningful for own messages).
            pub delivery: Delivery,
        }

        /// One conversation on one device.
        #[derive(Debug, Clone)]
        pub struct Conversation {
            me: PeerId,
            clock: u64,
            next_local: u64,
            messages: BTreeMap<(u64, PeerId, u64), Message>,
            index: BTreeMap<(PeerId, u64), (u64, PeerId, u64)>,
        }

        impl Conversation {
            /// Conversation as seen by `me`.
            #[must_use]
            pub fn new(me: PeerId) -> Self {
                Self {
                    me,
                    clock: 0,
                    next_local: 0,
                    messages: BTreeMap::new(),
                    index: BTreeMap::new(),
                }
            }

            fn insert(&mut self, m: Message) -> bool {
                if self.index.contains_key(&m.id) {
                    return false;
                }
                let key = (m.clock, m.sender, m.id.1);
                self.index.insert(m.id, key);
                self.messages.insert(key, m);
                true
            }

            /// Compose a message; returns the copy to send.
            pub fn compose(&mut self, body: &str) -> Message {
                self.clock += 1;
                let m = Message {
                    id: (self.me, self.next_local),
                    clock: self.clock,
                    sender: self.me,
                    body: body.to_owned(),
                    delivery: Delivery::Pending,
                };
                self.next_local += 1;
                self.insert(m.clone());
                m
            }

            /// Accept a message from the network. Returns false for duplicates.
            pub fn receive(&mut self, mut m: Message) -> bool {
                self.clock = self.clock.max(m.clock) + 1;
                m.delivery = Delivery::Delivered;
                self.insert(m)
            }

            /// Update an own message's delivery state (never moves backwards).
            pub fn mark(&mut self, id: (PeerId, u64), state: Delivery) -> bool {
                let Some(key) = self.index.get(&id) else {
                    return false;
                };
                match self.messages.get_mut(key) {
                    Some(m) if m.sender == self.me && state > m.delivery => {
                        m.delivery = state;
                        true
                    }
                    _ => false,
                }
            }

            /// Own messages not yet accepted by the transport (the outbox).
            pub fn outbox(&self) -> impl Iterator<Item = &Message> {
                self.messages
                    .values()
                    .filter(move |m| m.sender == self.me && m.delivery == Delivery::Pending)
            }

            /// Messages in display order.
            pub fn timeline(&self) -> impl Iterator<Item = &Message> {
                self.messages.values()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn same_order_on_both_devices() {
                let mut a = Conversation::new(1);
                let mut b = Conversation::new(2);
                let a1 = a.compose("hi");
                let b1 = b.compose("hello"); // concurrent with a1
                let a2 = a.compose("how are you?");
                // Deliver in different orders.
                assert!(b.receive(a2.clone()));
                assert!(b.receive(a1.clone()));
                assert!(a.receive(b1.clone()));
                let order =
                    |c: &Conversation| c.timeline().map(|m| m.body.clone()).collect::<Vec<_>>();
                assert_eq!(order(&a), order(&b));
                assert!(!b.receive(a1), "duplicate ignored");
                let b2 = b.compose("fine");
                assert!(b2.clock > a2.clock, "reply sorts after what was seen");
            }

            #[test]
            fn receipts_and_outbox() {
                let mut a = Conversation::new(1);
                let m = a.compose("x");
                assert_eq!(a.outbox().count(), 1);
                assert!(a.mark(m.id, Delivery::Sent));
                assert_eq!(a.outbox().count(), 0);
                assert!(a.mark(m.id, Delivery::Read));
                assert!(!a.mark(m.id, Delivery::Delivered), "never backwards");
                assert!(!a.mark((9, 9), Delivery::Read));
            }
        }
    }
    pub mod crypto {
        //! Session keys and sealed messages.

        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
        use core::fmt;
        use hkdf::Hkdf;
        use sha2::Sha256;
        use x25519_dalek::{PublicKey, StaticSecret};

        /// Crypto errors.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CryptoError {
            /// Authentication failed (tampered, wrong key, or wrong header).
            Open,
            /// Counter already seen or too old.
            Replay,
            /// Peer key is a low-order point.
            BadPeer,
        }

        impl fmt::Display for CryptoError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self {
                    Self::Open => "message failed authentication",
                    Self::Replay => "replayed message",
                    Self::BadPeer => "invalid peer key",
                })
            }
        }

        impl std::error::Error for CryptoError {}

        /// A device identity key pair.
        pub struct Identity {
            secret: StaticSecret,
            public: PublicKey,
        }

        impl fmt::Debug for Identity {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct("Identity")
                    .field("public", &self.public.as_bytes())
                    .finish_non_exhaustive()
            }
        }

        impl Identity {
            /// Identity from 32 secret bytes (from the OS random source in production).
            #[must_use]
            pub fn from_secret(bytes: [u8; 32]) -> Self {
                let secret = StaticSecret::from(bytes);
                let public = PublicKey::from(&secret);
                Self { secret, public }
            }

            /// Public key to share.
            #[must_use]
            pub fn public(&self) -> [u8; 32] {
                *self.public.as_bytes()
            }

            /// Session with `peer`. Both sides derive the same keys; each direction
            /// uses its own key.
            pub fn session(&self, peer: [u8; 32]) -> Result<Session, CryptoError> {
                let peer_pk = PublicKey::from(peer);
                let shared = self.secret.diffie_hellman(&peer_pk);
                if !shared.was_contributory() {
                    return Err(CryptoError::BadPeer);
                }
                let me = self.public();
                let (lo, hi) = if me < peer { (me, peer) } else { (peer, me) };
                let hk = Hkdf::<Sha256>::new(Some(b"mouli-comms-v1"), shared.as_bytes());
                let mut okm = [0u8; 64];
                let mut info = Vec::with_capacity(64);
                info.extend_from_slice(&lo);
                info.extend_from_slice(&hi);
                hk.expand(&info, &mut okm)
                    .map_err(|_| CryptoError::BadPeer)?;
                let (k_lo, k_hi) = okm.split_at(32);
                let (send, recv) = if me == lo { (k_lo, k_hi) } else { (k_hi, k_lo) };
                Ok(Session {
                    send: ChaCha20Poly1305::new(Key::from_slice(send)),
                    recv: ChaCha20Poly1305::new(Key::from_slice(recv)),
                    send_ctr: 0,
                    recv_window: Window::default(),
                })
            }
        }

        /// Sliding replay window over the last 64 counters.
        #[derive(Debug, Clone, Copy, Default)]
        struct Window {
            top: u64,
            bits: u64,
            any: bool,
        }

        impl Window {
            fn check_and_set(&mut self, c: u64) -> bool {
                if !self.any {
                    self.any = true;
                    self.top = c;
                    self.bits = 1;
                    return true;
                }
                if c > self.top {
                    let shift = c - self.top;
                    self.bits = if shift >= 64 { 0 } else { self.bits << shift };
                    self.bits |= 1;
                    self.top = c;
                    return true;
                }
                let back = self.top - c;
                if back >= 64 || (self.bits >> back) & 1 == 1 {
                    return false;
                }
                self.bits |= 1 << back;
                true
            }
        }

        /// A sealed message: counter plus ciphertext. The header is authenticated.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Sealed {
            /// Message counter (also the nonce).
            pub counter: u64,
            /// Ciphertext with tag.
            pub ciphertext: Vec<u8>,
        }

        /// Keys for one peer.
        pub struct Session {
            send: ChaCha20Poly1305,
            recv: ChaCha20Poly1305,
            send_ctr: u64,
            recv_window: Window,
        }

        impl fmt::Debug for Session {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct("Session")
                    .field("send_ctr", &self.send_ctr)
                    .finish_non_exhaustive()
            }
        }

        fn nonce(counter: u64) -> [u8; 12] {
            let mut n = [0u8; 12];
            n[4..].copy_from_slice(&counter.to_be_bytes());
            n
        }

        impl Session {
            /// Encrypt `plaintext`, binding `header` (e.g. conversation id and sender).
            pub fn seal(&mut self, header: &[u8], plaintext: &[u8]) -> Result<Sealed, CryptoError> {
                let counter = self.send_ctr;
                self.send_ctr += 1;
                let n = nonce(counter);
                let mut aad = header.to_vec();
                aad.extend_from_slice(&counter.to_be_bytes());
                let ciphertext = self
                    .send
                    .encrypt(
                        Nonce::from_slice(&n),
                        Payload {
                            msg: plaintext,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| CryptoError::Open)?;
                Ok(Sealed {
                    counter,
                    ciphertext,
                })
            }

            /// Decrypt and reject replays.
            pub fn open(&mut self, header: &[u8], m: &Sealed) -> Result<Vec<u8>, CryptoError> {
                let n = nonce(m.counter);
                let mut aad = header.to_vec();
                aad.extend_from_slice(&m.counter.to_be_bytes());
                let plain = self
                    .recv
                    .decrypt(
                        Nonce::from_slice(&n),
                        Payload {
                            msg: &m.ciphertext,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| CryptoError::Open)?;
                if !self.recv_window.check_and_set(m.counter) {
                    return Err(CryptoError::Replay);
                }
                Ok(plain)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn pair() -> (Session, Session) {
                let a = Identity::from_secret([1; 32]);
                let b = Identity::from_secret([2; 32]);
                (
                    a.session(b.public()).unwrap(),
                    b.session(a.public()).unwrap(),
                )
            }

            #[test]
            fn round_trip_both_directions() {
                let (mut a, mut b) = pair();
                let m = a.seal(b"chat:1|alice", b"hello bob").unwrap();
                assert_eq!(b.open(b"chat:1|alice", &m).unwrap(), b"hello bob");
                let r = b.seal(b"chat:1|bob", b"hi").unwrap();
                assert_eq!(a.open(b"chat:1|bob", &r).unwrap(), b"hi");
                // Directions use different keys: a cannot open its own message.
                let own = a.seal(b"h", b"x").unwrap();
                assert_eq!(a.open(b"h", &own), Err(CryptoError::Open));
            }

            #[test]
            fn tamper_header_and_replay() {
                let (mut a, mut b) = pair();
                let m = a.seal(b"hdr", b"pay 10").unwrap();
                let mut bad = m.clone();
                bad.ciphertext[0] ^= 1;
                assert_eq!(b.open(b"hdr", &bad), Err(CryptoError::Open));
                assert_eq!(b.open(b"other", &m), Err(CryptoError::Open));
                assert!(b.open(b"hdr", &m).is_ok());
                assert_eq!(b.open(b"hdr", &m), Err(CryptoError::Replay));
                // Out-of-order delivery inside the window is fine.
                let m1 = a.seal(b"hdr", b"1").unwrap();
                let m2 = a.seal(b"hdr", b"2").unwrap();
                assert!(b.open(b"hdr", &m2).is_ok());
                assert!(b.open(b"hdr", &m1).is_ok());
            }

            #[test]
            fn low_order_peer_rejected() {
                let a = Identity::from_secret([1; 32]);
                assert_eq!(a.session([0; 32]).unwrap_err(), CryptoError::BadPeer);
            }
        }
    }
}

#[cfg(feature = "topstar")]
#[allow(missing_docs)]
#[allow(clippy::module_inception)]
pub mod topstar {
    //! Foundational domain types for Topstar.

    pub mod action {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{ActorId, CapabilityId, ResponsibilityContractId};

        pub type ActionId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum ActionState {
            Proposed,
            Authorized,
            Executing,
            Succeeded,
            Failed,
            Cancelled,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Action {
            pub id: ActionId,
            pub actor: ActorId,
            pub capability: CapabilityId,
            pub responsibility: ResponsibilityContractId,
            pub state: ActionState,
            pub intent: String,
        }

        impl Action {
            pub fn new(
                actor: ActorId,
                capability: CapabilityId,
                responsibility: ResponsibilityContractId,
                intent: impl Into<String>,
            ) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    actor,
                    capability,
                    responsibility,
                    state: ActionState::Proposed,
                    intent: intent.into(),
                }
            }
        }
    }
    pub mod audit {
        use serde::{Deserialize, Serialize};
        use sha2::{Digest, Sha256};
        use uuid::Uuid;

        use crate::topstar::{ActionId, ActorId, EventId};

        pub type AuditEntryId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum AuditOutcome {
            Allowed,
            Denied,
            Started,
            Succeeded,
            Failed,
            Cancelled,
            Revoked,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct AuditEntry {
            pub id: AuditEntryId,
            pub actor: ActorId,
            pub action: Option<ActionId>,
            pub event: Option<EventId>,
            pub timestamp_unix: i64,
            pub outcome: AuditOutcome,
            pub detail: String,
            pub previous_hash: Option<String>,
            pub hash: String,
        }

        impl AuditEntry {
            pub fn new(
                actor: ActorId,
                action: Option<ActionId>,
                timestamp_unix: i64,
                outcome: AuditOutcome,
                detail: impl Into<String>,
            ) -> Self {
                Self::with_previous_hash(actor, action, timestamp_unix, outcome, detail, None)
            }

            pub fn with_previous_hash(
                actor: ActorId,
                action: Option<ActionId>,
                timestamp_unix: i64,
                outcome: AuditOutcome,
                detail: impl Into<String>,
                previous_hash: Option<String>,
            ) -> Self {
                let mut entry = Self {
                    id: Uuid::new_v4(),
                    actor,
                    action,
                    event: None,
                    timestamp_unix,
                    outcome,
                    detail: detail.into(),
                    previous_hash,
                    hash: String::new(),
                };
                entry.hash = entry.compute_hash();
                entry
            }

            pub fn compute_hash(&self) -> String {
                let mut hasher = Sha256::new();
                hasher.update(self.id.as_bytes());
                hasher.update(self.actor.as_bytes());
                if let Some(action) = self.action {
                    hasher.update(action.as_bytes());
                }
                if let Some(event) = self.event {
                    hasher.update(event.as_bytes());
                }
                hasher.update(self.timestamp_unix.to_le_bytes());
                hasher.update(serde_json::to_vec(&self.outcome).expect("audit outcome serializes"));
                hasher.update(self.detail.as_bytes());
                if let Some(previous_hash) = &self.previous_hash {
                    hasher.update(previous_hash.as_bytes());
                }
                format!("{:x}", hasher.finalize())
            }

            pub fn is_integrity_valid(&self) -> bool {
                self.hash == self.compute_hash()
            }
        }
    }
    pub mod authorization {
        use serde::{Deserialize, Serialize};

        use crate::topstar::{CapabilityId, Identity, Policy, ResponsibilityContract};

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum AuthorizationDecision {
            Allow,
            Deny(String),
        }

        pub fn authorize(
            identity: &Identity,
            capability: CapabilityId,
            responsibility: &ResponsibilityContract,
            policy: &Policy,
            now_unix: i64,
        ) -> AuthorizationDecision {
            if responsibility.actor != identity.id {
                return AuthorizationDecision::Deny("responsibility actor mismatch".into());
            }
            if !responsibility.allows(capability, now_unix) {
                return AuthorizationDecision::Deny(
                    "responsibility contract denies capability".into(),
                );
            }
            if !policy.applies_to(identity) || !policy.permits(capability) {
                return AuthorizationDecision::Deny("policy denies capability".into());
            }
            AuthorizationDecision::Allow
        }
    }
    pub mod boundary {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
        pub struct ExecutionBoundary {
            pub max_cost: Option<u64>,
            pub max_duration_ms: Option<u64>,
            pub network_allowed: bool,
            pub physical_control_allowed: bool,
            pub financial_control_allowed: bool,
            pub geographic_scope: Option<String>,
        }

        impl ExecutionBoundary {
            pub fn permits_network(&self) -> bool {
                self.network_allowed
            }

            pub fn permits_physical_control(&self) -> bool {
                self.physical_control_allowed
            }

            pub fn permits_financial_control(&self) -> bool {
                self.financial_control_allowed
            }
        }
    }
    pub mod capability {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        pub type CapabilityId = Uuid;

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum CapabilityRisk {
            Low,
            Medium,
            High,
            Critical,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Capability {
            pub id: CapabilityId,
            pub name: String,
            pub description: String,
            pub risk: CapabilityRisk,
        }

        impl Capability {
            pub fn new(
                name: impl Into<String>,
                description: impl Into<String>,
                risk: CapabilityRisk,
            ) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    name: name.into(),
                    description: description.into(),
                    risk,
                }
            }
        }
    }
    pub mod chat {
        use serde::{Deserialize, Serialize};
        use thiserror::Error;
        use uuid::Uuid;

        pub type ConversationId = Uuid;
        pub type MessageId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum ChatRole {
            User,
            Assistant,
            System,
            Tool,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ChatMessage {
            pub id: MessageId,
            pub role: ChatRole,
            pub content: String,
        }

        impl ChatMessage {
            pub fn new(role: ChatRole, content: impl Into<String>) -> Result<Self, ChatError> {
                let content = content.into();
                if content.trim().is_empty() {
                    return Err(ChatError::EmptyContent);
                }
                Ok(Self {
                    id: Uuid::new_v4(),
                    role,
                    content,
                })
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ChatRequest {
            pub conversation_id: ConversationId,
            pub messages: Vec<ChatMessage>,
        }

        impl ChatRequest {
            pub fn validate(&self) -> Result<(), ChatError> {
                if self.messages.is_empty() {
                    return Err(ChatError::EmptyConversation);
                }
                if self
                    .messages
                    .iter()
                    .any(|message| message.content.trim().is_empty())
                {
                    return Err(ChatError::EmptyContent);
                }
                Ok(())
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ChatResponse {
            pub conversation_id: ConversationId,
            pub message: ChatMessage,
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum ChatError {
            #[error("chat message content cannot be empty")]
            EmptyContent,
            #[error("chat request must contain at least one message")]
            EmptyConversation,
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn message_rejects_empty_content() {
                assert_eq!(
                    ChatMessage::new(ChatRole::User, "  "),
                    Err(ChatError::EmptyContent)
                );
            }

            #[test]
            fn request_requires_messages() {
                let request = ChatRequest {
                    conversation_id: Uuid::new_v4(),
                    messages: Vec::new(),
                };
                assert_eq!(request.validate(), Err(ChatError::EmptyConversation));
            }

            #[test]
            fn request_serializes_for_http_transport() {
                let message = ChatMessage::new(ChatRole::User, "Hello Topstar").expect("message");
                let request = ChatRequest {
                    conversation_id: Uuid::new_v4(),
                    messages: vec![message],
                };
                let encoded = serde_json::to_string(&request).expect("json");
                let decoded: ChatRequest = serde_json::from_str(&encoded).expect("decode");
                assert_eq!(decoded, request);
            }
        }
    }
    pub mod context {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        pub type ContextId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Context {
            pub id: ContextId,
            pub values: serde_json::Value,
        }

        impl Context {
            pub fn new(values: serde_json::Value) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    values,
                }
            }
        }
    }
    pub mod device {
        use serde::{Deserialize, Serialize};
        use thiserror::Error;
        use uuid::Uuid;

        use crate::topstar::{CapabilityId, NetworkAddress, Resource, SessionState};

        pub type DeviceId = Uuid;

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum MachineProfile {
            Tiny,
            Embedded,
            Iot,
            Wearable,
            Drone,
            Robot,
            Mobile,
            Computer,
            Workstation,
            Server,
            Edge,
            Cluster,
            Cloud,
            Vehicle,
            Industrial,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum PowerState {
            Unknown,
            Off,
            Booting,
            On,
            Sleep,
            Hibernating,
            ShuttingDown,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum SecurityState {
            Unknown,
            Untrusted,
            Provisioning,
            Trusted,
            Quarantined,
            Revoked,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum HealthState {
            Unknown,
            Healthy,
            Degraded,
            Failed,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct HardwareInfo {
            pub architecture: String,
            pub cpu_cores: Option<u32>,
            pub cpu_threads: Option<u32>,
            pub gpu_count: Option<u32>,
            pub npu_count: Option<u32>,
            pub memory_bytes: Option<u64>,
            pub storage_bytes: Option<u64>,
        }

        impl HardwareInfo {
            pub fn new(architecture: impl Into<String>) -> Result<Self, DeviceError> {
                let architecture = architecture.into();
                if architecture.trim().is_empty() {
                    return Err(DeviceError::InvalidHardware("architecture is empty"));
                }
                Ok(Self {
                    architecture,
                    cpu_cores: None,
                    cpu_threads: None,
                    gpu_count: None,
                    npu_count: None,
                    memory_bytes: None,
                    storage_bytes: None,
                })
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Device {
            pub id: DeviceId,
            pub name: String,
            pub profile: MachineProfile,
            pub hardware: HardwareInfo,
            pub capabilities: Vec<CapabilityId>,
            pub resources: Vec<Resource>,
            pub network_addresses: Vec<NetworkAddress>,
            pub power: PowerState,
            pub security: SecurityState,
            pub health: HealthState,
            pub session_state: Option<SessionState>,
        }

        impl Device {
            pub fn new(
                name: impl Into<String>,
                profile: MachineProfile,
                hardware: HardwareInfo,
            ) -> Result<Self, DeviceError> {
                let name = name.into();
                if name.trim().is_empty() {
                    return Err(DeviceError::InvalidDevice("device name is empty"));
                }

                Ok(Self {
                    id: Uuid::new_v4(),
                    name,
                    profile,
                    hardware,
                    capabilities: Vec::new(),
                    resources: Vec::new(),
                    network_addresses: Vec::new(),
                    power: PowerState::Unknown,
                    security: SecurityState::Unknown,
                    health: HealthState::Unknown,
                    session_state: None,
                })
            }

            pub fn add_capability(&mut self, capability: CapabilityId) {
                if !self.capabilities.contains(&capability) {
                    self.capabilities.push(capability);
                }
            }

            pub fn add_resource(&mut self, resource: Resource) {
                if !self
                    .resources
                    .iter()
                    .any(|candidate| candidate.id == resource.id)
                {
                    self.resources.push(resource);
                }
            }

            pub fn add_network_address(&mut self, address: NetworkAddress) {
                if !self.network_addresses.contains(&address) {
                    self.network_addresses.push(address);
                }
            }

            pub fn set_power_state(&mut self, state: PowerState) {
                self.power = state;
            }

            pub fn set_security_state(&mut self, state: SecurityState) {
                self.security = state;
            }

            pub fn set_health_state(&mut self, state: HealthState) {
                self.health = state;
            }

            pub fn bind_session(&mut self, state: SessionState) -> Result<(), DeviceError> {
                if state == SessionState::Closed {
                    return Err(DeviceError::InvalidSessionBinding);
                }
                self.session_state = Some(state);
                Ok(())
            }

            pub fn can_execute(&self) -> bool {
                matches!(self.security, SecurityState::Trusted)
                    && matches!(self.power, PowerState::On)
                    && matches!(self.health, HealthState::Healthy | HealthState::Degraded)
            }
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum DeviceError {
            #[error("invalid device: {0}")]
            InvalidDevice(&'static str),
            #[error("invalid hardware: {0}")]
            InvalidHardware(&'static str),
            #[error("closed sessions cannot be bound to a device")]
            InvalidSessionBinding,
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn device_deduplicates_capabilities_resources_and_addresses() {
                let hardware = HardwareInfo::new("x86_64").expect("hardware");
                let mut device =
                    Device::new("node", MachineProfile::Server, hardware).expect("device");
                let capability = Uuid::new_v4();
                let resource = Resource::new(crate::topstar::ResourceKind::Memory, "ram");
                let address = NetworkAddress::new(
                    crate::topstar::Transport::Tcp,
                    crate::topstar::Endpoint::new("127.0.0.1", 8443).expect("endpoint"),
                );

                device.add_capability(capability);
                device.add_capability(capability);
                device.add_resource(resource.clone());
                device.add_resource(resource);
                device.add_network_address(address.clone());
                device.add_network_address(address);

                assert_eq!(device.capabilities.len(), 1);
                assert_eq!(device.resources.len(), 1);
                assert_eq!(device.network_addresses.len(), 1);
            }

            #[test]
            fn execution_requires_trusted_healthy_powered_device() {
                let hardware = HardwareInfo::new("aarch64").expect("hardware");
                let mut device =
                    Device::new("edge", MachineProfile::Edge, hardware).expect("device");

                assert!(!device.can_execute());

                device.set_security_state(SecurityState::Trusted);
                device.set_power_state(PowerState::On);
                device.set_health_state(HealthState::Healthy);

                assert!(device.can_execute());
            }

            #[test]
            fn closed_session_cannot_be_bound() {
                let hardware = HardwareInfo::new("x86_64").expect("hardware");
                let mut device =
                    Device::new("node", MachineProfile::Computer, hardware).expect("device");

                assert_eq!(
                    device.bind_session(SessionState::Closed),
                    Err(DeviceError::InvalidSessionBinding)
                );
            }
        }
    }
    pub mod device_discovery {
        use crate::topstar::{
            CapabilityId, DeviceId, DeviceLifecycle, DeviceRecord, DeviceRegistry,
            DeviceRegistryError, MachineProfile,
        };

        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct DeviceSelector {
            pub profile: Option<MachineProfile>,
            pub capability: Option<CapabilityId>,
            pub lifecycle: Option<DeviceLifecycle>,
            pub executable: Option<bool>,
        }

        impl DeviceSelector {
            pub fn matches(&self, record: &DeviceRecord) -> bool {
                if self
                    .profile
                    .is_some_and(|profile| record.device.profile != profile)
                {
                    return false;
                }
                if self
                    .capability
                    .is_some_and(|capability| !record.device.capabilities.contains(&capability))
                {
                    return false;
                }
                if self
                    .lifecycle
                    .is_some_and(|lifecycle| record.lifecycle != lifecycle)
                {
                    return false;
                }
                if self
                    .executable
                    .is_some_and(|executable| record.device.can_execute() != executable)
                {
                    return false;
                }
                true
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct DeviceSnapshot {
            pub id: DeviceId,
            pub profile: MachineProfile,
            pub lifecycle: DeviceLifecycle,
            pub executable: bool,
        }

        impl From<&DeviceRecord> for DeviceSnapshot {
            fn from(record: &DeviceRecord) -> Self {
                Self {
                    id: record.device.id,
                    profile: record.device.profile,
                    lifecycle: record.lifecycle,
                    executable: record.device.can_execute(),
                }
            }
        }

        impl DeviceRegistry {
            pub fn discover(
                &self,
                selector: &DeviceSelector,
            ) -> Result<Vec<DeviceSnapshot>, DeviceRegistryError> {
                let mut snapshots = self
                    .list()?
                    .iter()
                    .filter(|record| selector.matches(record))
                    .map(DeviceSnapshot::from)
                    .collect::<Vec<_>>();
                snapshots.sort_by_key(|snapshot| snapshot.id);
                Ok(snapshots)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::{HardwareInfo, MachineProfile};

            fn device(name: &str, profile: MachineProfile) -> crate::topstar::Device {
                crate::topstar::Device::new(
                    name,
                    profile,
                    HardwareInfo::new("x86_64").expect("hardware"),
                )
                .expect("device")
            }

            #[test]
            fn discovery_filters_profile_and_lifecycle() {
                let registry = DeviceRegistry::new();
                let server = device("server", MachineProfile::Server);
                let edge = device("edge", MachineProfile::Edge);
                let edge_id = edge.id;

                registry.register(server).expect("server");
                registry.register(edge).expect("edge");
                registry
                    .set_lifecycle(edge_id, DeviceLifecycle::Quarantined)
                    .expect("quarantine");

                let selector = DeviceSelector {
                    profile: Some(MachineProfile::Edge),
                    lifecycle: Some(DeviceLifecycle::Quarantined),
                    ..Default::default()
                };

                let found = registry.discover(&selector).expect("discover");
                assert_eq!(found.len(), 1);
                assert_eq!(found[0].id, edge_id);
            }
        }
    }
    pub mod device_registry {
        use std::collections::HashMap;
        use std::sync::{Arc, RwLock};

        use thiserror::Error;

        use crate::topstar::{Device, DeviceId, SecurityState};

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DeviceLifecycle {
            Active,
            Quarantined,
            Revoked,
        }

        #[derive(Debug, Clone)]
        pub struct DeviceRecord {
            pub device: Device,
            pub lifecycle: DeviceLifecycle,
        }

        #[derive(Debug, Clone, Default)]
        pub struct DeviceRegistry {
            devices: Arc<RwLock<HashMap<DeviceId, DeviceRecord>>>,
        }

        impl DeviceRegistry {
            pub fn new() -> Self {
                Self::default()
            }

            pub fn register(&self, device: Device) -> Result<DeviceId, DeviceRegistryError> {
                let id = device.id;
                let mut devices = self
                    .devices
                    .write()
                    .map_err(|_| DeviceRegistryError::LockPoisoned)?;
                if devices.contains_key(&id) {
                    return Err(DeviceRegistryError::AlreadyRegistered(id));
                }
                let lifecycle = match device.security {
                    SecurityState::Quarantined => DeviceLifecycle::Quarantined,
                    SecurityState::Revoked => DeviceLifecycle::Revoked,
                    _ => DeviceLifecycle::Active,
                };
                devices.insert(id, DeviceRecord { device, lifecycle });
                Ok(id)
            }

            pub fn list(&self) -> Result<Vec<DeviceRecord>, DeviceRegistryError> {
                let devices = self
                    .devices
                    .read()
                    .map_err(|_| DeviceRegistryError::LockPoisoned)?;
                let mut records = devices.values().cloned().collect::<Vec<_>>();
                records.sort_by_key(|record| record.device.id);
                Ok(records)
            }

            pub fn get(&self, id: DeviceId) -> Result<Option<DeviceRecord>, DeviceRegistryError> {
                let devices = self
                    .devices
                    .read()
                    .map_err(|_| DeviceRegistryError::LockPoisoned)?;
                Ok(devices.get(&id).cloned())
            }

            pub fn set_lifecycle(
                &self,
                id: DeviceId,
                lifecycle: DeviceLifecycle,
            ) -> Result<(), DeviceRegistryError> {
                let mut devices = self
                    .devices
                    .write()
                    .map_err(|_| DeviceRegistryError::LockPoisoned)?;
                let record = devices
                    .get_mut(&id)
                    .ok_or(DeviceRegistryError::NotFound(id))?;
                record.lifecycle = lifecycle;
                record.device.security = match lifecycle {
                    DeviceLifecycle::Active => SecurityState::Trusted,
                    DeviceLifecycle::Quarantined => SecurityState::Quarantined,
                    DeviceLifecycle::Revoked => SecurityState::Revoked,
                };
                Ok(())
            }

            pub fn unregister(&self, id: DeviceId) -> Result<DeviceRecord, DeviceRegistryError> {
                let mut devices = self
                    .devices
                    .write()
                    .map_err(|_| DeviceRegistryError::LockPoisoned)?;
                devices.remove(&id).ok_or(DeviceRegistryError::NotFound(id))
            }

            pub fn len(&self) -> Result<usize, DeviceRegistryError> {
                let devices = self
                    .devices
                    .read()
                    .map_err(|_| DeviceRegistryError::LockPoisoned)?;
                Ok(devices.len())
            }

            pub fn is_empty(&self) -> Result<bool, DeviceRegistryError> {
                Ok(self.len()? == 0)
            }
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum DeviceRegistryError {
            #[error("device {0} is already registered")]
            AlreadyRegistered(DeviceId),
            #[error("device {0} was not found")]
            NotFound(DeviceId),
            #[error("device registry lock is poisoned")]
            LockPoisoned,
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::{HardwareInfo, MachineProfile};

            fn device(name: &str) -> Device {
                Device::new(
                    name,
                    MachineProfile::Server,
                    HardwareInfo::new("x86_64").expect("hardware"),
                )
                .expect("device")
            }

            #[test]
            fn registration_and_lookup_are_durable_in_memory() {
                let registry = DeviceRegistry::new();
                let id = registry.register(device("node")).expect("register");

                assert_eq!(registry.len().expect("len"), 1);
                assert_eq!(
                    registry.get(id).expect("lookup").expect("record").device.id,
                    id
                );
            }

            #[test]
            fn duplicate_registration_is_rejected() {
                let registry = DeviceRegistry::new();
                let first = device("node");
                let id = first.id;
                registry.register(first).expect("register");

                let mut duplicate = device("other");
                duplicate.id = id;

                assert_eq!(
                    registry.register(duplicate),
                    Err(DeviceRegistryError::AlreadyRegistered(id))
                );
            }

            #[test]
            fn quarantine_and_revoke_change_security_state() {
                let registry = DeviceRegistry::new();
                let id = registry.register(device("node")).expect("register");

                registry
                    .set_lifecycle(id, DeviceLifecycle::Quarantined)
                    .expect("quarantine");
                assert_eq!(
                    registry
                        .get(id)
                        .expect("lookup")
                        .expect("record")
                        .device
                        .security,
                    SecurityState::Quarantined
                );

                registry
                    .set_lifecycle(id, DeviceLifecycle::Revoked)
                    .expect("revoke");
                assert_eq!(
                    registry
                        .get(id)
                        .expect("lookup")
                        .expect("record")
                        .device
                        .security,
                    SecurityState::Revoked
                );
            }
        }
    }
    pub mod event {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{ActionId, ActorId};

        pub type EventId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Event {
            pub id: EventId,
            pub actor: ActorId,
            pub action: Option<ActionId>,
            pub event_type: String,
            pub timestamp_unix: i64,
            pub payload: serde_json::Value,
        }

        impl Event {
            pub fn new(
                actor: ActorId,
                action: Option<ActionId>,
                event_type: impl Into<String>,
                timestamp_unix: i64,
                payload: serde_json::Value,
            ) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    actor,
                    action,
                    event_type: event_type.into(),
                    timestamp_unix,
                    payload,
                }
            }
        }
    }
    pub mod eventing {
        use std::collections::{HashMap, VecDeque};

        use thiserror::Error;
        use uuid::Uuid;

        use crate::topstar::{Event, Journal, PersistenceError};

        pub type SubscriptionId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        pub struct EventRecord {
            pub sequence: u64,
            pub event: Event,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct EventFilter {
            pub event_type: Option<String>,
            pub actor: Option<Uuid>,
        }

        impl EventFilter {
            pub fn all() -> Self {
                Self {
                    event_type: None,
                    actor: None,
                }
            }

            pub fn matches(&self, event: &Event) -> bool {
                self.event_type
                    .as_ref()
                    .is_none_or(|event_type| event.event_type == *event_type)
                    && self.actor.is_none_or(|actor| event.actor == actor)
            }
        }

        #[derive(Debug)]
        struct Subscription {
            filter: EventFilter,
            queue: VecDeque<EventRecord>,
            capacity: usize,
        }

        #[derive(Debug, Error)]
        pub enum EventingError {
            #[error("subscription not found: {0}")]
            SubscriptionNotFound(SubscriptionId),
            #[error("subscription queue is full: {0}")]
            QueueFull(SubscriptionId),
            #[error("subscription capacity must be greater than zero")]
            InvalidCapacity,
            #[error("event persistence failed: {0}")]
            Persistence(#[from] PersistenceError),
        }

        #[derive(Debug)]
        pub struct EventBroker {
            subscriptions: HashMap<SubscriptionId, Subscription>,
            sequence: u64,
            journal: Option<Journal>,
        }

        impl Default for EventBroker {
            fn default() -> Self {
                Self::new()
            }
        }

        impl EventBroker {
            pub fn new() -> Self {
                Self {
                    subscriptions: HashMap::new(),
                    sequence: 0,
                    journal: None,
                }
            }

            pub fn with_journal(journal: Journal) -> Result<Self, EventingError> {
                let sequence = journal.next_sequence();
                Ok(Self {
                    subscriptions: HashMap::new(),
                    sequence,
                    journal: Some(journal),
                })
            }

            pub fn sequence(&self) -> u64 {
                self.sequence
            }

            pub fn subscribe(
                &mut self,
                filter: EventFilter,
                capacity: usize,
            ) -> Result<SubscriptionId, EventingError> {
                if capacity == 0 {
                    return Err(EventingError::InvalidCapacity);
                }

                let id = Uuid::new_v4();
                self.subscriptions.insert(
                    id,
                    Subscription {
                        filter,
                        queue: VecDeque::with_capacity(capacity),
                        capacity,
                    },
                );
                Ok(id)
            }

            pub fn unsubscribe(&mut self, id: SubscriptionId) -> Result<(), EventingError> {
                self.subscriptions
                    .remove(&id)
                    .map(|_| ())
                    .ok_or(EventingError::SubscriptionNotFound(id))
            }

            pub fn publish(&mut self, event: Event) -> Result<EventRecord, EventingError> {
                let record = EventRecord {
                    sequence: self.sequence,
                    event,
                };

                for (id, subscription) in &self.subscriptions {
                    if subscription.filter.matches(&record.event)
                        && subscription.queue.len() >= subscription.capacity
                    {
                        return Err(EventingError::QueueFull(*id));
                    }
                }

                if let Some(journal) = self.journal.as_mut() {
                    journal.append(&record)?;
                }

                for subscription in self.subscriptions.values_mut() {
                    if !subscription.filter.matches(&record.event) {
                        continue;
                    }
                    subscription.queue.push_back(record.clone());
                }

                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .expect("event sequence exhausted");

                Ok(record)
            }

            pub fn poll(
                &mut self,
                id: SubscriptionId,
                limit: usize,
            ) -> Result<Vec<EventRecord>, EventingError> {
                let subscription = self
                    .subscriptions
                    .get_mut(&id)
                    .ok_or(EventingError::SubscriptionNotFound(id))?;

                let count = limit.min(subscription.queue.len());
                Ok(subscription.queue.drain(..count).collect())
            }

            pub fn pending(&self, id: SubscriptionId) -> Result<usize, EventingError> {
                self.subscriptions
                    .get(&id)
                    .map(|subscription| subscription.queue.len())
                    .ok_or(EventingError::SubscriptionNotFound(id))
            }

            pub fn replay<T>(&self) -> Result<Vec<crate::topstar::JournalRecord<T>>, EventingError>
            where
                T: serde::de::DeserializeOwned,
            {
                self.journal
                    .as_ref()
                    .map(Journal::read_all::<T>)
                    .transpose()?
                    .ok_or(EventingError::Persistence(PersistenceError::InvalidHeader))
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::Identity;

            fn event(event_type: &str) -> Event {
                Event::new(
                    Identity::new("agent").id,
                    None,
                    event_type,
                    1,
                    serde_json::json!({"value": 1}),
                )
            }

            #[test]
            fn publishes_only_matching_events() {
                let mut broker = EventBroker::new();
                let subscription = broker
                    .subscribe(
                        EventFilter {
                            event_type: Some("action.completed".into()),
                            actor: None,
                        },
                        4,
                    )
                    .expect("subscription should succeed");

                broker
                    .publish(event("action.started"))
                    .expect("publish should succeed");
                broker
                    .publish(event("action.completed"))
                    .expect("publish should succeed");

                let records = broker.poll(subscription, 10).expect("poll should succeed");
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].event.event_type, "action.completed");
                assert_eq!(records[0].sequence, 1);
            }

            #[test]
            fn queue_backpressure_is_explicit() {
                let mut broker = EventBroker::new();
                let subscription = broker
                    .subscribe(EventFilter::all(), 1)
                    .expect("subscription should succeed");

                broker
                    .publish(event("first"))
                    .expect("first publish should succeed");
                assert!(matches!(
                    broker.publish(event("second")),
                    Err(EventingError::QueueFull(id)) if id == subscription
                ));
            }

            #[test]
            fn unsubscribe_removes_subscription() {
                let mut broker = EventBroker::new();
                let subscription = broker
                    .subscribe(EventFilter::all(), 1)
                    .expect("subscription should succeed");
                broker
                    .unsubscribe(subscription)
                    .expect("unsubscribe should succeed");
                assert!(matches!(
                    broker.pending(subscription),
                    Err(EventingError::SubscriptionNotFound(id)) if id == subscription
                ));
            }
        }
    }
    pub mod evidence {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{ActionId, EventId};

        pub type EvidenceId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Evidence {
            pub id: EvidenceId,
            pub action: Option<ActionId>,
            pub event: Option<EventId>,
            pub source: String,
            pub content: serde_json::Value,
            pub timestamp_unix: i64,
        }

        impl Evidence {
            pub fn new(
                source: impl Into<String>,
                content: serde_json::Value,
                timestamp_unix: i64,
            ) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    action: None,
                    event: None,
                    source: source.into(),
                    content,
                    timestamp_unix,
                }
            }
        }
    }
    pub mod execution_audit {
        use std::path::Path;
        use std::sync::{Arc, Mutex};

        use thiserror::Error;

        use crate::topstar::{
            ActionId, ActorId, AuditEntry, AuditOutcome, Journal, JournalRecord, PersistenceError,
        };

        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct AuditQuery {
            pub actor: Option<ActorId>,
            pub action: Option<ActionId>,
            pub outcome: Option<AuditOutcome>,
            pub from_unix: Option<i64>,
            pub to_unix: Option<i64>,
        }

        impl AuditQuery {
            fn matches(&self, entry: &AuditEntry) -> bool {
                self.actor.is_none_or(|v| v == entry.actor)
                    && self.action.is_none_or(|v| Some(v) == entry.action)
                    && self.outcome.as_ref().is_none_or(|v| v == &entry.outcome)
                    && self.from_unix.is_none_or(|v| entry.timestamp_unix >= v)
                    && self.to_unix.is_none_or(|v| entry.timestamp_unix <= v)
            }
        }

        #[derive(Debug, Error)]
        pub enum ExecutionAuditError {
            #[error("audit journal error: {0}")]
            Persistence(#[from] PersistenceError),
            #[error("audit integrity violation at entry {0}")]
            IntegrityViolation(crate::topstar::AuditEntryId),
        }

        #[derive(Debug, Clone, Default)]
        pub struct ExecutionAuditLog {
            entries: Arc<Mutex<Vec<AuditEntry>>>,
            journal: Option<Arc<Mutex<Journal>>>,
            max_entries: Option<usize>,
        }

        impl ExecutionAuditLog {
            pub fn new() -> Self {
                Self::default()
            }

            pub fn with_capacity(max_entries: usize) -> Self {
                Self {
                    entries: Arc::new(Mutex::new(Vec::with_capacity(max_entries.min(1024)))),
                    journal: None,
                    max_entries: Some(max_entries),
                }
            }

            pub fn with_journal(path: impl AsRef<Path>) -> Result<Self, ExecutionAuditError> {
                let journal = Journal::open(path)?;
                let records: Vec<JournalRecord<AuditEntry>> = journal.read_all()?;
                let entries: Vec<AuditEntry> =
                    records.into_iter().map(|record| record.payload).collect();
                Self::validate_chain(&entries)?;
                Ok(Self {
                    entries: Arc::new(Mutex::new(entries)),
                    journal: Some(Arc::new(Mutex::new(journal))),
                    max_entries: None,
                })
            }

            pub fn validate_chain(entries: &[AuditEntry]) -> Result<(), ExecutionAuditError> {
                let mut previous = None;
                for entry in entries {
                    if !entry.is_integrity_valid() || entry.previous_hash != previous {
                        return Err(ExecutionAuditError::IntegrityViolation(entry.id));
                    }
                    previous = Some(entry.hash.clone());
                }
                Ok(())
            }

            pub fn append(&self, mut entry: AuditEntry) -> Result<(), ExecutionAuditError> {
                let mut entries = self.entries.lock().expect("audit log lock poisoned");
                if entry.previous_hash.is_none() {
                    if let Some(previous) = entries.last() {
                        entry.previous_hash = Some(previous.hash.clone());
                        entry.hash = entry.compute_hash();
                    }
                }
                if !entry.is_integrity_valid() {
                    return Err(ExecutionAuditError::IntegrityViolation(entry.id));
                }
                if let Some(journal) = &self.journal {
                    journal
                        .lock()
                        .expect("audit journal lock poisoned")
                        .append(&entry)?;
                }
                entries.push(entry);
                if let Some(max_entries) = self.max_entries {
                    if max_entries == 0 {
                        entries.clear();
                    } else if entries.len() > max_entries {
                        let excess = entries.len() - max_entries;
                        entries.drain(..excess);
                    }
                }
                Ok(())
            }

            pub fn record(
                &self,
                actor: ActorId,
                action: Option<ActionId>,
                timestamp_unix: i64,
                outcome: AuditOutcome,
                detail: impl Into<String>,
            ) -> Result<(), ExecutionAuditError> {
                self.append(AuditEntry::new(
                    actor,
                    action,
                    timestamp_unix,
                    outcome,
                    detail,
                ))
            }

            pub fn entries(&self) -> Vec<AuditEntry> {
                self.entries
                    .lock()
                    .expect("audit log lock poisoned")
                    .clone()
            }

            pub fn query(&self, query: &AuditQuery) -> Vec<AuditEntry> {
                self.entries
                    .lock()
                    .expect("audit log lock poisoned")
                    .iter()
                    .filter(|entry| query.matches(entry))
                    .cloned()
                    .collect()
            }

            pub fn len(&self) -> usize {
                self.entries.lock().expect("audit log lock poisoned").len()
            }

            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }

            pub fn verify_integrity(&self) -> Result<(), ExecutionAuditError> {
                let entries = self.entries();
                Self::validate_chain(&entries)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use uuid::Uuid;

            #[test]
            fn audit_integrity_forms_a_chain() {
                let log = ExecutionAuditLog::new();
                let actor = Uuid::new_v4();
                log.record(actor, None, 1, AuditOutcome::Started, "one")
                    .expect("append");
                log.record(actor, None, 2, AuditOutcome::Succeeded, "two")
                    .expect("append");

                let entries = log.entries();
                assert!(entries.iter().all(AuditEntry::is_integrity_valid));
                assert_eq!(
                    entries[1].previous_hash.as_deref(),
                    Some(entries[0].hash.as_str())
                );
            }

            #[test]
            fn tampered_entry_is_rejected() {
                let log = ExecutionAuditLog::new();
                let actor = Uuid::new_v4();
                let mut entry = AuditEntry::new(actor, None, 1, AuditOutcome::Started, "one");
                entry.detail = "tampered".into();
                assert!(matches!(
                    log.append(entry),
                    Err(ExecutionAuditError::IntegrityViolation(_))
                ));
            }

            #[test]
            fn audit_query_filters_by_actor_and_outcome() {
                let log = ExecutionAuditLog::new();
                let actor = Uuid::new_v4();
                let other = Uuid::new_v4();
                log.record(actor, None, 1, AuditOutcome::Started, "started")
                    .expect("append");
                log.record(actor, None, 2, AuditOutcome::Succeeded, "succeeded")
                    .expect("append");
                log.record(other, None, 3, AuditOutcome::Succeeded, "other")
                    .expect("append");

                let query = AuditQuery {
                    actor: Some(actor),
                    outcome: Some(AuditOutcome::Succeeded),
                    ..Default::default()
                };
                let entries = log.query(&query);
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].timestamp_unix, 2);
            }

            #[test]
            fn audit_query_filters_time_range() {
                let log = ExecutionAuditLog::new();
                let actor = Uuid::new_v4();
                log.record(actor, None, 10, AuditOutcome::Started, "before")
                    .expect("append");
                log.record(actor, None, 20, AuditOutcome::Succeeded, "inside")
                    .expect("append");
                log.record(actor, None, 30, AuditOutcome::Succeeded, "after")
                    .expect("append");

                let query = AuditQuery {
                    from_unix: Some(15),
                    to_unix: Some(25),
                    ..Default::default()
                };
                let entries = log.query(&query);
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].timestamp_unix, 20);
            }

            #[test]
            fn audit_log_preserves_execution_order() {
                let log = ExecutionAuditLog::new();
                let actor = Uuid::new_v4();
                let action = Uuid::new_v4();

                log.record(actor, Some(action), 1, AuditOutcome::Started, "started")
                    .expect("append");
                log.record(actor, Some(action), 2, AuditOutcome::Succeeded, "succeeded")
                    .expect("append");

                let entries = log.entries();
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].outcome, AuditOutcome::Started);
                assert_eq!(entries[1].outcome, AuditOutcome::Succeeded);
            }
        }
    }
    pub mod execution_record {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{
            ActionId, ActorId, CapabilityId, Evidence, EvidenceId, ResponsibilityContractId,
        };

        pub type ExecutionRecordId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ExecutionRecord {
            pub id: ExecutionRecordId,
            pub actor: ActorId,
            pub action: ActionId,
            pub capability: CapabilityId,
            pub responsibility: ResponsibilityContractId,
            pub started_at_unix: i64,
            pub completed_at_unix: Option<i64>,
            pub outcome: ExecutionOutcome,
            pub evidence: Vec<EvidenceId>,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum ExecutionOutcome {
            Started,
            Succeeded,
            Failed,
            Cancelled,
            Revoked,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ExecutionRecordError {
            AlreadyCompleted,
            InvalidCompletionOutcome,
            InvalidCompletionTimestamp,
            EvidenceActionMismatch,
        }

        impl std::fmt::Display for ExecutionRecordError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::AlreadyCompleted => f.write_str("execution record is already completed"),
                    Self::InvalidCompletionOutcome => {
                        f.write_str("execution completion requires a terminal outcome")
                    }
                    Self::InvalidCompletionTimestamp => {
                        f.write_str("execution completion timestamp precedes start timestamp")
                    }
                    Self::EvidenceActionMismatch => {
                        f.write_str("evidence belongs to a different action")
                    }
                }
            }
        }

        impl std::error::Error for ExecutionRecordError {}

        impl ExecutionRecord {
            pub fn started(
                actor: ActorId,
                action: ActionId,
                capability: CapabilityId,
                responsibility: ResponsibilityContractId,
                timestamp_unix: i64,
            ) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    actor,
                    action,
                    capability,
                    responsibility,
                    started_at_unix: timestamp_unix,
                    completed_at_unix: None,
                    outcome: ExecutionOutcome::Started,
                    evidence: Vec::new(),
                }
            }

            pub fn is_complete(&self) -> bool {
                self.completed_at_unix.is_some()
            }

            pub fn complete(
                &mut self,
                timestamp_unix: i64,
                outcome: ExecutionOutcome,
            ) -> Result<(), ExecutionRecordError> {
                if self.is_complete() {
                    return Err(ExecutionRecordError::AlreadyCompleted);
                }
                if !matches!(
                    outcome,
                    ExecutionOutcome::Succeeded
                        | ExecutionOutcome::Failed
                        | ExecutionOutcome::Cancelled
                        | ExecutionOutcome::Revoked
                ) {
                    return Err(ExecutionRecordError::InvalidCompletionOutcome);
                }
                if timestamp_unix < self.started_at_unix {
                    return Err(ExecutionRecordError::InvalidCompletionTimestamp);
                }

                self.completed_at_unix = Some(timestamp_unix);
                self.outcome = outcome;
                Ok(())
            }

            pub fn attach_evidence(
                &mut self,
                evidence: &Evidence,
            ) -> Result<(), ExecutionRecordError> {
                if let Some(action) = evidence.action {
                    if action != self.action {
                        return Err(ExecutionRecordError::EvidenceActionMismatch);
                    }
                }
                if !self.evidence.contains(&evidence.id) {
                    self.evidence.push(evidence.id);
                }
                Ok(())
            }

            pub fn add_evidence(&mut self, evidence: EvidenceId) {
                if !self.evidence.contains(&evidence) {
                    self.evidence.push(evidence);
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn record() -> ExecutionRecord {
                ExecutionRecord::started(
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    10,
                )
            }

            #[test]
            fn execution_record_tracks_completion_and_evidence() {
                let evidence = Uuid::new_v4();
                let mut record = record();
                record.add_evidence(evidence);
                record.add_evidence(evidence);
                record
                    .complete(20, ExecutionOutcome::Succeeded)
                    .expect("completion");

                assert!(record.is_complete());
                assert_eq!(record.completed_at_unix, Some(20));
                assert_eq!(record.outcome, ExecutionOutcome::Succeeded);
                assert_eq!(record.evidence, vec![evidence]);
            }

            #[test]
            fn attach_evidence_accepts_unbound_evidence() {
                let mut record = record();
                let evidence = Evidence::new("executor", serde_json::json!({"ok": true}), 20);
                record.attach_evidence(&evidence).expect("attach");
                assert_eq!(record.evidence, vec![evidence.id]);
            }

            #[test]
            fn attach_evidence_rejects_evidence_for_another_action() {
                let mut record = record();
                let mut evidence = Evidence::new("executor", serde_json::json!({"ok": true}), 20);
                evidence.action = Some(Uuid::new_v4());

                assert_eq!(
                    record.attach_evidence(&evidence),
                    Err(ExecutionRecordError::EvidenceActionMismatch)
                );
                assert!(record.evidence.is_empty());
            }

            #[test]
            fn completion_rejects_non_terminal_outcome() {
                let mut record = record();
                assert_eq!(
                    record.complete(20, ExecutionOutcome::Started),
                    Err(ExecutionRecordError::InvalidCompletionOutcome)
                );
                assert!(!record.is_complete());
            }

            #[test]
            fn completion_rejects_timestamp_before_start() {
                let mut record = record();
                assert_eq!(
                    record.complete(9, ExecutionOutcome::Failed),
                    Err(ExecutionRecordError::InvalidCompletionTimestamp)
                );
                assert!(!record.is_complete());
            }

            #[test]
            fn completed_record_cannot_be_completed_twice() {
                let mut record = record();
                record
                    .complete(20, ExecutionOutcome::Succeeded)
                    .expect("completion");
                assert_eq!(
                    record.complete(30, ExecutionOutcome::Failed),
                    Err(ExecutionRecordError::AlreadyCompleted)
                );
                assert_eq!(record.completed_at_unix, Some(20));
                assert_eq!(record.outcome, ExecutionOutcome::Succeeded);
            }
        }
    }
    pub mod execution_record_store {
        use std::path::Path;
        use std::sync::{Arc, Mutex};

        use std::cmp::Ordering;
        use thiserror::Error;

        use crate::topstar::{
            ActionId, ActorId, ExecutionOutcome, ExecutionRecord, ExecutionRecordId, Journal,
            PersistenceError,
        };

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct ExecutionRecordCursor {
            pub started_at_unix: i64,
            pub record_id: ExecutionRecordId,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct ExecutionRecordPage {
            pub records: Vec<ExecutionRecord>,
            pub next_cursor: Option<ExecutionRecordCursor>,
            pub has_more: bool,
        }

        impl ExecutionRecordPage {
            pub fn empty() -> Self {
                Self {
                    records: Vec::new(),
                    next_cursor: None,
                    has_more: false,
                }
            }
        }

        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct ExecutionRecordQuery {
            pub actor: Option<ActorId>,
            pub action: Option<ActionId>,
            pub record: Option<ExecutionRecordId>,
            pub outcome: Option<ExecutionOutcome>,
            pub from_unix: Option<i64>,
            pub to_unix: Option<i64>,
            pub completed_from_unix: Option<i64>,
            pub completed_to_unix: Option<i64>,
        }

        impl ExecutionRecordCursor {
            pub fn new(record: &ExecutionRecord) -> Self {
                Self {
                    started_at_unix: record.started_at_unix,
                    record_id: record.id,
                }
            }
        }

        impl ExecutionRecordQuery {
            pub fn validate(&self) -> Result<(), ExecutionRecordQueryError> {
                if let (Some(from), Some(to)) = (self.from_unix, self.to_unix) {
                    if from > to {
                        return Err(ExecutionRecordQueryError::InvalidStartTimeRange { from, to });
                    }
                }
                if let (Some(from), Some(to)) = (self.completed_from_unix, self.completed_to_unix) {
                    if from > to {
                        return Err(ExecutionRecordQueryError::InvalidCompletionTimeRange {
                            from,
                            to,
                        });
                    }
                }
                Ok(())
            }

            fn matches(&self, record: &ExecutionRecord) -> bool {
                self.actor.is_none_or(|actor| actor == record.actor)
                    && self.action.is_none_or(|action| action == record.action)
                    && self.record.is_none_or(|id| id == record.id)
                    && self
                        .outcome
                        .as_ref()
                        .is_none_or(|outcome| outcome == &record.outcome)
                    && self
                        .from_unix
                        .is_none_or(|from| record.started_at_unix >= from)
                    && self.to_unix.is_none_or(|to| record.started_at_unix <= to)
                    && self.completed_from_unix.is_none_or(|from| {
                        record.completed_at_unix.is_some_and(|value| value >= from)
                    })
                    && self
                        .completed_to_unix
                        .is_none_or(|to| record.completed_at_unix.is_some_and(|value| value <= to))
            }
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum ExecutionRecordQueryError {
            #[error(
                "execution record query has an invalid start time range: from {from} is after to {to}"
            )]
            InvalidStartTimeRange { from: i64, to: i64 },
            #[error(
                "execution record query has an invalid completion time range: from {from} is after to {to}"
            )]
            InvalidCompletionTimeRange { from: i64, to: i64 },
        }

        #[derive(Debug, Error)]
        pub enum ExecutionRecordStoreError {
            #[error("execution record journal error: {0}")]
            Persistence(#[from] PersistenceError),
            #[error("execution record {0} already exists")]
            Duplicate(ExecutionRecordId),
            #[error("execution record {0} is not complete")]
            Incomplete(ExecutionRecordId),
        }

        #[derive(Debug, Clone)]
        pub struct ExecutionRecordStore {
            records: Arc<Mutex<Vec<ExecutionRecord>>>,
            journal: Arc<Mutex<Journal>>,
        }

        impl ExecutionRecordStore {
            pub fn open(path: impl AsRef<Path>) -> Result<Self, ExecutionRecordStoreError> {
                let journal = Journal::open(path)?;
                let records = journal
                    .read_all::<ExecutionRecord>()?
                    .into_iter()
                    .map(|record| record.payload)
                    .map(|record| {
                        if record.is_complete() {
                            Ok(record)
                        } else {
                            Err(ExecutionRecordStoreError::Incomplete(record.id))
                        }
                    })
                    .collect::<Result<Vec<_>, ExecutionRecordStoreError>>()?;

                Ok(Self {
                    records: Arc::new(Mutex::new(records)),
                    journal: Arc::new(Mutex::new(journal)),
                })
            }

            pub fn append(&self, record: ExecutionRecord) -> Result<(), ExecutionRecordStoreError> {
                let mut records = self.records.lock().expect("execution record lock poisoned");
                if records.iter().any(|existing| existing.id == record.id) {
                    return Err(ExecutionRecordStoreError::Duplicate(record.id));
                }
                if !record.is_complete() {
                    return Err(ExecutionRecordStoreError::Incomplete(record.id));
                }

                self.journal
                    .lock()
                    .expect("execution record journal lock poisoned")
                    .append(&record)?;

                records.push(record);
                Ok(())
            }

            pub fn get(&self, id: ExecutionRecordId) -> Option<ExecutionRecord> {
                self.records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .find(|record| record.id == id)
                    .cloned()
            }

            pub fn try_query(
                &self,
                query: &ExecutionRecordQuery,
            ) -> Result<Vec<ExecutionRecord>, ExecutionRecordQueryError> {
                query.validate()?;
                Ok(self.query_page_unchecked(query, 0, usize::MAX))
            }

            pub fn query(&self, query: &ExecutionRecordQuery) -> Vec<ExecutionRecord> {
                self.try_query(query).unwrap_or_default()
            }

            pub fn try_query_cursor(
                &self,
                query: &ExecutionRecordQuery,
                cursor: Option<ExecutionRecordCursor>,
                limit: usize,
            ) -> Result<Vec<ExecutionRecord>, ExecutionRecordQueryError> {
                query.validate()?;
                if limit == 0 {
                    return Ok(Vec::new());
                }
                Ok(self.query_cursor_unchecked(query, cursor, limit))
            }

            pub fn query_cursor(
                &self,
                query: &ExecutionRecordQuery,
                cursor: Option<ExecutionRecordCursor>,
                limit: usize,
            ) -> Vec<ExecutionRecord> {
                self.try_query_cursor(query, cursor, limit)
                    .unwrap_or_default()
            }

            fn query_cursor_unchecked(
                &self,
                query: &ExecutionRecordQuery,
                cursor: Option<ExecutionRecordCursor>,
                limit: usize,
            ) -> Vec<ExecutionRecord> {
                let mut matches = self
                    .records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .filter(|record| query.matches(record))
                    .filter(|record| {
                        cursor.is_none_or(|cursor| {
                            (record.started_at_unix, record.id)
                                > (cursor.started_at_unix, cursor.record_id)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                matches.sort_by(|a, b| match a.started_at_unix.cmp(&b.started_at_unix) {
                    Ordering::Equal => a.id.cmp(&b.id),
                    ordering => ordering,
                });
                matches.truncate(limit);
                matches
            }

            pub fn try_query_cursor_page(
                &self,
                query: &ExecutionRecordQuery,
                cursor: Option<ExecutionRecordCursor>,
                limit: usize,
            ) -> Result<ExecutionRecordPage, ExecutionRecordQueryError> {
                query.validate()?;
                if limit == 0 {
                    return Ok(ExecutionRecordPage::empty());
                }
                Ok(self.query_cursor_page_unchecked(query, cursor, limit))
            }

            pub fn query_cursor_page(
                &self,
                query: &ExecutionRecordQuery,
                cursor: Option<ExecutionRecordCursor>,
                limit: usize,
            ) -> ExecutionRecordPage {
                self.try_query_cursor_page(query, cursor, limit)
                    .unwrap_or_else(|_| ExecutionRecordPage::empty())
            }

            fn query_cursor_page_unchecked(
                &self,
                query: &ExecutionRecordQuery,
                cursor: Option<ExecutionRecordCursor>,
                limit: usize,
            ) -> ExecutionRecordPage {
                let mut matches = self
                    .records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .filter(|record| query.matches(record))
                    .filter(|record| {
                        cursor.is_none_or(|cursor| {
                            (record.started_at_unix, record.id)
                                > (cursor.started_at_unix, cursor.record_id)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                matches.sort_by(|a, b| match a.started_at_unix.cmp(&b.started_at_unix) {
                    Ordering::Equal => a.id.cmp(&b.id),
                    ordering => ordering,
                });
                let has_more = matches.len() > limit;
                matches.truncate(limit);
                let next_cursor = if has_more {
                    matches.last().map(ExecutionRecordCursor::new)
                } else {
                    None
                };
                ExecutionRecordPage {
                    records: matches,
                    next_cursor,
                    has_more,
                }
            }

            pub fn try_query_page(
                &self,
                query: &ExecutionRecordQuery,
                offset: usize,
                limit: usize,
            ) -> Result<Vec<ExecutionRecord>, ExecutionRecordQueryError> {
                query.validate()?;
                if limit == 0 {
                    return Ok(Vec::new());
                }
                Ok(self.query_page_unchecked(query, offset, limit))
            }

            pub fn query_page(
                &self,
                query: &ExecutionRecordQuery,
                offset: usize,
                limit: usize,
            ) -> Vec<ExecutionRecord> {
                self.try_query_page(query, offset, limit)
                    .unwrap_or_default()
            }

            fn query_page_unchecked(
                &self,
                query: &ExecutionRecordQuery,
                offset: usize,
                limit: usize,
            ) -> Vec<ExecutionRecord> {
                let mut matches = self
                    .records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .filter(|record| query.matches(record))
                    .cloned()
                    .collect::<Vec<_>>();
                matches.sort_by(|a, b| match a.started_at_unix.cmp(&b.started_at_unix) {
                    Ordering::Equal => a.id.cmp(&b.id),
                    ordering => ordering,
                });
                matches.into_iter().skip(offset).take(limit).collect()
            }

            pub fn all(&self) -> Vec<ExecutionRecord> {
                self.records
                    .lock()
                    .expect("execution record lock poisoned")
                    .clone()
            }

            pub fn len(&self) -> usize {
                self.records
                    .lock()
                    .expect("execution record lock poisoned")
                    .len()
            }

            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::{ExecutionOutcome, ResponsibilityContractId};
            use std::time::{SystemTime, UNIX_EPOCH};
            use uuid::Uuid;

            fn temporary_path() -> std::path::PathBuf {
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock should be valid")
                    .as_nanos();
                std::env::temp_dir().join(format!("topstar-execution-records-{nonce}.log"))
            }

            fn record(actor: ActorId, action: ActionId) -> ExecutionRecord {
                ExecutionRecord::started(
                    actor,
                    action,
                    Uuid::new_v4(),
                    ResponsibilityContractId::new_v4(),
                    10,
                )
            }

            #[test]
            fn append_and_recover_records() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let mut stored = record(actor, action);
                stored
                    .complete(20, ExecutionOutcome::Succeeded)
                    .expect("completion should succeed");

                {
                    let store = ExecutionRecordStore::open(&path).expect("store should open");
                    store.append(stored.clone()).expect("append should succeed");
                    assert_eq!(store.get(stored.id), Some(stored.clone()));
                }

                let recovered = ExecutionRecordStore::open(&path).expect("store should recover");
                assert_eq!(recovered.all(), vec![stored]);

                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn incomplete_records_are_rejected() {
                let path = temporary_path();
                let store = ExecutionRecordStore::open(&path).expect("store should open");
                let stored = record(ActorId::new_v4(), ActionId::new_v4());

                assert!(matches!(
                    store.append(stored.clone()),
                    Err(ExecutionRecordStoreError::Incomplete(id)) if id == stored.id
                ));
                assert!(store.is_empty());
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn duplicate_record_ids_are_rejected() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let mut stored = record(actor, action);
                stored
                    .complete(20, ExecutionOutcome::Succeeded)
                    .expect("completion should succeed");

                let store = ExecutionRecordStore::open(&path).expect("store should open");
                store
                    .append(stored.clone())
                    .expect("first append should succeed");

                assert!(matches!(
                    store.append(stored.clone()),
                    Err(ExecutionRecordStoreError::Duplicate(id)) if id == stored.id
                ));
                assert_eq!(store.len(), 1);

                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_filters_by_actor_and_action() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let other_actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let other_action = ActionId::new_v4();

                let store = ExecutionRecordStore::open(&path).expect("store should open");
                for (actor, action) in [
                    (actor, action),
                    (actor, other_action),
                    (other_actor, action),
                ] {
                    let mut stored = record(actor, action);
                    stored
                        .complete(20, ExecutionOutcome::Succeeded)
                        .expect("completion should succeed");
                    store.append(stored).expect("append should succeed");
                }

                assert_eq!(
                    store
                        .query(&ExecutionRecordQuery {
                            actor: Some(actor),
                            action: Some(action),
                            record: None,
                            ..Default::default()
                        })
                        .len(),
                    1
                );

                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_filters_by_start_time_range() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let mut early = record(actor, action);
                early.started_at_unix = 10;
                early
                    .complete(11, ExecutionOutcome::Succeeded)
                    .expect("completion");
                let mut late = record(actor, action);
                late.started_at_unix = 30;
                late.complete(31, ExecutionOutcome::Succeeded)
                    .expect("completion");

                let store = ExecutionRecordStore::open(&path).expect("store should open");
                store.append(early).expect("first append");
                store.append(late).expect("second append");

                let results = store.query(&ExecutionRecordQuery {
                    actor: Some(actor),
                    action: None,
                    record: None,
                    outcome: None,
                    from_unix: Some(20),
                    to_unix: Some(40),
                    completed_from_unix: None,
                    completed_to_unix: None,
                });
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].started_at_unix, 30);
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_supports_open_ended_time_ranges() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let mut stored = record(actor, action);
                stored.started_at_unix = 50;
                stored
                    .complete(50, ExecutionOutcome::Succeeded)
                    .expect("completion");

                let store = ExecutionRecordStore::open(&path).expect("store should open");
                store.append(stored).expect("append");
                assert_eq!(
                    store
                        .query(&ExecutionRecordQuery {
                            actor: Some(actor),
                            action: None,
                            record: None,
                            outcome: None,
                            from_unix: Some(50),
                            to_unix: None,
                            completed_from_unix: None,
                            completed_to_unix: None,
                        })
                        .len(),
                    1
                );
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_page_is_deterministic_and_bounded() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let store = ExecutionRecordStore::open(&path).expect("store should open");

                for timestamp in [10_i64, 20, 30, 40] {
                    let mut stored = record(actor, action);
                    stored.started_at_unix = timestamp;
                    stored
                        .complete(timestamp, ExecutionOutcome::Succeeded)
                        .expect("completion");
                    store.append(stored).expect("append");
                }

                let query = ExecutionRecordQuery {
                    actor: Some(actor),
                    action: Some(action),
                    record: None,
                    outcome: None,
                    from_unix: None,
                    to_unix: None,
                    completed_from_unix: None,
                    completed_to_unix: None,
                };
                let page = store.query_page(&query, 1, 2);
                assert_eq!(page.len(), 2);
                assert_eq!(page[0].started_at_unix, 20);
                assert_eq!(page[1].started_at_unix, 30);
                assert!(store.query_page(&query, 0, 0).is_empty());
                assert!(store.query_page(&query, 10, 2).is_empty());

                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn cursor_query_is_stable_across_appends() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let store = ExecutionRecordStore::open(&path).expect("store should open");

                for timestamp in [10_i64, 20, 30] {
                    let mut stored = record(actor, action);
                    stored.started_at_unix = timestamp;
                    stored
                        .complete(timestamp, ExecutionOutcome::Succeeded)
                        .expect("completion");
                    store.append(stored).expect("append");
                }

                let query = ExecutionRecordQuery {
                    actor: Some(actor),
                    action: Some(action),
                    record: None,
                    outcome: None,
                    from_unix: None,
                    to_unix: None,
                    completed_from_unix: None,
                    completed_to_unix: None,
                };
                let first = store.query_cursor(&query, None, 1);
                let cursor = ExecutionRecordCursor::new(&first[0]);

                let mut appended = record(actor, action);
                appended.started_at_unix = 40;
                appended
                    .complete(40, ExecutionOutcome::Succeeded)
                    .expect("completion");
                store.append(appended).expect("append");

                let second = store.query_cursor(&query, Some(cursor), 10);
                assert_eq!(
                    second.iter().map(|r| r.started_at_unix).collect::<Vec<_>>(),
                    vec![20, 30, 40]
                );
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn cursor_query_orders_by_timestamp_then_id() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let store = ExecutionRecordStore::open(&path).expect("store should open");

                let mut first = record(actor, action);
                first.started_at_unix = 20;
                first
                    .complete(20, ExecutionOutcome::Succeeded)
                    .expect("completion");
                let mut second = record(actor, action);
                second.started_at_unix = 20;
                second
                    .complete(20, ExecutionOutcome::Succeeded)
                    .expect("completion");
                let expected = if first.id < second.id {
                    vec![first.id, second.id]
                } else {
                    vec![second.id, first.id]
                };

                store.append(first).expect("append");
                store.append(second).expect("append");

                let query = ExecutionRecordQuery {
                    actor: Some(actor),
                    action: Some(action),
                    record: None,
                    outcome: None,
                    from_unix: None,
                    to_unix: None,
                    completed_from_unix: None,
                    completed_to_unix: None,
                };
                let ids = store
                    .query_cursor(&query, None, 10)
                    .into_iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>();
                assert_eq!(ids, expected);
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_filters_by_outcome() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let store = ExecutionRecordStore::open(&path).expect("store should open");

                for (timestamp, outcome) in [
                    (10_i64, ExecutionOutcome::Succeeded),
                    (20, ExecutionOutcome::Failed),
                    (30, ExecutionOutcome::Cancelled),
                ] {
                    let mut stored = record(actor, action);
                    stored.started_at_unix = timestamp;
                    stored.complete(timestamp, outcome).expect("completion");
                    store.append(stored).expect("append");
                }

                let results = store.query(&ExecutionRecordQuery {
                    actor: Some(actor),
                    action: Some(action),
                    record: None,
                    outcome: Some(ExecutionOutcome::Failed),
                    from_unix: None,
                    to_unix: None,
                    completed_from_unix: None,
                    completed_to_unix: None,
                });
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].outcome, ExecutionOutcome::Failed);
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_filters_by_completion_time() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let store = ExecutionRecordStore::open(&path).expect("store should open");

                for (started, completed) in [(10_i64, 15_i64), (20, 25), (30, 35)] {
                    let mut stored = record(actor, action);
                    stored.started_at_unix = started;
                    stored
                        .complete(completed, ExecutionOutcome::Succeeded)
                        .expect("completion");
                    store.append(stored).expect("append");
                }

                let results = store.query(&ExecutionRecordQuery {
                    actor: Some(actor),
                    action: Some(action),
                    record: None,
                    outcome: Some(ExecutionOutcome::Succeeded),
                    from_unix: None,
                    to_unix: None,
                    completed_from_unix: Some(20),
                    completed_to_unix: Some(30),
                });
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].completed_at_unix, Some(25));
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn try_query_surfaces_invalid_start_time_range() {
                let path = temporary_path();
                let store = ExecutionRecordStore::open(&path).expect("store should open");
                let query = ExecutionRecordQuery {
                    from_unix: Some(20),
                    to_unix: Some(10),
                    ..ExecutionRecordQuery::default()
                };

                assert_eq!(
                    store.try_query(&query),
                    Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
                );
                assert_eq!(
                    store.try_query_page(&query, 0, 10),
                    Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
                );
                assert_eq!(
                    store.try_query_cursor(&query, None, 10),
                    Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
                );
                assert_eq!(
                    store.try_query_cursor_page(&query, None, 10),
                    Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
                );
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_rejects_invalid_time_range() {
                let query = ExecutionRecordQuery {
                    actor: None,
                    action: None,
                    record: None,
                    outcome: None,
                    from_unix: Some(20),
                    to_unix: Some(10),
                    completed_from_unix: None,
                    completed_to_unix: None,
                };
                assert_eq!(
                    query.validate(),
                    Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
                );

                let path = temporary_path();
                let store = ExecutionRecordStore::open(&path).expect("store should open");
                assert!(store.query(&query).is_empty());
                assert!(store.query_page(&query, 0, 10).is_empty());
                assert!(store.query_cursor(&query, None, 10).is_empty());
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn query_rejects_invalid_completion_time_range() {
                let query = ExecutionRecordQuery {
                    actor: None,
                    action: None,
                    record: None,
                    outcome: None,
                    from_unix: None,
                    to_unix: None,
                    completed_from_unix: Some(20),
                    completed_to_unix: Some(10),
                };
                assert_eq!(
                    query.validate(),
                    Err(ExecutionRecordQueryError::InvalidCompletionTimeRange { from: 20, to: 10 })
                );

                let path = temporary_path();
                let store = ExecutionRecordStore::open(&path).expect("store should open");
                assert!(store.query_cursor(&query, None, 10).is_empty());
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn cursor_page_reports_continuation_metadata() {
                let path = temporary_path();
                let actor = ActorId::new_v4();
                let action = ActionId::new_v4();
                let store = ExecutionRecordStore::open(&path).expect("store should open");
                for timestamp in [10_i64, 20, 30] {
                    let mut stored = record(actor, action);
                    stored.started_at_unix = timestamp;
                    stored
                        .complete(timestamp, ExecutionOutcome::Succeeded)
                        .expect("completion");
                    store.append(stored).expect("append");
                }
                let query = ExecutionRecordQuery {
                    actor: Some(actor),
                    action: Some(action),
                    record: None,
                    outcome: None,
                    from_unix: None,
                    to_unix: None,
                    completed_from_unix: None,
                    completed_to_unix: None,
                };
                let page = store.query_cursor_page(&query, None, 2);
                assert_eq!(page.records.len(), 2);
                assert!(page.has_more);
                assert_eq!(page.next_cursor.map(|c| c.started_at_unix), Some(20));
                let next = store.query_cursor_page(&query, page.next_cursor, 2);
                assert_eq!(next.records.len(), 1);
                assert!(!next.has_more);
                assert!(next.next_cursor.is_none());
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn store_preserves_terminal_records() {
                let path = temporary_path();
                let mut stored = record(ActorId::new_v4(), ActionId::new_v4());
                stored
                    .complete(20, ExecutionOutcome::Succeeded)
                    .expect("completion should succeed");

                let store = ExecutionRecordStore::open(&path).expect("store should open");
                store.append(stored.clone()).expect("append should succeed");

                assert_eq!(
                    store.get(stored.id).expect("record should exist").outcome,
                    ExecutionOutcome::Succeeded
                );

                std::fs::remove_file(path).expect("temporary store should be removed");
            }
        }
    }
    pub mod executor {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};

        use thiserror::Error;

        use crate::topstar::{
            authorize, Action, ActionResult, ActionState, AuditOutcome, AuthorizationDecision,
            CapabilityRegistry, ExecutionAuditLog, ExecutionOutcome, ExecutionRecord,
            ExecutionRecordStore, ExecutionRecordStoreError, Identity, Policy,
            ResponsibilityContract, RevocationRegistry,
        };

        const MAX_RECENT_EXECUTION_RECORDS: usize = 4096;

        pub trait ActionHandler: Send + Sync {
            fn execute(&self, action: &Action) -> Result<serde_json::Value, String>;
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum ExecutionError {
            #[error("unknown capability")]
            UnknownCapability,
            #[error("execution authority has been revoked")]
            Revoked,
            #[error("authorization denied: {0}")]
            AuthorizationDenied(String),
            #[error("invalid action state")]
            InvalidState,
            #[error("execution failed: {0}")]
            HandlerFailed(String),
            #[error("audit logging failed: {0}")]
            Audit(String),
            #[error("execution provenance persistence failed: {0}")]
            Provenance(String),
        }

        #[derive(Debug, Default)]
        pub struct ExecutionEngine {
            capabilities: CapabilityRegistry,
            revocations: RevocationRegistry,
            audit: ExecutionAuditLog,
            recent_records: Arc<Mutex<VecDeque<ExecutionRecord>>>,
            record_store: Option<ExecutionRecordStore>,
        }

        impl ExecutionEngine {
            pub fn new() -> Self {
                Self::default()
            }

            pub fn with_record_store(record_store: ExecutionRecordStore) -> Self {
                Self {
                    record_store: Some(record_store),
                    ..Self::default()
                }
            }

            pub fn record_store(&self) -> Option<&ExecutionRecordStore> {
                self.record_store.as_ref()
            }

            pub fn capabilities(&self) -> &CapabilityRegistry {
                &self.capabilities
            }

            pub fn capabilities_mut(&mut self) -> &mut CapabilityRegistry {
                &mut self.capabilities
            }

            pub fn revocations(&self) -> &RevocationRegistry {
                &self.revocations
            }

            pub fn revocations_mut(&mut self) -> &mut RevocationRegistry {
                &mut self.revocations
            }

            pub fn audit(&self) -> &ExecutionAuditLog {
                &self.audit
            }

            pub fn recent_execution_records(&self) -> Vec<ExecutionRecord> {
                self.recent_records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .cloned()
                    .collect()
            }

            pub fn execution_record(
                &self,
                id: crate::topstar::ExecutionRecordId,
            ) -> Option<ExecutionRecord> {
                self.recent_records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .find(|record| record.id == id)
                    .cloned()
            }

            pub fn execution_records_for_action(
                &self,
                action: crate::topstar::ActionId,
            ) -> Vec<ExecutionRecord> {
                self.recent_records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .filter(|record| record.action == action)
                    .cloned()
                    .collect()
            }

            pub fn execution_records_for_actor(
                &self,
                actor: crate::topstar::ActorId,
            ) -> Vec<ExecutionRecord> {
                self.recent_records
                    .lock()
                    .expect("execution record lock poisoned")
                    .iter()
                    .filter(|record| record.actor == actor)
                    .cloned()
                    .collect()
            }

            pub const MAX_RECENT_EXECUTION_RECORDS: usize = MAX_RECENT_EXECUTION_RECORDS;

            fn retain_execution_record(&self, record: ExecutionRecord) {
                let mut records = self
                    .recent_records
                    .lock()
                    .expect("execution record lock poisoned");
                if MAX_RECENT_EXECUTION_RECORDS == 0 {
                    return;
                }
                if records.len() == MAX_RECENT_EXECUTION_RECORDS {
                    records.pop_front();
                }
                records.push_back(record);
            }

            pub fn execute<H: ActionHandler>(
                &self,
                mut action: Action,
                identity: &Identity,
                responsibility: &ResponsibilityContract,
                policy: &Policy,
                now_unix: i64,
                handler: &H,
            ) -> Result<ActionResult, ExecutionError> {
                if action.state != ActionState::Proposed {
                    self.audit
                        .record(
                            identity.id,
                            Some(action.id),
                            now_unix,
                            AuditOutcome::Denied,
                            "invalid action state",
                        )
                        .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                    return Err(ExecutionError::InvalidState);
                }
                if !self.capabilities.contains(action.capability) {
                    self.audit
                        .record(
                            identity.id,
                            Some(action.id),
                            now_unix,
                            AuditOutcome::Denied,
                            "unknown capability",
                        )
                        .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                    return Err(ExecutionError::UnknownCapability);
                }

                if responsibility.revoked
                    || self.revocations.is_capability_revoked(action.capability)
                    || self
                        .revocations
                        .is_responsibility_revoked(responsibility.id)
                    || self.revocations.is_action_revoked(action.id)
                {
                    self.audit
                        .record(
                            identity.id,
                            Some(action.id),
                            now_unix,
                            AuditOutcome::Revoked,
                            "execution authority revoked",
                        )
                        .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                    return Err(ExecutionError::Revoked);
                }

                match authorize(
                    identity,
                    action.capability,
                    responsibility,
                    policy,
                    now_unix,
                ) {
                    AuthorizationDecision::Allow => {}
                    AuthorizationDecision::Deny(reason) => {
                        self.audit
                            .record(
                                identity.id,
                                Some(action.id),
                                now_unix,
                                AuditOutcome::Denied,
                                format!("authorization denied: {reason}"),
                            )
                            .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                        return Err(ExecutionError::AuthorizationDenied(reason));
                    }
                }

                action.state = ActionState::Authorized;
                self.audit
                    .record(
                        identity.id,
                        Some(action.id),
                        now_unix,
                        AuditOutcome::Allowed,
                        "execution authorized",
                    )
                    .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                action.state = ActionState::Executing;
                self.audit
                    .record(
                        identity.id,
                        Some(action.id),
                        now_unix,
                        AuditOutcome::Started,
                        "execution started",
                    )
                    .map_err(|e| ExecutionError::Audit(e.to_string()))?;

                let mut record = ExecutionRecord::started(
                    identity.id,
                    action.id,
                    action.capability,
                    responsibility.id,
                    now_unix,
                );

                let output = match handler.execute(&action) {
                    Ok(output) => output,
                    Err(error) => {
                        action.state = ActionState::Failed;
                        record
                            .complete(now_unix, ExecutionOutcome::Failed)
                            .expect("execution record completion must be valid");
                        self.audit
                            .record(
                                identity.id,
                                Some(action.id),
                                now_unix,
                                AuditOutcome::Failed,
                                format!("handler failed: {error}"),
                            )
                            .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                        if let Some(store) = &self.record_store {
                            store.append(record.clone()).map_err(
                                |e: ExecutionRecordStoreError| {
                                    ExecutionError::Provenance(e.to_string())
                                },
                            )?;
                        }
                        self.retain_execution_record(record);
                        return Err(ExecutionError::HandlerFailed(error));
                    }
                };

                action.state = ActionState::Succeeded;
                record
                    .complete(now_unix, ExecutionOutcome::Succeeded)
                    .expect("execution record completion must be valid");
                self.audit
                    .record(
                        identity.id,
                        Some(action.id),
                        now_unix,
                        AuditOutcome::Succeeded,
                        "execution succeeded",
                    )
                    .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                if let Some(store) = &self.record_store {
                    store
                        .append(record.clone())
                        .map_err(|e: ExecutionRecordStoreError| {
                            ExecutionError::Provenance(e.to_string())
                        })?;
                }
                self.retain_execution_record(record);
                Ok(ActionResult::success(action.id, output))
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::{Capability, CapabilityRisk, ResponsibilityContract};

            struct Handler;

            impl ActionHandler for Handler {
                fn execute(&self, _action: &Action) -> Result<serde_json::Value, String> {
                    Ok(serde_json::json!({"executed": true}))
                }
            }

            struct FailingHandler;

            impl ActionHandler for FailingHandler {
                fn execute(&self, _action: &Action) -> Result<serde_json::Value, String> {
                    Err("side effect failed".into())
                }
            }

            fn policy_for(r: &ResponsibilityContract) -> Policy {
                let mut policy = Policy::new("test");
                policy.allowed_capabilities = r.allowed_capabilities.clone();
                policy
            }

            fn fixture() -> (Identity, ResponsibilityContract, ExecutionEngine, Action) {
                fixture_with(ExecutionEngine::new())
            }

            fn fixture_with(
                mut engine: ExecutionEngine,
            ) -> (Identity, ResponsibilityContract, ExecutionEngine, Action) {
                let identity = Identity::new("executor");
                let capability =
                    Capability::new("test.execute", "test execution", CapabilityRisk::Low);
                let mut responsibility = ResponsibilityContract::new("test goal", identity.id);
                responsibility.allowed_capabilities = vec![capability.id];
                responsibility.expires_at_unix = Some(i64::MAX);
                engine
                    .capabilities_mut()
                    .register(capability)
                    .expect("register");
                let action = Action::new(
                    identity.id,
                    responsibility.allowed_capabilities[0],
                    responsibility.id,
                    "execute test",
                );
                (identity, responsibility, engine, action)
            }

            #[test]
            fn revoked_responsibility_blocks_execution() {
                let (identity, mut responsibility, engine, action) = fixture();
                responsibility.revoked = true;

                let error = engine
                    .execute(
                        action,
                        &identity,
                        &responsibility,
                        &policy_for(&responsibility),
                        0,
                        &Handler,
                    )
                    .expect_err("revocation must block execution");
                assert_eq!(error, ExecutionError::Revoked);
                assert!(engine.recent_execution_records().is_empty());
            }

            #[test]
            fn centrally_revoked_action_blocks_execution() {
                let (identity, responsibility, mut engine, action) = fixture();
                engine.revocations_mut().revoke_action(action.id);

                let error = engine
                    .execute(
                        action,
                        &identity,
                        &responsibility,
                        &policy_for(&responsibility),
                        0,
                        &Handler,
                    )
                    .expect_err("revoked action must be blocked");
                assert_eq!(error, ExecutionError::Revoked);
                assert!(engine.recent_execution_records().is_empty());
            }

            #[test]
            fn successful_execution_creates_completed_execution_record() {
                let (identity, responsibility, engine, action) = fixture();

                let result = engine
                    .execute(
                        action,
                        &identity,
                        &responsibility,
                        &policy_for(&responsibility),
                        10,
                        &Handler,
                    )
                    .expect("execution");

                assert_eq!(result.status, crate::topstar::ResultStatus::Success);
                assert_eq!(engine.audit().len(), 3);

                let records = engine.recent_execution_records();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].actor, identity.id);
                assert_eq!(records[0].action, result.action);
                assert_eq!(
                    records[0].capability,
                    responsibility.allowed_capabilities[0]
                );
                assert_eq!(records[0].responsibility, responsibility.id);
                assert_eq!(records[0].started_at_unix, 10);
                assert_eq!(records[0].completed_at_unix, Some(10));
                assert_eq!(records[0].outcome, ExecutionOutcome::Succeeded);
                assert_eq!(
                    engine.execution_record(records[0].id),
                    Some(records[0].clone())
                );
                assert_eq!(
                    engine.execution_records_for_action(records[0].action).len(),
                    1
                );
                assert_eq!(
                    engine.execution_records_for_actor(records[0].actor).len(),
                    1
                );
                assert!(engine
                    .execution_record(crate::topstar::ExecutionRecordId::new_v4())
                    .is_none());
            }

            #[test]
            fn recent_execution_records_are_bounded_and_evict_oldest() {
                let (identity, responsibility, engine, _) = fixture();
                let mut ids = Vec::with_capacity(MAX_RECENT_EXECUTION_RECORDS + 1);

                for index in 0..=MAX_RECENT_EXECUTION_RECORDS {
                    let action = Action::new(
                        identity.id,
                        responsibility.allowed_capabilities[0],
                        responsibility.id,
                        format!("execute {index}"),
                    );
                    engine
                        .execute(
                            action,
                            &identity,
                            &responsibility,
                            &policy_for(&responsibility),
                            index as i64,
                            &Handler,
                        )
                        .expect("execution");
                    let records = engine.recent_execution_records();
                    ids.push(records.last().expect("record").id);
                }

                let records = engine.recent_execution_records();
                assert_eq!(records.len(), MAX_RECENT_EXECUTION_RECORDS);
                assert!(!records.iter().any(|record| record.id == ids[0]));
                assert_eq!(records.last().map(|record| record.id), ids.last().copied());
            }

            #[test]
            fn audit_terminal_event_precedes_provenance_retention() {
                let (identity, responsibility, engine, action) = fixture();
                let result = engine
                    .execute(
                        action,
                        &identity,
                        &responsibility,
                        &policy_for(&responsibility),
                        30,
                        &Handler,
                    )
                    .expect("execution");

                let audit = engine.audit().entries();
                let record = engine
                    .execution_record(engine.execution_records_for_action(result.action)[0].id)
                    .expect("record");
                assert_eq!(
                    audit.last().expect("audit entry").outcome,
                    AuditOutcome::Succeeded
                );
                assert_eq!(record.outcome, ExecutionOutcome::Succeeded);
            }

            #[test]
            fn durable_record_store_receives_successful_execution() {
                use std::time::{SystemTime, UNIX_EPOCH};

                let path = std::env::temp_dir().join(format!(
                    "topstar-executor-store-{}.log",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("clock")
                        .as_nanos()
                ));
                let store = ExecutionRecordStore::open(&path).expect("store");
                let (identity, responsibility, engine, action) =
                    fixture_with(ExecutionEngine::with_record_store(store.clone()));

                engine
                    .execute(
                        action,
                        &identity,
                        &responsibility,
                        &policy_for(&responsibility),
                        40,
                        &Handler,
                    )
                    .expect("execution");

                let records = store.all();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].outcome, ExecutionOutcome::Succeeded);
                std::fs::remove_file(path).expect("temporary store should be removed");
            }

            #[test]
            fn failed_handler_creates_failed_execution_record() {
                let (identity, responsibility, engine, action) = fixture();

                let error = engine
                    .execute(
                        action,
                        &identity,
                        &responsibility,
                        &policy_for(&responsibility),
                        20,
                        &FailingHandler,
                    )
                    .expect_err("handler failure must propagate");
                assert_eq!(
                    error,
                    ExecutionError::HandlerFailed("side effect failed".into())
                );

                let records = engine.recent_execution_records();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].outcome, ExecutionOutcome::Failed);
                assert_eq!(records[0].started_at_unix, 20);
                assert_eq!(records[0].completed_at_unix, Some(20));
            }
        }
    }
    pub mod identity {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        pub type ActorId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Identity {
            pub id: ActorId,
            pub name: String,
        }

        impl Identity {
            pub fn new(name: impl Into<String>) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    name: name.into(),
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn creates_unique_identity() {
                let first = Identity::new("human");
                let second = Identity::new("agent");
                assert_ne!(first.id, second.id);
            }
        }
    }
    pub mod intent {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::ActorId;

        pub type IntentId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Intent {
            pub id: IntentId,
            pub actor: ActorId,
            pub statement: String,
            pub context: serde_json::Value,
        }

        impl Intent {
            pub fn new(
                actor: ActorId,
                statement: impl Into<String>,
                context: serde_json::Value,
            ) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    actor,
                    statement: statement.into(),
                    context,
                }
            }
        }
    }
    pub mod lifecycle {
        use serde::{Deserialize, Serialize};

        use crate::topstar::ActionState;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Lifecycle {
            pub state: ActionState,
        }

        impl Lifecycle {
            pub fn new() -> Self {
                Self {
                    state: ActionState::Proposed,
                }
            }

            pub fn authorize(&mut self) -> bool {
                if self.state == ActionState::Proposed {
                    self.state = ActionState::Authorized;
                    true
                } else {
                    false
                }
            }

            pub fn start(&mut self) -> bool {
                if self.state == ActionState::Authorized {
                    self.state = ActionState::Executing;
                    true
                } else {
                    false
                }
            }

            pub fn succeed(&mut self) -> bool {
                if self.state == ActionState::Executing {
                    self.state = ActionState::Succeeded;
                    true
                } else {
                    false
                }
            }

            pub fn fail(&mut self) -> bool {
                if matches!(self.state, ActionState::Authorized | ActionState::Executing) {
                    self.state = ActionState::Failed;
                    true
                } else {
                    false
                }
            }

            pub fn cancel(&mut self) -> bool {
                if matches!(
                    self.state,
                    ActionState::Proposed | ActionState::Authorized | ActionState::Executing
                ) {
                    self.state = ActionState::Cancelled;
                    true
                } else {
                    false
                }
            }
        }

        impl Default for Lifecycle {
            fn default() -> Self {
                Self::new()
            }
        }
    }
    pub mod network {
        use serde::{Deserialize, Serialize};
        use std::collections::BTreeMap;
        use std::net::{IpAddr, SocketAddr};
        use std::str::FromStr;
        use thiserror::Error;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Endpoint {
            pub host: String,
            pub port: u16,
        }

        impl Endpoint {
            pub fn new(host: impl Into<String>, port: u16) -> Result<Self, NetworkError> {
                let host = host.into();
                if host.trim().is_empty() {
                    return Err(NetworkError::InvalidEndpoint("host is empty".into()));
                }
                if host.len() > 253 {
                    return Err(NetworkError::InvalidEndpoint("host is too long".into()));
                }
                Ok(Self { host, port })
            }

            pub fn socket_addr(&self) -> Result<SocketAddr, NetworkError> {
                self.to_socket_addr()
            }
        }

        impl FromStr for Endpoint {
            type Err = NetworkError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let addr: SocketAddr = value
                    .parse()
                    .map_err(|_| NetworkError::InvalidEndpoint(value.into()))?;
                Self::new(addr.ip().to_string(), addr.port())
            }
        }

        impl Endpoint {
            fn to_socket_addr(&self) -> Result<SocketAddr, NetworkError> {
                let ip: IpAddr = self
                    .host
                    .parse()
                    .map_err(|_| NetworkError::NonIpHost(self.host.clone()))?;
                Ok(SocketAddr::new(ip, self.port))
            }
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum Transport {
            Tcp,
            Quic,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct NetworkAddress {
            pub transport: Transport,
            pub endpoint: Endpoint,
            pub metadata: BTreeMap<String, String>,
        }

        impl NetworkAddress {
            pub fn new(transport: Transport, endpoint: Endpoint) -> Self {
                Self {
                    transport,
                    endpoint,
                    metadata: BTreeMap::new(),
                }
            }
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum NetworkError {
            #[error("invalid endpoint: {0}")]
            InvalidEndpoint(String),
            #[error("hostname is not a literal IP address: {0}")]
            NonIpHost(String),
            #[error("invalid network identity: {0}")]
            InvalidIdentity(String),
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct NetworkIdentity {
            pub node_id: uuid::Uuid,
            pub addresses: Vec<NetworkAddress>,
        }

        impl NetworkIdentity {
            pub fn new(
                node_id: uuid::Uuid,
                addresses: Vec<NetworkAddress>,
            ) -> Result<Self, NetworkError> {
                if addresses.is_empty() {
                    return Err(NetworkError::InvalidIdentity(
                        "at least one address is required".into(),
                    ));
                }
                Ok(Self { node_id, addresses })
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use uuid::Uuid;

            #[test]
            fn parses_literal_socket_endpoint() {
                let endpoint: Endpoint = "127.0.0.1:8443".parse().expect("valid endpoint");
                assert_eq!(endpoint.socket_addr().expect("socket address").port(), 8443);
            }

            #[test]
            fn rejects_empty_host() {
                assert!(matches!(
                    Endpoint::new("", 1),
                    Err(NetworkError::InvalidEndpoint(_))
                ));
            }

            #[test]
            fn network_identity_requires_address() {
                assert!(matches!(
                    NetworkIdentity::new(Uuid::new_v4(), vec![]),
                    Err(NetworkError::InvalidIdentity(_))
                ));
            }
        }
    }
    pub mod persistence {
        use std::fs::{File, OpenOptions};
        use std::io::{self, BufReader, Read, Seek, Write};
        use std::path::{Path, PathBuf};

        use serde::{de::DeserializeOwned, Deserialize, Serialize};
        use thiserror::Error;

        const MAGIC: &[u8; 8] = b"TOPJNL01";
        const HEADER_LEN: usize = 8 + 4;
        const MAX_RECORD_SIZE: u32 = 64 * 1024 * 1024;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct JournalRecord<T> {
            pub sequence: u64,
            pub payload: T,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct JournalPosition {
            pub sequence: u64,
            pub offset: u64,
        }

        #[derive(Debug, Error)]
        pub enum PersistenceError {
            #[error("I/O error: {0}")]
            Io(#[from] io::Error),
            #[error("serialization error: {0}")]
            Serialization(#[from] serde_json::Error),
            #[error("invalid journal header")]
            InvalidHeader,
            #[error("record size {0} exceeds maximum")]
            RecordTooLarge(u32),
            #[error("truncated journal record at byte offset {offset}")]
            TruncatedRecord { offset: u64 },
            #[error("invalid journal record at byte offset {offset}: {source}")]
            InvalidRecord {
                offset: u64,
                source: serde_json::Error,
            },
            #[error("non-monotonic sequence: expected {expected}, found {found}")]
            NonMonotonicSequence { expected: u64, found: u64 },
        }

        #[derive(Debug)]
        pub struct Journal {
            path: PathBuf,
            file: File,
            next_sequence: u64,
        }

        impl Journal {
            pub fn open(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
                let path = path.as_ref().to_path_buf();
                let mut file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .append(true)
                    .open(&path)?;

                let metadata = file.metadata()?;
                if metadata.len() == 0 {
                    file.write_all(MAGIC)?;
                    file.sync_data()?;
                } else {
                    if metadata.len() < MAGIC.len() as u64 {
                        return Err(PersistenceError::InvalidHeader);
                    }
                    let mut header = [0_u8; MAGIC.len()];
                    let mut reader = BufReader::new(file.try_clone()?);
                    reader.read_exact(&mut header)?;
                    if &header != MAGIC {
                        return Err(PersistenceError::InvalidHeader);
                    }
                }

                let next_sequence = Self::recover_next_sequence(&mut file)?;
                Ok(Self {
                    path,
                    file,
                    next_sequence,
                })
            }

            pub fn path(&self) -> &Path {
                &self.path
            }

            pub fn next_sequence(&self) -> u64 {
                self.next_sequence
            }

            pub fn append<T: Serialize>(
                &mut self,
                payload: &T,
            ) -> Result<JournalPosition, PersistenceError> {
                let record = JournalRecord {
                    sequence: self.next_sequence,
                    payload,
                };
                let bytes = serde_json::to_vec(&record)?;
                let length = u32::try_from(bytes.len())
                    .map_err(|_| PersistenceError::RecordTooLarge(u32::MAX))?;
                if length > MAX_RECORD_SIZE {
                    return Err(PersistenceError::RecordTooLarge(length));
                }

                let offset = self.file.metadata()?.len();
                self.file.write_all(&length.to_le_bytes())?;
                self.file.write_all(&bytes)?;
                self.file.sync_data()?;

                self.next_sequence = self
                    .next_sequence
                    .checked_add(1)
                    .expect("journal sequence exhausted");

                Ok(JournalPosition {
                    sequence: record.sequence,
                    offset,
                })
            }

            pub fn read_all<T: DeserializeOwned>(
                &self,
            ) -> Result<Vec<JournalRecord<T>>, PersistenceError> {
                let mut file = File::open(&self.path)?;
                let mut magic = [0_u8; MAGIC.len()];
                file.read_exact(&mut magic)?;
                if &magic != MAGIC {
                    return Err(PersistenceError::InvalidHeader);
                }

                let mut records: Vec<JournalRecord<T>> = Vec::new();
                let mut offset = HEADER_LEN as u64;

                loop {
                    let mut length_bytes = [0_u8; 4];
                    match file.read_exact(&mut length_bytes) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(error) => return Err(error.into()),
                    }

                    let length = u32::from_le_bytes(length_bytes);
                    if length > MAX_RECORD_SIZE {
                        return Err(PersistenceError::RecordTooLarge(length));
                    }

                    let mut bytes = vec![0_u8; length as usize];
                    if let Err(error) = file.read_exact(&mut bytes) {
                        if error.kind() == io::ErrorKind::UnexpectedEof {
                            return Err(PersistenceError::TruncatedRecord { offset });
                        }
                        return Err(error.into());
                    }

                    let record: JournalRecord<T> = serde_json::from_slice(&bytes)
                        .map_err(|source| PersistenceError::InvalidRecord { offset, source })?;

                    if let Some(previous) = records.last() {
                        let expected = previous.sequence.checked_add(1).ok_or(
                            PersistenceError::NonMonotonicSequence {
                                expected: u64::MAX,
                                found: record.sequence,
                            },
                        )?;
                        if record.sequence != expected {
                            return Err(PersistenceError::NonMonotonicSequence {
                                expected,
                                found: record.sequence,
                            });
                        }
                    } else if record.sequence != 0 {
                        return Err(PersistenceError::NonMonotonicSequence {
                            expected: 0,
                            found: record.sequence,
                        });
                    }

                    records.push(record);
                    offset = offset
                        .checked_add(4 + u64::from(length))
                        .ok_or(PersistenceError::TruncatedRecord { offset })?;
                }

                Ok(records)
            }

            fn recover_next_sequence(file: &mut File) -> Result<u64, PersistenceError> {
                let records = Self::read_records_from(file)?;
                match records.last() {
                    Some(record) => record.sequence.checked_add(1).ok_or(
                        PersistenceError::NonMonotonicSequence {
                            expected: u64::MAX,
                            found: u64::MAX,
                        },
                    ),
                    None => Ok(0),
                }
            }

            fn read_records_from(
                file: &mut File,
            ) -> Result<Vec<JournalRecord<serde_json::Value>>, PersistenceError> {
                file.rewind()?;

                let mut magic = [0_u8; MAGIC.len()];
                file.read_exact(&mut magic)?;
                if &magic != MAGIC {
                    return Err(PersistenceError::InvalidHeader);
                }

                let mut records: Vec<JournalRecord<serde_json::Value>> = Vec::new();
                let mut offset = HEADER_LEN as u64;

                loop {
                    let mut length_bytes = [0_u8; 4];
                    match file.read_exact(&mut length_bytes) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(error) => return Err(error.into()),
                    }

                    let length = u32::from_le_bytes(length_bytes);
                    if length > MAX_RECORD_SIZE {
                        return Err(PersistenceError::RecordTooLarge(length));
                    }

                    let mut bytes = vec![0_u8; length as usize];
                    if let Err(error) = file.read_exact(&mut bytes) {
                        if error.kind() == io::ErrorKind::UnexpectedEof {
                            return Err(PersistenceError::TruncatedRecord { offset });
                        }
                        return Err(error.into());
                    }

                    let record: JournalRecord<serde_json::Value> =
                        serde_json::from_slice(&bytes)
                            .map_err(|source| PersistenceError::InvalidRecord { offset, source })?;

                    if let Some(previous) = records.last() {
                        let expected = previous.sequence.checked_add(1).ok_or(
                            PersistenceError::NonMonotonicSequence {
                                expected: u64::MAX,
                                found: u64::MAX,
                            },
                        )?;
                        if record.sequence != expected {
                            return Err(PersistenceError::NonMonotonicSequence {
                                expected,
                                found: record.sequence,
                            });
                        }
                    } else if record.sequence != 0 {
                        return Err(PersistenceError::NonMonotonicSequence {
                            expected: 0,
                            found: record.sequence,
                        });
                    }

                    records.push(record);
                    offset = offset
                        .checked_add(4 + u64::from(length))
                        .ok_or(PersistenceError::TruncatedRecord { offset })?;
                }

                Ok(records)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use std::time::{SystemTime, UNIX_EPOCH};

            fn temporary_path() -> PathBuf {
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock should be valid")
                    .as_nanos();
                std::env::temp_dir().join(format!("topstar-journal-{nonce}.log"))
            }

            #[test]
            fn appends_and_recovers_records() {
                let path = temporary_path();

                {
                    let mut journal = Journal::open(&path).expect("journal should open");
                    assert_eq!(journal.next_sequence(), 0);
                    assert_eq!(
                        journal
                            .append(&serde_json::json!({"event": "created"}))
                            .expect("append should succeed")
                            .sequence,
                        0
                    );
                    assert_eq!(journal.next_sequence(), 1);
                    journal
                        .append(&serde_json::json!({"event": "authorized"}))
                        .expect("append should succeed");
                }

                let journal = Journal::open(&path).expect("journal should recover");
                let records: Vec<JournalRecord<serde_json::Value>> =
                    journal.read_all().expect("records should decode");

                assert_eq!(records.len(), 2);
                assert_eq!(records[0].sequence, 0);
                assert_eq!(records[1].sequence, 1);
                assert_eq!(records[1].payload["event"], "authorized");

                std::fs::remove_file(path).expect("temporary journal should be removed");
            }

            #[test]
            fn rejects_invalid_header() {
                let path = temporary_path();
                std::fs::write(&path, b"invalid").expect("fixture should write");

                assert!(matches!(
                    Journal::open(&path),
                    Err(PersistenceError::InvalidHeader)
                ));

                std::fs::remove_file(path).expect("temporary fixture should be removed");
            }

            #[test]
            fn rejects_non_monotonic_sequences() {
                let path = temporary_path();

                let first = JournalRecord {
                    sequence: 0_u64,
                    payload: serde_json::json!({"ok": true}),
                };
                let second = JournalRecord {
                    sequence: 2_u64,
                    payload: serde_json::json!({"ok": true}),
                };

                let first_bytes = serde_json::to_vec(&first).expect("serialization should succeed");
                let second_bytes =
                    serde_json::to_vec(&second).expect("serialization should succeed");
                let mut bytes = MAGIC.to_vec();
                bytes.extend_from_slice(&(first_bytes.len() as u32).to_le_bytes());
                bytes.extend_from_slice(&first_bytes);
                bytes.extend_from_slice(&(second_bytes.len() as u32).to_le_bytes());
                bytes.extend_from_slice(&second_bytes);
                std::fs::write(&path, bytes).expect("fixture should write");

                assert!(matches!(
                    Journal::open(&path),
                    Err(PersistenceError::NonMonotonicSequence {
                        expected: 1,
                        found: 2
                    })
                ));

                std::fs::remove_file(path).expect("temporary fixture should be removed");
            }
        }
    }
    pub mod policy {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{CapabilityId, Identity};

        pub type PolicyId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Policy {
            pub id: PolicyId,
            pub name: String,
            pub allowed_capabilities: Vec<CapabilityId>,
            pub denied_capabilities: Vec<CapabilityId>,
        }

        impl Default for Policy {
            fn default() -> Self {
                Self::new("default")
            }
        }

        impl Policy {
            pub fn new(name: impl Into<String>) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    name: name.into(),
                    allowed_capabilities: Vec::new(),
                    denied_capabilities: Vec::new(),
                }
            }

            pub fn permits(&self, capability: CapabilityId) -> bool {
                self.allowed_capabilities.contains(&capability)
                    && !self.denied_capabilities.contains(&capability)
            }

            pub fn applies_to(&self, _identity: &Identity) -> bool {
                true
            }
        }
    }
    pub mod protocol {
        use std::collections::BTreeMap;

        use serde::{Deserialize, Serialize};
        use thiserror::Error;
        use uuid::Uuid;

        use crate::topstar::{
            Action, ActionResult, CapabilityId, Context, Evidence, Identity, Intent,
            ResponsibilityContract,
        };

        pub const PROTOCOL_NAME: &str = "topstar";
        pub const PROTOCOL_MAJOR: u16 = 1;
        pub const PROTOCOL_MINOR: u16 = 0;

        pub type MessageId = Uuid;
        pub type CorrelationId = Uuid;

        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct ProtocolVersion {
            pub major: u16,
            pub minor: u16,
        }

        impl ProtocolVersion {
            pub const CURRENT: Self = Self {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
            };

            pub const fn new(major: u16, minor: u16) -> Self {
                Self { major, minor }
            }

            pub const fn compatible_with(self, other: Self) -> bool {
                self.major == other.major && self.minor >= other.minor
            }
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum MessageKind {
            Request,
            Response,
            Event,
            Progress,
            Cancel,
            Error,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct SchemaDescriptor {
            pub name: String,
            pub version: ProtocolVersion,
            pub content_type: String,
        }

        impl SchemaDescriptor {
            pub fn new(
                name: impl Into<String>,
                version: ProtocolVersion,
                content_type: impl Into<String>,
            ) -> Self {
                Self {
                    name: name.into(),
                    version,
                    content_type: content_type.into(),
                }
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct RetryPolicy {
            pub max_attempts: u32,
            pub backoff_ms: u64,
            pub retryable_errors: Vec<String>,
        }

        impl Default for RetryPolicy {
            fn default() -> Self {
                Self {
                    max_attempts: 1,
                    backoff_ms: 0,
                    retryable_errors: Vec::new(),
                }
            }
        }

        impl RetryPolicy {
            pub fn validate(&self) -> Result<(), ProtocolError> {
                if self.max_attempts == 0 {
                    return Err(ProtocolError::InvalidRetryPolicy(
                        "max_attempts must be at least one".into(),
                    ));
                }
                Ok(())
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct RequestOptions {
            pub timeout_ms: u64,
            pub retry: RetryPolicy,
            pub streaming: bool,
        }

        impl Default for RequestOptions {
            fn default() -> Self {
                Self {
                    timeout_ms: 30_000,
                    retry: RetryPolicy::default(),
                    streaming: false,
                }
            }
        }

        impl RequestOptions {
            pub fn validate(&self) -> Result<(), ProtocolError> {
                if self.timeout_ms == 0 {
                    return Err(ProtocolError::InvalidTimeout);
                }
                self.retry.validate()
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
        pub struct SecurityMetadata {
            pub authenticated: bool,
            pub encrypted: bool,
            pub signed: bool,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ProtocolEnvelope {
            pub id: MessageId,
            pub correlation_id: Option<CorrelationId>,
            pub protocol: String,
            pub version: ProtocolVersion,
            pub kind: MessageKind,
            pub schema: SchemaDescriptor,
            pub sender: Identity,
            pub recipient: Option<Identity>,
            pub payload: serde_json::Value,
            pub metadata: BTreeMap<String, String>,
            pub security: SecurityMetadata,
        }

        impl ProtocolEnvelope {
            pub fn new(
                kind: MessageKind,
                schema: SchemaDescriptor,
                sender: Identity,
                payload: serde_json::Value,
            ) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    correlation_id: None,
                    protocol: PROTOCOL_NAME.into(),
                    version: ProtocolVersion::CURRENT,
                    kind,
                    schema,
                    sender,
                    recipient: None,
                    payload,
                    metadata: BTreeMap::new(),
                    security: SecurityMetadata::default(),
                }
            }

            pub fn with_correlation(mut self, correlation_id: CorrelationId) -> Self {
                self.correlation_id = Some(correlation_id);
                self
            }

            pub fn with_recipient(mut self, recipient: Identity) -> Self {
                self.recipient = Some(recipient);
                self
            }

            pub fn validate(&self, supported: ProtocolVersion) -> Result<(), ProtocolError> {
                if self.protocol != PROTOCOL_NAME {
                    return Err(ProtocolError::UnsupportedProtocol(self.protocol.clone()));
                }
                if !self.version.compatible_with(supported) {
                    return Err(ProtocolError::IncompatibleVersion {
                        message: self.version,
                        supported,
                    });
                }
                if self.schema.name.trim().is_empty() {
                    return Err(ProtocolError::InvalidSchema("schema name is empty".into()));
                }
                if self.schema.content_type.trim().is_empty() {
                    return Err(ProtocolError::InvalidSchema("content type is empty".into()));
                }
                if self.sender.name.trim().is_empty() {
                    return Err(ProtocolError::InvalidIdentity);
                }
                Ok(())
            }

            pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
                serde_json::to_vec(self).map_err(|e| ProtocolError::Serialization(e.to_string()))
            }

            pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
                serde_json::from_slice(bytes)
                    .map_err(|e| ProtocolError::Serialization(e.to_string()))
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct RequestMessage {
            pub intent: Option<Intent>,
            pub context: Option<Context>,
            pub capability: Option<CapabilityId>,
            pub responsibility: Option<ResponsibilityContract>,
            pub action: Option<Action>,
            pub options: RequestOptions,
            pub parameters: serde_json::Value,
        }

        impl RequestMessage {
            pub fn validate(&self) -> Result<(), ProtocolError> {
                self.options.validate()?;
                if self.intent.is_none() && self.action.is_none() {
                    return Err(ProtocolError::InvalidRequest(
                        "request must contain an intent or action".into(),
                    ));
                }
                Ok(())
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ResponseMessage {
            pub result: Option<ActionResult>,
            pub output: serde_json::Value,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct EventMessage {
            pub topic: String,
            pub payload: serde_json::Value,
        }

        impl EventMessage {
            pub fn validate(&self) -> Result<(), ProtocolError> {
                if self.topic.trim().is_empty() {
                    return Err(ProtocolError::InvalidEvent("topic is empty".into()));
                }
                Ok(())
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ProgressMessage {
            pub completed: u64,
            pub total: Option<u64>,
            pub message: Option<String>,
        }

        impl ProgressMessage {
            pub fn validate(&self) -> Result<(), ProtocolError> {
                if let Some(total) = self.total {
                    if total == 0 || self.completed > total {
                        return Err(ProtocolError::InvalidProgress);
                    }
                }
                Ok(())
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct CancelMessage {
            pub reason: Option<String>,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ErrorMessage {
            pub code: String,
            pub message: String,
            pub retryable: bool,
        }

        impl ErrorMessage {
            pub fn new(
                code: impl Into<String>,
                message: impl Into<String>,
                retryable: bool,
            ) -> Self {
                Self {
                    code: code.into(),
                    message: message.into(),
                    retryable,
                }
            }

            pub fn validate(&self) -> Result<(), ProtocolError> {
                if self.code.trim().is_empty() || self.message.trim().is_empty() {
                    return Err(ProtocolError::InvalidError);
                }
                Ok(())
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct CapabilityAdvertisement {
            pub capability: CapabilityId,
            pub schema: SchemaDescriptor,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct SchemaNegotiation {
            pub supported: Vec<SchemaDescriptor>,
        }

        impl SchemaNegotiation {
            pub fn negotiate(
                &self,
                requested: &SchemaDescriptor,
            ) -> Result<SchemaDescriptor, ProtocolError> {
                self.supported
                    .iter()
                    .find(|candidate| {
                        candidate.name == requested.name
                            && candidate.content_type == requested.content_type
                            && candidate.version.compatible_with(requested.version)
                    })
                    .cloned()
                    .ok_or_else(|| ProtocolError::SchemaNotSupported(requested.name.clone()))
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct EvidenceMessage {
            pub evidence: Evidence,
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum ProtocolError {
            #[error("unsupported protocol: {0}")]
            UnsupportedProtocol(String),
            #[error("incompatible protocol version: message {message:?}, supported {supported:?}")]
            IncompatibleVersion {
                message: ProtocolVersion,
                supported: ProtocolVersion,
            },
            #[error("invalid schema: {0}")]
            InvalidSchema(String),
            #[error("invalid identity")]
            InvalidIdentity,
            #[error("invalid retry policy: {0}")]
            InvalidRetryPolicy(String),
            #[error("invalid timeout")]
            InvalidTimeout,
            #[error("invalid request: {0}")]
            InvalidRequest(String),
            #[error("invalid event: {0}")]
            InvalidEvent(String),
            #[error("invalid progress")]
            InvalidProgress,
            #[error("invalid error message")]
            InvalidError,
            #[error("schema not supported: {0}")]
            SchemaNotSupported(String),
            #[error("serialization failed: {0}")]
            Serialization(String),
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::Identity;

            #[test]
            fn envelope_round_trips_through_json() {
                let sender = Identity::new("runtime");
                let schema = SchemaDescriptor::new(
                    "topstar.request",
                    ProtocolVersion::CURRENT,
                    "application/json",
                );
                let envelope = ProtocolEnvelope::new(
                    MessageKind::Request,
                    schema,
                    sender,
                    serde_json::json!({"operation": "discover"}),
                );

                let encoded = envelope.encode().expect("encoding should succeed");
                let decoded = ProtocolEnvelope::decode(&encoded).expect("decoding should succeed");

                assert_eq!(decoded.id, envelope.id);
                assert_eq!(decoded.payload, envelope.payload);
                assert_eq!(decoded.version, ProtocolVersion::CURRENT);
            }

            #[test]
            fn request_requires_intent_or_action() {
                let request = RequestMessage {
                    intent: None,
                    context: None,
                    capability: None,
                    responsibility: None,
                    action: None,
                    options: RequestOptions::default(),
                    parameters: serde_json::Value::Null,
                };

                assert!(matches!(
                    request.validate(),
                    Err(ProtocolError::InvalidRequest(_))
                ));
            }

            #[test]
            fn schema_negotiation_requires_compatible_schema() {
                let supported = SchemaNegotiation {
                    supported: vec![SchemaDescriptor::new(
                        "topstar.request",
                        ProtocolVersion::new(1, 2),
                        "application/json",
                    )],
                };
                let requested = SchemaDescriptor::new(
                    "topstar.request",
                    ProtocolVersion::new(1, 1),
                    "application/json",
                );

                assert!(supported.negotiate(&requested).is_ok());
            }

            #[test]
            fn incompatible_major_version_is_rejected() {
                let mut envelope = ProtocolEnvelope::new(
                    MessageKind::Event,
                    SchemaDescriptor::new(
                        "topstar.event",
                        ProtocolVersion::new(2, 0),
                        "application/json",
                    ),
                    Identity::new("runtime"),
                    serde_json::json!({}),
                );

                envelope.version = ProtocolVersion::new(2, 0);

                assert!(matches!(
                    envelope.validate(ProtocolVersion::CURRENT),
                    Err(ProtocolError::IncompatibleVersion { .. })
                ));
            }

            #[test]
            fn progress_cannot_exceed_total() {
                let progress = ProgressMessage {
                    completed: 11,
                    total: Some(10),
                    message: None,
                };
                assert_eq!(progress.validate(), Err(ProtocolError::InvalidProgress));
            }
        }
    }
    pub mod provenance {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{
            ActionId, ActorId, EventId, EvidenceId, IntentId, ResponsibilityContractId,
        };

        pub type ProvenanceId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Provenance {
            pub id: ProvenanceId,
            pub actor: ActorId,
            pub intent: Option<IntentId>,
            pub responsibility: Option<ResponsibilityContractId>,
            pub action: Option<ActionId>,
            pub event: Option<EventId>,
            pub evidence: Vec<EvidenceId>,
        }

        impl Provenance {
            pub fn new(actor: ActorId) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    actor,
                    intent: None,
                    responsibility: None,
                    action: None,
                    event: None,
                    evidence: Vec::new(),
                }
            }
        }
    }
    pub mod registry {
        use std::collections::HashMap;
        use thiserror::Error;

        use crate::topstar::{Capability, CapabilityId};

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum RegistryError {
            #[error("capability already registered: {0}")]
            AlreadyRegistered(CapabilityId),
            #[error("capability not found: {0}")]
            NotFound(CapabilityId),
        }

        #[derive(Debug, Default)]
        pub struct CapabilityRegistry {
            capabilities: HashMap<CapabilityId, Capability>,
        }

        impl CapabilityRegistry {
            pub fn new() -> Self {
                Self::default()
            }

            pub fn register(&mut self, capability: Capability) -> Result<(), RegistryError> {
                if self.capabilities.contains_key(&capability.id) {
                    return Err(RegistryError::AlreadyRegistered(capability.id));
                }
                self.capabilities.insert(capability.id, capability);
                Ok(())
            }

            pub fn get(&self, id: CapabilityId) -> Result<&Capability, RegistryError> {
                self.capabilities
                    .get(&id)
                    .ok_or(RegistryError::NotFound(id))
            }

            pub fn contains(&self, id: CapabilityId) -> bool {
                self.capabilities.contains_key(&id)
            }

            pub fn len(&self) -> usize {
                self.capabilities.len()
            }

            pub fn is_empty(&self) -> bool {
                self.capabilities.is_empty()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::CapabilityRisk;

            #[test]
            fn registers_and_retrieves_capability() {
                let capability = Capability::new("read", "read data", CapabilityRisk::Low);
                let id = capability.id;
                let mut registry = CapabilityRegistry::new();
                registry.register(capability).unwrap();
                assert_eq!(registry.get(id).unwrap().name, "read");
            }
        }
    }
    pub mod resource {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        pub type ResourceId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum ResourceKind {
            Cpu,
            Memory,
            Storage,
            Network,
            Gpu,
            Device,
            Financial,
            Physical,
            Data,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Resource {
            pub id: ResourceId,
            pub kind: ResourceKind,
            pub name: String,
            pub limit: Option<u64>,
            pub unit: Option<String>,
        }

        impl Resource {
            pub fn new(kind: ResourceKind, name: impl Into<String>) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    kind,
                    name: name.into(),
                    limit: None,
                    unit: None,
                }
            }

            pub fn with_limit(mut self, limit: u64, unit: impl Into<String>) -> Self {
                self.limit = Some(limit);
                self.unit = Some(unit.into());
                self
            }
        }
    }
    pub mod responsibility {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{ActorId, CapabilityId, ExecutionBoundary};

        pub type ResponsibilityContractId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ResponsibilityContract {
            pub id: ResponsibilityContractId,
            pub goal: String,
            pub actor: ActorId,
            pub allowed_capabilities: Vec<CapabilityId>,
            pub forbidden_capabilities: Vec<CapabilityId>,
            pub boundary: ExecutionBoundary,
            pub expires_at_unix: Option<i64>,
            pub revoked: bool,
        }

        impl ResponsibilityContract {
            pub fn new(goal: impl Into<String>, actor: ActorId) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    goal: goal.into(),
                    actor,
                    allowed_capabilities: Vec::new(),
                    forbidden_capabilities: Vec::new(),
                    boundary: ExecutionBoundary::default(),
                    expires_at_unix: None,
                    revoked: false,
                }
            }

            pub fn allows(&self, capability: CapabilityId, now_unix: i64) -> bool {
                !self.revoked
                    && self.expires_at_unix.is_none_or(|expiry| now_unix < expiry)
                    && self.allowed_capabilities.contains(&capability)
                    && !self.forbidden_capabilities.contains(&capability)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::Identity;

            #[test]
            fn revoked_contract_denies_capability() {
                let actor = Identity::new("agent");
                let capability = Uuid::new_v4();
                let mut contract = ResponsibilityContract::new("test", actor.id);
                contract.allowed_capabilities.push(capability);
                contract.revoked = true;
                assert!(!contract.allows(capability, 0));
            }
        }
    }
    pub mod result {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{ActionId, EvidenceId};

        pub type ResultId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum ResultStatus {
            Success,
            Failure,
            Partial,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ActionResult {
            pub id: ResultId,
            pub action: ActionId,
            pub status: ResultStatus,
            pub output: serde_json::Value,
            pub evidence: Vec<EvidenceId>,
        }

        impl ActionResult {
            pub fn success(action: ActionId, output: serde_json::Value) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    action,
                    status: ResultStatus::Success,
                    output,
                    evidence: Vec::new(),
                }
            }
        }
    }
    pub mod revocation {
        use std::collections::HashSet;

        use crate::topstar::{ActionId, CapabilityId, ResponsibilityContractId};

        #[derive(Debug, Default)]
        pub struct RevocationRegistry {
            capabilities: HashSet<CapabilityId>,
            responsibilities: HashSet<ResponsibilityContractId>,
            actions: HashSet<ActionId>,
        }

        impl RevocationRegistry {
            pub fn new() -> Self {
                Self::default()
            }

            pub fn revoke_capability(&mut self, id: CapabilityId) {
                self.capabilities.insert(id);
            }

            pub fn revoke_responsibility(&mut self, id: ResponsibilityContractId) {
                self.responsibilities.insert(id);
            }

            pub fn revoke_action(&mut self, id: ActionId) {
                self.actions.insert(id);
            }

            pub fn is_capability_revoked(&self, id: CapabilityId) -> bool {
                self.capabilities.contains(&id)
            }

            pub fn is_responsibility_revoked(&self, id: ResponsibilityContractId) -> bool {
                self.responsibilities.contains(&id)
            }

            pub fn is_action_revoked(&self, id: ActionId) -> bool {
                self.actions.contains(&id)
            }
        }
    }
    pub mod runtime {
        use thiserror::Error;

        use crate::topstar::{
            authorize, Action, ActionResult, ActionState, AuthorizationDecision,
            CapabilityRegistry, Evidence, Identity, Policy, ResponsibilityContract,
        };

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum RuntimeError {
            #[error("unknown capability")]
            UnknownCapability,
            #[error("authorization denied: {0}")]
            AuthorizationDenied(String),
        }

        #[derive(Debug, Default)]
        pub struct Runtime {
            capabilities: CapabilityRegistry,
        }

        impl Runtime {
            pub fn new() -> Self {
                Self::default()
            }

            pub fn capabilities(&self) -> &CapabilityRegistry {
                &self.capabilities
            }

            pub fn capabilities_mut(&mut self) -> &mut CapabilityRegistry {
                &mut self.capabilities
            }

            pub fn authorize_action(
                &self,
                action: &Action,
                identity: &Identity,
                responsibility: &ResponsibilityContract,
                policy: &Policy,
                now_unix: i64,
            ) -> Result<(), RuntimeError> {
                if !self.capabilities.contains(action.capability) {
                    return Err(RuntimeError::UnknownCapability);
                }

                match authorize(
                    identity,
                    action.capability,
                    responsibility,
                    policy,
                    now_unix,
                ) {
                    AuthorizationDecision::Allow => Ok(()),
                    AuthorizationDecision::Deny(reason) => {
                        Err(RuntimeError::AuthorizationDenied(reason))
                    }
                }
            }

            pub fn execute(
                &self,
                mut action: Action,
                identity: &Identity,
                responsibility: &ResponsibilityContract,
                policy: &Policy,
                now_unix: i64,
            ) -> Result<ActionResult, RuntimeError> {
                self.authorize_action(&action, identity, responsibility, policy, now_unix)?;
                action.state = ActionState::Executing;
                action.state = ActionState::Succeeded;

                Ok(ActionResult::success(
                    action.id,
                    serde_json::json!({
                        "status": "executed",
                        "capability": action.capability,
                    }),
                ))
            }

            pub fn verify_evidence(&self, evidence: &Evidence) -> bool {
                !evidence.source.is_empty()
            }
        }
    }
    pub mod session {
        use serde::{Deserialize, Serialize};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        use thiserror::Error;
        use uuid::Uuid;

        use crate::topstar::{Identity, NetworkAddress, ProtocolVersion};

        pub type SessionId = Uuid;

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum SessionState {
            Handshaking,
            Established,
            Closing,
            Closed,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct PeerIdentity {
            pub identity: Identity,
            pub node_id: Uuid,
            pub addresses: Vec<NetworkAddress>,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct SessionLimits {
            pub idle_timeout: Duration,
            pub max_frame_size: usize,
        }

        impl Default for SessionLimits {
            fn default() -> Self {
                Self {
                    idle_timeout: Duration::from_secs(300),
                    max_frame_size: 16 * 1024 * 1024,
                }
            }
        }

        impl SessionLimits {
            pub fn validate(&self) -> Result<(), SessionError> {
                if self.idle_timeout.is_zero() {
                    return Err(SessionError::InvalidLimits("idle timeout must be non-zero"));
                }
                if self.max_frame_size == 0 {
                    return Err(SessionError::InvalidLimits(
                        "max frame size must be non-zero",
                    ));
                }
                Ok(())
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Session {
            pub id: SessionId,
            pub local: Identity,
            pub peer: PeerIdentity,
            pub protocol: ProtocolVersion,
            pub state: SessionState,
            pub authenticated: bool,
            pub encrypted: bool,
            pub established_at_unix: Option<u64>,
            pub last_activity_unix: u64,
            pub limits: SessionLimits,
        }

        impl Session {
            pub fn begin(
                local: Identity,
                peer: PeerIdentity,
                protocol: ProtocolVersion,
                limits: SessionLimits,
            ) -> Result<Self, SessionError> {
                limits.validate()?;
                if peer.identity.name.trim().is_empty() {
                    return Err(SessionError::InvalidPeer("peer identity name is empty"));
                }

                let now = unix_now();
                Ok(Self {
                    id: Uuid::new_v4(),
                    local,
                    peer,
                    protocol,
                    state: SessionState::Handshaking,
                    authenticated: false,
                    encrypted: false,
                    established_at_unix: None,
                    last_activity_unix: now,
                    limits,
                })
            }

            pub fn authenticate(&mut self, encrypted: bool) -> Result<(), SessionError> {
                if self.state != SessionState::Handshaking {
                    return Err(SessionError::InvalidTransition {
                        from: self.state,
                        operation: "authenticate",
                    });
                }
                if !encrypted {
                    return Err(SessionError::SecurityRequirement(
                        "authenticated sessions must be encrypted",
                    ));
                }
                self.authenticated = true;
                self.encrypted = true;
                self.state = SessionState::Established;
                let now = unix_now();
                self.established_at_unix = Some(now);
                self.last_activity_unix = now;
                Ok(())
            }

            pub fn touch(&mut self) -> Result<(), SessionError> {
                if self.state != SessionState::Established {
                    return Err(SessionError::InvalidTransition {
                        from: self.state,
                        operation: "touch",
                    });
                }
                self.last_activity_unix = unix_now();
                Ok(())
            }

            pub fn begin_close(&mut self) -> Result<(), SessionError> {
                match self.state {
                    SessionState::Established => {
                        self.state = SessionState::Closing;
                        Ok(())
                    }
                    SessionState::Closing | SessionState::Closed => Ok(()),
                    SessionState::Handshaking => Err(SessionError::InvalidTransition {
                        from: self.state,
                        operation: "begin_close",
                    }),
                }
            }

            pub fn close(&mut self) {
                self.state = SessionState::Closed;
                self.authenticated = false;
            }

            pub fn is_idle(&self, now_unix: u64) -> bool {
                now_unix.saturating_sub(self.last_activity_unix)
                    >= self.limits.idle_timeout.as_secs()
            }
        }

        #[derive(Debug, Error, PartialEq, Eq)]
        pub enum SessionError {
            #[error("invalid session limits: {0}")]
            InvalidLimits(&'static str),
            #[error("invalid peer: {0}")]
            InvalidPeer(&'static str),
            #[error("invalid session transition from {from:?} during {operation}")]
            InvalidTransition {
                from: SessionState,
                operation: &'static str,
            },
            #[error("security requirement failed: {0}")]
            SecurityRequirement(&'static str),
        }

        fn unix_now() -> u64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::topstar::{Endpoint, NetworkAddress, Transport};

            fn peer() -> PeerIdentity {
                PeerIdentity {
                    identity: Identity::new("peer"),
                    node_id: Uuid::new_v4(),
                    addresses: vec![NetworkAddress::new(
                        Transport::Tcp,
                        Endpoint::new("127.0.0.1", 8443).expect("endpoint"),
                    )],
                }
            }

            #[test]
            fn authentication_establishes_encrypted_session() {
                let mut session = Session::begin(
                    Identity::new("local"),
                    peer(),
                    ProtocolVersion::CURRENT,
                    SessionLimits::default(),
                )
                .expect("session");
                session.authenticate(true).expect("authentication");
                assert_eq!(session.state, SessionState::Established);
                assert!(session.authenticated);
                assert!(session.encrypted);
                assert!(session.established_at_unix.is_some());
            }

            #[test]
            fn plaintext_authentication_is_rejected() {
                let mut session = Session::begin(
                    Identity::new("local"),
                    peer(),
                    ProtocolVersion::CURRENT,
                    SessionLimits::default(),
                )
                .expect("session");
                assert_eq!(
                    session.authenticate(false),
                    Err(SessionError::SecurityRequirement(
                        "authenticated sessions must be encrypted"
                    ))
                );
            }

            #[test]
            fn session_lifecycle_is_bounded() {
                let mut session = Session::begin(
                    Identity::new("local"),
                    peer(),
                    ProtocolVersion::CURRENT,
                    SessionLimits::default(),
                )
                .expect("session");
                session.authenticate(true).expect("authentication");
                session.begin_close().expect("close start");
                session.close();
                assert_eq!(session.state, SessionState::Closed);
                assert!(!session.authenticated);
            }
        }
    }
    pub mod verification {
        use serde::{Deserialize, Serialize};
        use uuid::Uuid;

        use crate::topstar::{ActionId, EvidenceId, ResultId};

        pub type VerificationId = Uuid;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum VerificationStatus {
            Pending,
            Verified,
            Rejected,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Verification {
            pub id: VerificationId,
            pub action: ActionId,
            pub result: ResultId,
            pub evidence: Vec<EvidenceId>,
            pub status: VerificationStatus,
            pub reason: Option<String>,
        }

        impl Verification {
            pub fn pending(action: ActionId, result: ResultId) -> Self {
                Self {
                    id: Uuid::new_v4(),
                    action,
                    result,
                    evidence: Vec::new(),
                    status: VerificationStatus::Pending,
                    reason: None,
                }
            }

            pub fn verified(mut self) -> Self {
                self.status = VerificationStatus::Verified;
                self.reason = None;
                self
            }

            pub fn rejected(mut self, reason: impl Into<String>) -> Self {
                self.status = VerificationStatus::Rejected;
                self.reason = Some(reason.into());
                self
            }
        }
    }

    pub use action::{Action, ActionId, ActionState};
    pub use audit::{AuditEntry, AuditEntryId, AuditOutcome};
    pub use authorization::{authorize, AuthorizationDecision};
    pub use boundary::ExecutionBoundary;
    pub use capability::{Capability, CapabilityId, CapabilityRisk};
    pub use chat::{
        ChatError, ChatMessage, ChatRequest, ChatResponse, ChatRole, ConversationId,
        MessageId as ChatMessageId,
    };
    pub use context::{Context, ContextId};
    pub use device::{
        Device, DeviceError, DeviceId, HardwareInfo, HealthState, MachineProfile, PowerState,
        SecurityState,
    };
    pub use device_discovery::{DeviceSelector, DeviceSnapshot};
    pub use device_registry::{DeviceLifecycle, DeviceRecord, DeviceRegistry, DeviceRegistryError};
    pub use event::{Event, EventId};
    pub use eventing::{EventBroker, EventFilter, EventRecord, EventingError, SubscriptionId};
    pub use evidence::{Evidence, EvidenceId};
    pub use execution_audit::{AuditQuery, ExecutionAuditError, ExecutionAuditLog};
    pub use execution_record::{
        ExecutionOutcome, ExecutionRecord, ExecutionRecordError, ExecutionRecordId,
    };
    pub use execution_record_store::{
        ExecutionRecordCursor, ExecutionRecordPage, ExecutionRecordQuery,
        ExecutionRecordQueryError, ExecutionRecordStore, ExecutionRecordStoreError,
    };
    pub use executor::{ActionHandler, ExecutionEngine, ExecutionError};
    pub use identity::{ActorId, Identity};
    pub use intent::{Intent, IntentId};
    pub use lifecycle::Lifecycle;
    pub use network::{Endpoint, NetworkAddress, NetworkError, NetworkIdentity, Transport};
    pub use persistence::{Journal, JournalPosition, JournalRecord, PersistenceError};
    pub use policy::{Policy, PolicyId};
    pub use protocol::{
        CancelMessage, CapabilityAdvertisement, CorrelationId, ErrorMessage, EventMessage,
        MessageId, MessageKind, ProgressMessage, ProtocolEnvelope, ProtocolError, ProtocolVersion,
        RequestMessage, RequestOptions, ResponseMessage, RetryPolicy, SchemaDescriptor,
        SchemaNegotiation, SecurityMetadata, PROTOCOL_MAJOR, PROTOCOL_MINOR, PROTOCOL_NAME,
    };
    pub use provenance::{Provenance, ProvenanceId};
    pub use registry::{CapabilityRegistry, RegistryError};
    pub use resource::{Resource, ResourceId, ResourceKind};
    pub use responsibility::{ResponsibilityContract, ResponsibilityContractId};
    pub use result::{ActionResult, ResultId, ResultStatus};
    pub use revocation::RevocationRegistry;
    pub use runtime::{Runtime, RuntimeError};
    pub use session::{
        PeerIdentity, Session, SessionError, SessionId, SessionLimits, SessionState,
    };
    pub use verification::{Verification, VerificationId, VerificationStatus};
}

/// Monitoring: metrics, health, service-level objectives and alerts.
///
/// Everything is plain data with no clock and no I/O: callers pass the time (seconds since the
/// Unix epoch) and decide where the output goes. Everything is bounded, so a program that invents
/// metric names or labels cannot grow memory without limit.
///
/// * [`metrics::Registry`]: counters, gauges and histograms with labels, and a Prometheus-style
///   text export.
/// * [`health::Board`]: named health checks, with staleness (a check that stops reporting is not
///   healthy).
/// * [`slo::Slo`]: a success-ratio objective over a rolling window, with its error budget and
///   burn rate.
/// * [`alert::Alerter`]: threshold rules with a "for" duration and a pending, firing, resolved
///   life cycle, and a bounded history.
#[allow(clippy::module_inception)]
pub mod monitor {
    #![forbid(unsafe_code)]

    /// Why a monitoring call was refused.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MonitorError {
        /// A name or label is invalid (the text says which).
        BadName(&'static str),
        /// A value is not usable (NaN, infinite, negative where that is not allowed).
        BadValue,
        /// A limit was reached (the text says which).
        Full(&'static str),
    }

    impl std::fmt::Display for MonitorError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::BadName(w) => write!(f, "invalid {w}"),
                Self::BadValue => f.write_str("invalid value"),
                Self::Full(w) => write!(f, "monitoring limit reached: {w}"),
            }
        }
    }

    impl std::error::Error for MonitorError {}

    fn valid_name(s: &str) -> bool {
        let mut c = s.chars();
        matches!(c.next(), Some(a) if a.is_ascii_lowercase() || a == '_')
            && s.len() <= 64
            && s.chars()
                .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_')
    }

    /// Metrics: counters, gauges and histograms.
    pub mod metrics {
        use super::{valid_name, MonitorError};
        use std::collections::BTreeMap;

        /// Most distinct series (a name with one set of labels) kept.
        pub const MAX_SERIES: usize = 10_000;
        /// Most labels on one series.
        pub const MAX_LABELS: usize = 8;
        /// Longest label value, in characters.
        pub const MAX_LABEL_VALUE: usize = 100;

        /// What a series measures.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Value {
            /// A total that only goes up.
            Counter(f64),
            /// A value that goes up and down.
            Gauge(f64),
            /// Observations counted into buckets (`bounds[i]` is the upper edge; the last count is
            /// everything above the last edge).
            Histogram {
                /// Upper edges, ascending.
                bounds: Vec<f64>,
                /// One count per edge, plus one for "above all edges".
                counts: Vec<u64>,
                /// Sum of all observations.
                sum: f64,
                /// Number of observations.
                count: u64,
            },
        }

        /// One series: a name, its labels and its value.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Series {
            /// Metric name.
            pub name: String,
            /// Labels, sorted by name.
            pub labels: BTreeMap<String, String>,
            /// The value.
            pub value: Value,
        }

        /// The set of all series.
        #[derive(Debug, Clone, Default)]
        pub struct Registry {
            series: BTreeMap<String, Series>,
        }

        fn key(name: &str, labels: &BTreeMap<String, String>) -> String {
            let mut k = name.to_string();
            for (a, b) in labels {
                k.push('\u{1}');
                k.push_str(a);
                k.push('=');
                k.push_str(b);
            }
            k
        }

        fn clean(
            name: &str,
            labels: &[(&str, &str)],
        ) -> Result<BTreeMap<String, String>, MonitorError> {
            if !valid_name(name) {
                return Err(MonitorError::BadName("metric name"));
            }
            if labels.len() > MAX_LABELS {
                return Err(MonitorError::Full("labels"));
            }
            let mut m = BTreeMap::new();
            for (k, v) in labels {
                if !valid_name(k) {
                    return Err(MonitorError::BadName("label name"));
                }
                m.insert((*k).to_string(), v.chars().take(MAX_LABEL_VALUE).collect());
            }
            Ok(m)
        }

        impl Registry {
            /// An empty registry.
            #[must_use]
            pub fn new() -> Self {
                Self::default()
            }

            /// Number of series.
            #[must_use]
            pub fn len(&self) -> usize {
                self.series.len()
            }

            /// True when there are no series.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.series.is_empty()
            }

            fn slot(
                &mut self,
                name: &str,
                labels: &[(&str, &str)],
                make: impl FnOnce() -> Value,
            ) -> Result<&mut Series, MonitorError> {
                let l = clean(name, labels)?;
                let k = key(name, &l);
                if !self.series.contains_key(&k) && self.series.len() >= MAX_SERIES {
                    return Err(MonitorError::Full("series"));
                }
                Ok(self.series.entry(k).or_insert_with(|| Series {
                    name: name.to_string(),
                    labels: l,
                    value: make(),
                }))
            }

            /// Adds `by` (not negative) to a counter.
            ///
            /// # Errors
            /// [`MonitorError`] for a bad name, label, value, or when the series limit is reached or
            /// the series is not a counter.
            pub fn counter_add(
                &mut self,
                name: &str,
                labels: &[(&str, &str)],
                by: f64,
            ) -> Result<(), MonitorError> {
                if !by.is_finite() || by < 0.0 {
                    return Err(MonitorError::BadValue);
                }
                match &mut self.slot(name, labels, || Value::Counter(0.0))?.value {
                    Value::Counter(v) => {
                        *v += by;
                        Ok(())
                    }
                    _ => Err(MonitorError::BadName("metric kind")),
                }
            }

            /// Sets a gauge.
            ///
            /// # Errors
            /// As [`Registry::counter_add`].
            pub fn gauge_set(
                &mut self,
                name: &str,
                labels: &[(&str, &str)],
                value: f64,
            ) -> Result<(), MonitorError> {
                if !value.is_finite() {
                    return Err(MonitorError::BadValue);
                }
                match &mut self.slot(name, labels, || Value::Gauge(0.0))?.value {
                    Value::Gauge(v) => {
                        *v = value;
                        Ok(())
                    }
                    _ => Err(MonitorError::BadName("metric kind")),
                }
            }

            /// Records an observation in a histogram with the given ascending `bounds` (used the first
            /// time the series is seen; later calls must pass the same number of bounds).
            ///
            /// # Errors
            /// As [`Registry::counter_add`], and [`MonitorError::BadValue`] for bounds that are empty,
            /// not ascending, too many (over 32) or not finite.
            pub fn observe(
                &mut self,
                name: &str,
                labels: &[(&str, &str)],
                bounds: &[f64],
                value: f64,
            ) -> Result<(), MonitorError> {
                if !value.is_finite()
                    || bounds.is_empty()
                    || bounds.len() > 32
                    || bounds.iter().any(|b| !b.is_finite())
                    || bounds.windows(2).any(|w| w[0] >= w[1])
                {
                    return Err(MonitorError::BadValue);
                }
                let b = bounds.to_vec();
                match &mut self
                    .slot(name, labels, || Value::Histogram {
                        counts: vec![0; b.len() + 1],
                        bounds: b,
                        sum: 0.0,
                        count: 0,
                    })?
                    .value
                {
                    Value::Histogram {
                        bounds: have,
                        counts,
                        sum,
                        count,
                    } => {
                        if have.len() != bounds.len() {
                            return Err(MonitorError::BadValue);
                        }
                        let i = have.iter().position(|e| value <= *e).unwrap_or(have.len());
                        counts[i] += 1;
                        *sum += value;
                        *count += 1;
                        Ok(())
                    }
                    _ => Err(MonitorError::BadName("metric kind")),
                }
            }

            /// The series of this name and labels.
            #[must_use]
            pub fn get(&self, name: &str, labels: &[(&str, &str)]) -> Option<&Series> {
                let l = clean(name, labels).ok()?;
                self.series.get(&key(name, &l))
            }

            /// The current number of a counter or gauge (0 when absent or a histogram).
            #[must_use]
            pub fn number(&self, name: &str, labels: &[(&str, &str)]) -> f64 {
                match self.get(name, labels).map(|s| &s.value) {
                    Some(Value::Counter(v) | Value::Gauge(v)) => *v,
                    _ => 0.0,
                }
            }

            /// Every series of `name`, whatever its labels.
            #[must_use]
            pub fn all_of(&self, name: &str) -> Vec<&Series> {
                self.series.values().filter(|s| s.name == name).collect()
            }

            /// The text export, in the common exposition format: one line per sample, labels in
            /// braces with values escaped, histograms as `_bucket`, `_sum` and `_count` lines.
            #[must_use]
            pub fn export(&self) -> String {
                fn esc(v: &str) -> String {
                    let mut o = String::new();
                    for c in v.chars() {
                        match c {
                            '\\' => o.push_str("\\\\"),
                            '"' => o.push_str("\\\""),
                            '\n' => o.push_str("\\n"),
                            c if c.is_control() => o.push(' '),
                            c => o.push(c),
                        }
                    }
                    o
                }
                fn lab(l: &BTreeMap<String, String>, extra: Option<(&str, String)>) -> String {
                    let mut parts: Vec<String> = l
                        .iter()
                        .map(|(k, v)| format!("{k}=\"{}\"", esc(v)))
                        .collect();
                    if let Some((k, v)) = extra {
                        parts.push(format!("{k}=\"{v}\""));
                    }
                    if parts.is_empty() {
                        String::new()
                    } else {
                        format!("{{{}}}", parts.join(","))
                    }
                }
                let mut out = String::new();
                for s in self.series.values() {
                    match &s.value {
                        Value::Counter(v) | Value::Gauge(v) => {
                            out.push_str(&format!("{}{} {v}\n", s.name, lab(&s.labels, None)));
                        }
                        Value::Histogram {
                            bounds,
                            counts,
                            sum,
                            count,
                        } => {
                            let mut run = 0u64;
                            for (i, e) in bounds.iter().enumerate() {
                                run += counts[i];
                                out.push_str(&format!(
                                    "{}_bucket{} {run}\n",
                                    s.name,
                                    lab(&s.labels, Some(("le", e.to_string())))
                                ));
                            }
                            out.push_str(&format!(
                                "{}_bucket{} {count}\n",
                                s.name,
                                lab(&s.labels, Some(("le", "+Inf".into())))
                            ));
                            out.push_str(&format!(
                                "{}_sum{} {sum}\n",
                                s.name,
                                lab(&s.labels, None)
                            ));
                            out.push_str(&format!(
                                "{}_count{} {count}\n",
                                s.name,
                                lab(&s.labels, None)
                            ));
                        }
                    }
                }
                out
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn counters_add_gauges_set_and_labels_separate_series() {
                let mut r = Registry::new();
                r.counter_add("requests_total", &[("route", "chat")], 2.0)
                    .unwrap();
                r.counter_add("requests_total", &[("route", "chat")], 3.0)
                    .unwrap();
                r.counter_add("requests_total", &[("route", "models")], 1.0)
                    .unwrap();
                r.gauge_set("temperature", &[], 61.5).unwrap();
                r.gauge_set("temperature", &[], 55.0).unwrap();
                assert_eq!(r.number("requests_total", &[("route", "chat")]), 5.0);
                assert_eq!(r.number("requests_total", &[("route", "models")]), 1.0);
                assert_eq!(r.number("temperature", &[]), 55.0);
                assert_eq!(r.number("missing", &[]), 0.0);
                assert_eq!(r.all_of("requests_total").len(), 2);
                assert_eq!(r.len(), 3);
                // Label order does not matter.
                r.counter_add("x", &[("a", "1"), ("b", "2")], 1.0).unwrap();
                r.counter_add("x", &[("b", "2"), ("a", "1")], 1.0).unwrap();
                assert_eq!(r.number("x", &[("a", "1"), ("b", "2")]), 2.0);
            }

            #[test]
            fn bad_names_values_and_kinds_are_refused() {
                let mut r = Registry::new();
                for n in ["", "Upper", "1abc", "has space", "a-b", &"a".repeat(65)] {
                    assert!(
                        matches!(r.counter_add(n, &[], 1.0), Err(MonitorError::BadName(_))),
                        "{n:?}"
                    );
                }
                assert!(r.counter_add("c", &[("Bad", "v")], 1.0).is_err());
                for v in [-1.0, f64::NAN, f64::INFINITY] {
                    assert_eq!(r.counter_add("c", &[], v), Err(MonitorError::BadValue));
                }
                assert_eq!(r.gauge_set("g", &[], f64::NAN), Err(MonitorError::BadValue));
                r.counter_add("k", &[], 1.0).unwrap();
                assert!(
                    r.gauge_set("k", &[], 1.0).is_err(),
                    "a counter is not a gauge"
                );
                assert!(r.observe("k", &[], &[1.0], 0.5).is_err());
                let many: Vec<(String, String)> = (0..=MAX_LABELS)
                    .map(|i| (format!("l{i}"), "v".into()))
                    .collect();
                let refs: Vec<(&str, &str)> =
                    many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                assert_eq!(
                    r.counter_add("c", &refs, 1.0),
                    Err(MonitorError::Full("labels"))
                );
            }

            #[test]
            fn the_series_limit_stops_runaway_labels() {
                let mut r = Registry::new();
                for i in 0..MAX_SERIES {
                    r.counter_add("c", &[("id", &i.to_string())], 1.0).unwrap();
                }
                assert_eq!(
                    r.counter_add("c", &[("id", "new")], 1.0),
                    Err(MonitorError::Full("series"))
                );
                assert!(
                    r.counter_add("c", &[("id", "5")], 1.0).is_ok(),
                    "existing series still work"
                );
                assert_eq!(r.len(), MAX_SERIES);
            }

            #[test]
            fn histograms_count_into_buckets() {
                let mut r = Registry::new();
                let b = [0.1, 0.5, 1.0];
                for v in [0.05, 0.1, 0.3, 0.9, 5.0, 7.0] {
                    r.observe("latency_seconds", &[("op", "chat")], &b, v)
                        .unwrap();
                }
                let Value::Histogram {
                    counts, sum, count, ..
                } = &r.get("latency_seconds", &[("op", "chat")]).unwrap().value
                else {
                    panic!("not a histogram");
                };
                assert_eq!(counts, &vec![2, 1, 1, 2]);
                assert_eq!(*count, 6);
                assert!((sum - 13.35).abs() < 1e-9);
                for bad in [&[][..], &[1.0, 1.0][..], &[2.0, 1.0][..], &[f64::NAN][..]] {
                    assert!(r.observe("h", &[], bad, 1.0).is_err(), "{bad:?}");
                }
                assert!(
                    r.observe("latency_seconds", &[("op", "chat")], &[1.0], 1.0)
                        .is_err(),
                    "bucket count must not change"
                );
                assert!(r.observe("h2", &[], &b, f64::NAN).is_err());
            }

            #[test]
            fn the_text_export_is_well_formed_and_escapes_label_values() {
                let mut r = Registry::new();
                r.counter_add("requests_total", &[("route", "a\"b\\c\nd")], 3.0)
                    .unwrap();
                r.gauge_set("up", &[], 1.0).unwrap();
                r.observe("lat", &[], &[1.0, 2.0], 1.5).unwrap();
                let t = r.export();
                assert!(
                    t.contains("requests_total{route=\"a\\\"b\\\\c\\nd\"} 3\n"),
                    "{t}"
                );
                assert!(t.contains("up 1\n"));
                assert!(
                    t.contains("lat_bucket{le=\"1\"} 0\n")
                        && t.contains("lat_bucket{le=\"2\"} 1\n"),
                    "{t}"
                );
                assert!(
                    t.contains("lat_bucket{le=\"+Inf\"} 1\n")
                        && t.contains("lat_sum 1.5\n")
                        && t.contains("lat_count 1\n"),
                    "{t}"
                );
                assert!(t.lines().all(|l| !l.is_empty()));
                assert_eq!(
                    t.lines().count(),
                    7,
                    "one line per sample, none forged by the label text: {t}"
                );
            }

            #[test]
            fn long_label_values_are_cut() {
                let mut r = Registry::new();
                let long = "é".repeat(500);
                r.counter_add("c", &[("v", &long)], 1.0).unwrap();
                let s = &r.all_of("c")[0];
                assert_eq!(s.labels["v"].chars().count(), MAX_LABEL_VALUE);
            }
        }
    }

    /// Health checks.
    pub mod health {
        use super::MonitorError;
        use std::collections::BTreeMap;

        /// Most checks kept.
        pub const MAX_CHECKS: usize = 1_000;

        /// How a part of the system is doing, best first.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Status {
            /// Working.
            Ok,
            /// Working with a problem.
            Degraded,
            /// Not working, or has stopped reporting.
            Down,
        }

        /// The last report of one check.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Check {
            /// What it said.
            pub status: Status,
            /// A short explanation.
            pub detail: String,
            /// When it said it.
            pub at: u64,
        }

        /// All checks.
        #[derive(Debug, Clone)]
        pub struct Board {
            checks: BTreeMap<String, Check>,
            max_age: u64,
        }

        impl Board {
            /// A board where a check that has not reported for `max_age` seconds counts as down.
            #[must_use]
            pub fn new(max_age: u64) -> Self {
                Self {
                    checks: BTreeMap::new(),
                    max_age,
                }
            }

            /// Records the latest state of a check.
            ///
            /// # Errors
            /// [`MonitorError`] for a bad name or when the limit is reached.
            pub fn report(
                &mut self,
                name: &str,
                status: Status,
                detail: &str,
                now: u64,
            ) -> Result<(), MonitorError> {
                if !super::valid_name(name) {
                    return Err(MonitorError::BadName("check name"));
                }
                if !self.checks.contains_key(name) && self.checks.len() >= MAX_CHECKS {
                    return Err(MonitorError::Full("checks"));
                }
                self.checks.insert(
                    name.to_string(),
                    Check {
                        status,
                        detail: detail
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(200)
                            .collect(),
                        at: now,
                    },
                );
                Ok(())
            }

            /// The state of one check as of `now`: a stale report reads as [`Status::Down`].
            #[must_use]
            pub fn status(&self, name: &str, now: u64) -> Option<Status> {
                self.checks.get(name).map(|c| self.effective(c, now))
            }

            fn effective(&self, c: &Check, now: u64) -> Status {
                if now.saturating_sub(c.at) > self.max_age {
                    Status::Down
                } else {
                    c.status
                }
            }

            /// The worst state of all checks (`Ok` when there are none).
            #[must_use]
            pub fn overall(&self, now: u64) -> Status {
                self.checks
                    .values()
                    .map(|c| self.effective(c, now))
                    .max()
                    .unwrap_or(Status::Ok)
            }

            /// Names of checks that are not `Ok`, worst first.
            #[must_use]
            pub fn problems(&self, now: u64) -> Vec<(String, Status, String)> {
                let mut v: Vec<(String, Status, String)> = self
                    .checks
                    .iter()
                    .filter_map(|(n, c)| {
                        let s = self.effective(c, now);
                        (s != Status::Ok).then(|| {
                            let why = if s == Status::Down && c.status != Status::Down {
                                format!("no report for {} s", now.saturating_sub(c.at))
                            } else {
                                c.detail.clone()
                            };
                            (n.clone(), s, why)
                        })
                    })
                    .collect();
                v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                v
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn the_overall_state_is_the_worst_one() {
                let mut b = Board::new(60);
                assert_eq!(b.overall(0), Status::Ok, "nothing to worry about yet");
                b.report("llm", Status::Ok, "", 10).unwrap();
                b.report("disk", Status::Degraded, "90% full", 10).unwrap();
                assert_eq!(b.overall(20), Status::Degraded);
                b.report("net", Status::Down, "no carrier", 15).unwrap();
                assert_eq!(b.overall(20), Status::Down);
                b.report("net", Status::Ok, "", 25).unwrap();
                assert_eq!(
                    b.overall(30),
                    Status::Degraded,
                    "a later report replaces the earlier one"
                );
            }

            #[test]
            fn a_check_that_stops_reporting_goes_down() {
                let mut b = Board::new(30);
                b.report("llm", Status::Ok, "", 100).unwrap();
                assert_eq!(b.status("llm", 130), Some(Status::Ok));
                assert_eq!(b.status("llm", 131), Some(Status::Down));
                let p = b.problems(200);
                assert_eq!(p.len(), 1);
                assert!(p[0].2.contains("no report for 100 s"), "{p:?}");
                assert_eq!(b.status("nothing", 0), None);
                assert_eq!(
                    b.status("llm", 50),
                    Some(Status::Ok),
                    "a clock that went back is not stale"
                );
            }

            #[test]
            fn problems_are_listed_worst_first_with_clean_details() {
                let mut b = Board::new(1000);
                b.report("a_ok", Status::Ok, "fine", 0).unwrap();
                b.report("b_slow", Status::Degraded, "slow\u{1b}[31m", 0)
                    .unwrap();
                b.report("c_dead", Status::Down, "gone", 0).unwrap();
                let p = b.problems(1);
                assert_eq!(
                    p.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
                    vec!["c_dead", "b_slow"]
                );
                assert!(!p[1].2.contains('\u{1b}'));
            }

            #[test]
            fn names_and_limits_are_enforced() {
                let mut b = Board::new(1);
                assert!(b.report("Bad Name", Status::Ok, "", 0).is_err());
                for i in 0..MAX_CHECKS {
                    b.report(&format!("c{i}"), Status::Ok, "", 0).unwrap();
                }
                assert_eq!(
                    b.report("one_more", Status::Ok, "", 0),
                    Err(MonitorError::Full("checks"))
                );
                assert!(b.report("c5", Status::Down, "still allowed", 0).is_ok());
            }
        }
    }

    /// Service-level objectives.
    pub mod slo {
        use super::MonitorError;

        /// An objective such as "99 percent of requests succeed over the last hour".
        ///
        /// Events are kept as per-minute buckets, so memory is bounded by the window.
        #[derive(Debug, Clone)]
        pub struct Slo {
            target_ppm: u64,
            window_minutes: usize,
            /// `(minute, good, bad)`, oldest first.
            buckets: std::collections::VecDeque<(u64, u64, u64)>,
        }

        impl Slo {
            /// An objective that `target` (0 to 1, for example 0.99) of events are good over
            /// `window_minutes` (1 to 10,080: up to a week).
            ///
            /// # Errors
            /// [`MonitorError::BadValue`] for a target outside (0, 1) or a window outside the range.
            pub fn new(target: f64, window_minutes: usize) -> Result<Self, MonitorError> {
                if !(target > 0.0 && target < 1.0) || window_minutes == 0 || window_minutes > 10_080
                {
                    return Err(MonitorError::BadValue);
                }
                Ok(Self {
                    // Parts per million, rounded.
                    target_ppm: (target * 1_000_000.0).round() as u64,
                    window_minutes,
                    buckets: std::collections::VecDeque::new(),
                })
            }

            fn trim(&mut self, minute: u64) {
                let oldest = minute.saturating_sub(self.window_minutes as u64 - 1);
                while self.buckets.front().is_some_and(|b| b.0 < oldest) {
                    self.buckets.pop_front();
                }
            }

            /// Records one event at `now` (seconds).
            pub fn record(&mut self, good: bool, now: u64) {
                let minute = now / 60;
                self.trim(minute);
                match self.buckets.back_mut() {
                    Some(b) if b.0 == minute => {
                        if good {
                            b.1 += 1;
                        } else {
                            b.2 += 1;
                        }
                    }
                    Some(b) if b.0 > minute => {} // an event from the past: not counted
                    _ => self
                        .buckets
                        .push_back((minute, u64::from(good), u64::from(!good))),
                }
            }

            /// `(good, bad)` events inside the window ending at `now`.
            #[must_use]
            pub fn counts(&self, now: u64) -> (u64, u64) {
                let oldest = (now / 60).saturating_sub(self.window_minutes as u64 - 1);
                self.buckets
                    .iter()
                    .filter(|b| b.0 >= oldest)
                    .fold((0, 0), |a, b| (a.0 + b.1, a.1 + b.2))
            }

            /// The fraction of good events in the window, or `None` with no events.
            #[must_use]
            pub fn compliance(&self, now: u64) -> Option<f64> {
                let (g, b) = self.counts(now);
                (g + b > 0).then(|| g as f64 / (g + b) as f64)
            }

            /// Whether the objective is met (true with no events).
            #[must_use]
            pub fn met(&self, now: u64) -> bool {
                let (g, b) = self.counts(now);
                // g / (g + b) >= target, in integers.
                (g + b == 0)
                    || (u128::from(g) * 1_000_000
                        >= u128::from(self.target_ppm) * u128::from(g + b))
            }

            /// The share of the error budget still unspent, from 1 (none spent) down to 0 (spent or
            /// overspent). The budget is the bad events the objective allows for the events seen.
            #[must_use]
            pub fn budget_remaining(&self, now: u64) -> f64 {
                let (g, b) = self.counts(now);
                let total = g + b;
                if total == 0 {
                    return 1.0;
                }
                let allowed = total as f64 * (1_000_000 - self.target_ppm) as f64 / 1_000_000.0;
                if allowed <= 0.0 {
                    return 0.0;
                }
                (1.0 - b as f64 / allowed).clamp(0.0, 1.0)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn compliance_and_the_objective_over_the_window() {
                let mut s = Slo::new(0.99, 60).unwrap();
                assert_eq!(s.compliance(0), None);
                assert!(s.met(0), "no events, nothing violated");
                for i in 0..99 {
                    s.record(true, i);
                }
                s.record(false, 99);
                assert_eq!(s.counts(100), (99, 1));
                assert!((s.compliance(100).unwrap() - 0.99).abs() < 1e-12);
                assert!(s.met(100), "exactly 99% meets a 99% objective");
                s.record(false, 100);
                assert!(!s.met(101));
            }

            #[test]
            fn old_events_leave_the_window() {
                let mut s = Slo::new(0.9, 10).unwrap();
                for i in 0..5 {
                    s.record(false, i * 60);
                }
                assert_eq!(s.counts(300), (0, 5));
                s.record(true, 20 * 60);
                assert_eq!(
                    s.counts(20 * 60),
                    (1, 0),
                    "minutes 0 to 4 are outside a 10-minute window ending at minute 20"
                );
                assert!(s.met(20 * 60));
                // A late event from before the newest bucket is ignored, not misplaced.
                s.record(false, 60);
                assert_eq!(s.counts(20 * 60), (1, 0));
            }

            #[test]
            fn the_error_budget_drains_as_failures_arrive() {
                let mut s = Slo::new(0.9, 60).unwrap();
                assert_eq!(s.budget_remaining(0), 1.0);
                for i in 0..100 {
                    s.record(i % 20 != 0, i); // 5 failures in 100: half the 10% budget
                }
                assert!(
                    (s.budget_remaining(100) - 0.5).abs() < 1e-9,
                    "{}",
                    s.budget_remaining(100)
                );
                for _ in 0..10 {
                    s.record(false, 100);
                }
                assert_eq!(s.budget_remaining(100), 0.0, "never below zero");
            }

            #[test]
            fn bad_objectives_are_refused_and_memory_is_bounded() {
                for (t, w) in [
                    (0.0, 10),
                    (1.0, 10),
                    (-0.5, 10),
                    (f64::NAN, 10),
                    (0.9, 0),
                    (0.9, 10_081),
                ] {
                    assert!(Slo::new(t, w).is_err(), "{t} {w}");
                }
                let mut s = Slo::new(0.9, 5).unwrap();
                for m in 0..10_000u64 {
                    s.record(true, m * 60);
                }
                assert!(s.buckets.len() <= 5);
            }
        }
    }

    /// Alerts.
    pub mod alert {
        use super::metrics::Registry;
        use super::MonitorError;
        use std::collections::{BTreeMap, VecDeque};

        /// Most rules.
        pub const MAX_RULES: usize = 500;
        /// Most events kept in the history.
        pub const MAX_HISTORY: usize = 1_000;

        /// How serious an alert is, least first.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Severity {
            /// Worth knowing.
            Info,
            /// Needs attention soon.
            Warning,
            /// Needs attention now.
            Critical,
        }

        /// When a rule's condition holds.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Op {
            /// The metric is above the threshold.
            Above,
            /// The metric is below the threshold.
            Below,
        }

        /// A rule: a metric compared with a threshold, which must hold for `for_secs` before the
        /// alert fires.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Rule {
            /// The rule's name (also the alert's name).
            pub name: String,
            /// The metric to watch (a counter or gauge; the sum over all its series).
            pub metric: String,
            /// The comparison.
            pub op: Op,
            /// The threshold.
            pub threshold: f64,
            /// Seconds the condition must keep holding before it fires.
            pub for_secs: u64,
            /// How serious it is.
            pub severity: Severity,
        }

        /// What happened to an alert.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Change {
            /// The condition started to hold (not firing yet).
            Pending,
            /// It held long enough.
            Firing,
            /// It stopped holding after firing.
            Resolved,
        }

        /// A change of one alert, as reported by [`Alerter::evaluate`] and kept in the history.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Event {
            /// The rule's name.
            pub name: String,
            /// What changed.
            pub change: Change,
            /// How serious it is.
            pub severity: Severity,
            /// The metric's value when it changed.
            pub value: f64,
            /// When.
            pub at: u64,
        }

        #[derive(Debug, Clone, Copy)]
        enum State {
            Clear,
            Pending(u64),
            Firing,
        }

        /// Evaluates rules against a [`Registry`].
        #[derive(Debug, Clone, Default)]
        pub struct Alerter {
            rules: Vec<Rule>,
            state: BTreeMap<String, State>,
            history: VecDeque<Event>,
        }

        impl Alerter {
            /// An alerter with no rules.
            #[must_use]
            pub fn new() -> Self {
                Self::default()
            }

            /// Adds a rule.
            ///
            /// # Errors
            /// [`MonitorError`] for a bad or repeated name, a bad metric name, a non-finite
            /// threshold, or too many rules.
            pub fn add(&mut self, rule: Rule) -> Result<(), MonitorError> {
                if !super::valid_name(&rule.name) || !super::valid_name(&rule.metric) {
                    return Err(MonitorError::BadName("rule or metric name"));
                }
                if !rule.threshold.is_finite() {
                    return Err(MonitorError::BadValue);
                }
                if self.rules.iter().any(|r| r.name == rule.name) {
                    return Err(MonitorError::BadName("repeated rule name"));
                }
                if self.rules.len() >= MAX_RULES {
                    return Err(MonitorError::Full("rules"));
                }
                self.state.insert(rule.name.clone(), State::Clear);
                self.rules.push(rule);
                Ok(())
            }

            /// Checks every rule against `metrics` at `now` and returns what changed.
            pub fn evaluate(&mut self, metrics: &Registry, now: u64) -> Vec<Event> {
                let mut events = Vec::new();
                for r in &self.rules {
                    let series = metrics.all_of(&r.metric);
                    let value: f64 = series
                        .iter()
                        .map(|s| match &s.value {
                            super::metrics::Value::Counter(v) | super::metrics::Value::Gauge(v) => {
                                *v
                            }
                            super::metrics::Value::Histogram { .. } => 0.0,
                        })
                        .sum();
                    // A metric that does not exist cannot trigger an alert.
                    let holds = !series.is_empty()
                        && match r.op {
                            Op::Above => value > r.threshold,
                            Op::Below => value < r.threshold,
                        };
                    let st = self.state.entry(r.name.clone()).or_insert(State::Clear);
                    let mut push = |change| {
                        events.push(Event {
                            name: r.name.clone(),
                            change,
                            severity: r.severity,
                            value,
                            at: now,
                        });
                    };
                    *st = match (*st, holds) {
                        (State::Clear, true) if r.for_secs == 0 => {
                            push(Change::Pending);
                            push(Change::Firing);
                            State::Firing
                        }
                        (State::Clear, true) => {
                            push(Change::Pending);
                            State::Pending(now)
                        }
                        (State::Pending(since), true) => {
                            if now.saturating_sub(since) >= r.for_secs {
                                push(Change::Firing);
                                State::Firing
                            } else {
                                State::Pending(since)
                            }
                        }
                        (State::Pending(_), false) => State::Clear,
                        (State::Firing, true) => State::Firing,
                        (State::Firing, false) => {
                            push(Change::Resolved);
                            State::Clear
                        }
                        (State::Clear, false) => State::Clear,
                    };
                }
                for e in &events {
                    if self.history.len() >= MAX_HISTORY {
                        self.history.pop_front();
                    }
                    self.history.push_back(e.clone());
                }
                events
            }

            /// Names of alerts that are firing now, most serious first.
            #[must_use]
            pub fn firing(&self) -> Vec<(&str, Severity)> {
                let mut v: Vec<(&str, Severity)> = self
                    .rules
                    .iter()
                    .filter(|r| matches!(self.state.get(&r.name), Some(State::Firing)))
                    .map(|r| (r.name.as_str(), r.severity))
                    .collect();
                v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                v
            }

            /// The recorded changes, oldest first.
            #[must_use]
            pub fn history(&self) -> Vec<&Event> {
                self.history.iter().collect()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn rule(name: &str, op: Op, threshold: f64, for_secs: u64) -> Rule {
                Rule {
                    name: name.into(),
                    metric: "temperature".into(),
                    op,
                    threshold,
                    for_secs,
                    severity: Severity::Warning,
                }
            }

            fn temp(v: f64) -> Registry {
                let mut r = Registry::new();
                r.gauge_set("temperature", &[], v).unwrap();
                r
            }

            #[test]
            fn an_alert_goes_pending_then_firing_then_resolved() {
                let mut a = Alerter::new();
                a.add(rule("hot", Op::Above, 80.0, 60)).unwrap();
                assert!(a.evaluate(&temp(70.0), 0).is_empty());
                let e = a.evaluate(&temp(90.0), 10);
                assert_eq!(
                    e.iter().map(|x| x.change).collect::<Vec<_>>(),
                    vec![Change::Pending]
                );
                assert!(a.evaluate(&temp(91.0), 40).is_empty(), "still waiting");
                assert!(a.firing().is_empty());
                let e = a.evaluate(&temp(92.0), 70);
                assert_eq!(e[0].change, Change::Firing);
                assert_eq!(e[0].value, 92.0);
                assert_eq!(a.firing(), vec![("hot", Severity::Warning)]);
                assert!(
                    a.evaluate(&temp(95.0), 80).is_empty(),
                    "firing is reported once"
                );
                let e = a.evaluate(&temp(60.0), 90);
                assert_eq!(e[0].change, Change::Resolved);
                assert!(a.firing().is_empty());
                assert_eq!(a.history().len(), 3);
            }

            #[test]
            fn a_blip_shorter_than_the_for_duration_never_fires() {
                let mut a = Alerter::new();
                a.add(rule("hot", Op::Above, 80.0, 60)).unwrap();
                a.evaluate(&temp(90.0), 0);
                assert!(
                    a.evaluate(&temp(50.0), 30).is_empty(),
                    "back to normal before it fired"
                );
                a.evaluate(&temp(90.0), 40);
                assert!(
                    a.evaluate(&temp(90.0), 80).is_empty(),
                    "the clock restarted at 40"
                );
                assert!(a.firing().is_empty());
                assert_eq!(a.evaluate(&temp(90.0), 100)[0].change, Change::Firing);
            }

            #[test]
            fn zero_for_fires_at_once_and_below_works() {
                let mut a = Alerter::new();
                a.add(rule("cold", Op::Below, 10.0, 0)).unwrap();
                let e = a.evaluate(&temp(5.0), 0);
                assert_eq!(
                    e.iter().map(|x| x.change).collect::<Vec<_>>(),
                    vec![Change::Pending, Change::Firing]
                );
                assert!(
                    a.evaluate(&temp(10.0), 1)
                        .iter()
                        .any(|x| x.change == Change::Resolved),
                    "equal to the threshold is not below"
                );
            }

            #[test]
            fn a_missing_metric_does_not_alert_and_labelled_series_are_summed() {
                let mut a = Alerter::new();
                a.add(rule("hot", Op::Above, 100.0, 0)).unwrap();
                assert!(a.evaluate(&Registry::new(), 0).is_empty());
                let mut r = Registry::new();
                r.gauge_set("temperature", &[("core", "0")], 60.0).unwrap();
                r.gauge_set("temperature", &[("core", "1")], 60.0).unwrap();
                assert_eq!(
                    a.evaluate(&r, 1).last().unwrap().change,
                    Change::Firing,
                    "60 + 60 is above 100"
                );
            }

            #[test]
            fn firing_is_listed_most_serious_first_and_rules_are_validated() {
                let mut a = Alerter::new();
                let mut warn = rule("b_warn", Op::Above, 1.0, 0);
                warn.severity = Severity::Warning;
                let mut crit = rule("a_crit", Op::Above, 1.0, 0);
                crit.severity = Severity::Critical;
                a.add(warn).unwrap();
                a.add(crit).unwrap();
                a.evaluate(&temp(5.0), 0);
                assert_eq!(
                    a.firing(),
                    vec![
                        ("a_crit", Severity::Critical),
                        ("b_warn", Severity::Warning)
                    ]
                );
                assert!(
                    a.add(rule("b_warn", Op::Above, 1.0, 0)).is_err(),
                    "repeated name"
                );
                assert!(a.add(rule("Bad Name", Op::Above, 1.0, 0)).is_err());
                assert!(a.add(rule("nan", Op::Above, f64::NAN, 0)).is_err());
                let mut m = rule("bad_metric", Op::Above, 1.0, 0);
                m.metric = "Not Valid".into();
                assert!(a.add(m).is_err());
            }

            #[test]
            fn the_history_and_rule_count_are_bounded() {
                let mut a = Alerter::new();
                a.add(rule("flap", Op::Above, 1.0, 0)).unwrap();
                for i in 0..2_000u64 {
                    a.evaluate(&temp(if i % 2 == 0 { 5.0 } else { 0.0 }), i);
                }
                assert!(a.history().len() <= MAX_HISTORY);
                let mut b = Alerter::new();
                for i in 0..MAX_RULES {
                    b.add(rule(&format!("r{i}"), Op::Above, 1.0, 0)).unwrap();
                }
                assert_eq!(
                    b.add(rule("overflow", Op::Above, 1.0, 0)),
                    Err(MonitorError::Full("rules"))
                );
            }
        }
    }
}

/// Billing: usage metering, plans and quotas, and invoices.
///
/// What this is: a deterministic calculation engine. Usage events (tokens, compute seconds,
/// storage, calls) are recorded once each (a repeated event id is not counted twice), checked
/// against a plan's quotas, and turned into invoices with exact integer arithmetic: money is in
/// millionths of a currency unit, rounding is half-up and happens once per line, and nothing
/// uses floating point. Invoices are chained by SHA-256, so a changed or removed invoice is
/// detectable.
///
/// What this is not: there is no payment processing, no payment-provider connection, no
/// currency conversion and no tax-law logic (a single tax rate in basis points is applied to
/// the subtotal). The clock is never read: callers pass the time.
#[cfg(feature = "billing")]
#[allow(clippy::module_inception)]
pub mod billing {
    #![forbid(unsafe_code)]

    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, HashMap};
    use std::fmt;

    /// Largest amount of money accepted anywhere, in millionths (one trillion units).
    pub const MAX_MONEY: i64 = 1_000_000_000_000_000_000;
    /// Most usage events kept.
    pub const MAX_EVENTS: usize = 2_000_000;
    /// Most kinds of usage in a plan.
    pub const MAX_KINDS: usize = 64;

    /// Why a billing call was refused.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BillingError {
        /// A name, id or currency is invalid (the text says which).
        BadName(&'static str),
        /// A number is out of range (the text says which).
        BadValue(&'static str),
        /// The same event id was recorded before with different content.
        Conflict,
        /// A limit was reached (the text says which).
        Full(&'static str),
        /// An amount would be too large to represent.
        Overflow,
    }

    impl fmt::Display for BillingError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::BadName(w) => write!(f, "invalid {w}"),
                Self::BadValue(w) => write!(f, "invalid {w}"),
                Self::Conflict => {
                    f.write_str("that event id was already recorded with different content")
                }
                Self::Full(w) => write!(f, "billing limit reached: {w}"),
                Self::Overflow => f.write_str("amount too large"),
            }
        }
    }

    impl std::error::Error for BillingError {}

    fn label(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    fn id_ok(s: &str) -> bool {
        !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
    }

    /// Money in millionths of a currency unit (so 1,500,000 is 1.50).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
    pub struct Money(pub i64);

    impl Money {
        /// Whole currency units.
        ///
        /// # Errors
        /// [`BillingError::Overflow`] when the amount is too large.
        pub fn units(n: i64) -> Result<Self, BillingError> {
            n.checked_mul(1_000_000)
                .filter(|m| m.abs() <= MAX_MONEY)
                .map(Money)
                .ok_or(BillingError::Overflow)
        }

        /// The amount as text with at least two and at most six decimals: `12.50`, `0.003750`.
        #[must_use]
        pub fn text(self) -> String {
            let sign = if self.0 < 0 { "-" } else { "" };
            let a = self.0.unsigned_abs();
            let (whole, frac) = (a / 1_000_000, a % 1_000_000);
            let mut f = format!("{frac:06}");
            while f.len() > 2 && f.ends_with('0') {
                f.pop();
            }
            format!("{sign}{whole}.{f}")
        }
    }

    fn checked(v: i128) -> Result<Money, BillingError> {
        i64::try_from(v)
            .ok()
            .filter(|m| m.abs() <= MAX_MONEY)
            .map(Money)
            .ok_or(BillingError::Overflow)
    }

    /// Divides rounding half up (for non-negative numbers).
    fn div_round(n: i128, d: i128) -> i128 {
        (n + d / 2) / d
    }

    /// A price: `micros` for every `per` units of usage (for example 1,500 millionths per 1,000
    /// tokens).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Rate {
        /// Millionths of a currency unit.
        pub micros: i64,
        /// The block of usage the price is for (at least 1).
        pub per: u64,
    }

    impl Rate {
        /// The cost of `quantity` units, rounded half up once.
        ///
        /// # Errors
        /// [`BillingError`] for a bad rate or an amount too large.
        pub fn cost(&self, quantity: u64) -> Result<Money, BillingError> {
            if self.micros < 0 || self.per == 0 {
                return Err(BillingError::BadValue("rate"));
            }
            checked(div_round(
                i128::from(quantity) * i128::from(self.micros),
                i128::from(self.per),
            ))
        }
    }

    /// What a customer pays for and may use.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Plan {
        /// The plan's name.
        pub name: String,
        /// The currency, three capital letters.
        pub currency: String,
        /// The fixed fee for a billing period.
        pub base_fee: Money,
        /// Usage included in the base fee, by kind.
        pub included: BTreeMap<String, u64>,
        /// The price of usage beyond what is included, by kind. A kind with no price is free.
        pub overage: BTreeMap<String, Rate>,
        /// Usage that may never be exceeded in a period, by kind.
        pub hard_limit: BTreeMap<String, u64>,
    }

    impl Plan {
        /// A plan with a fixed fee and nothing else.
        ///
        /// # Errors
        /// [`BillingError`] for a bad name, currency or fee.
        pub fn new(name: &str, currency: &str, base_fee: Money) -> Result<Self, BillingError> {
            if !label(name) {
                return Err(BillingError::BadName("plan name"));
            }
            if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(BillingError::BadName("currency"));
            }
            if base_fee.0 < 0 || base_fee.0 > MAX_MONEY {
                return Err(BillingError::BadValue("base fee"));
            }
            Ok(Self {
                name: name.into(),
                currency: currency.into(),
                base_fee,
                included: BTreeMap::new(),
                overage: BTreeMap::new(),
                hard_limit: BTreeMap::new(),
            })
        }

        /// Sets what a kind includes, what it costs beyond that, and its hard limit.
        ///
        /// # Errors
        /// [`BillingError`] for a bad kind, a bad rate, a limit below the included amount, or too
        /// many kinds.
        pub fn kind(
            mut self,
            kind: &str,
            included: u64,
            overage: Option<Rate>,
            hard_limit: Option<u64>,
        ) -> Result<Self, BillingError> {
            if !label(kind) {
                return Err(BillingError::BadName("usage kind"));
            }
            let known = self.included.contains_key(kind);
            if !known && self.included.len() >= MAX_KINDS {
                return Err(BillingError::Full("kinds"));
            }
            if let Some(r) = overage {
                if r.micros < 0 || r.per == 0 || r.micros > MAX_MONEY {
                    return Err(BillingError::BadValue("rate"));
                }
                self.overage.insert(kind.into(), r);
            }
            if let Some(l) = hard_limit {
                if l < included {
                    return Err(BillingError::BadValue(
                        "hard limit below the included amount",
                    ));
                }
                self.hard_limit.insert(kind.into(), l);
            }
            self.included.insert(kind.into(), included);
            Ok(self)
        }
    }

    /// One thing that was used.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UsageEvent {
        /// A unique id from the source, so a repeated report is not counted twice.
        pub id: String,
        /// Who used it.
        pub tenant: String,
        /// What was used (`tokens_in`, `compute_seconds`, ...).
        pub kind: String,
        /// How much.
        pub quantity: u64,
        /// When (seconds since the Unix epoch).
        pub at: u64,
    }

    /// What recording an event did.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Recorded {
        /// It was new and is now counted.
        New,
        /// The same event had been recorded; nothing changed.
        Duplicate,
    }

    /// The record of usage.
    #[derive(Debug, Clone)]
    pub struct Ledger {
        events: Vec<UsageEvent>,
        by_id: HashMap<String, usize>,
        max_events: usize,
    }

    impl Default for Ledger {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Ledger {
        /// An empty ledger.
        #[must_use]
        pub fn new() -> Self {
            Self::with_limit(MAX_EVENTS)
        }

        /// An empty ledger that keeps at most `max_events` events (used by tests).
        #[must_use]
        pub fn with_limit(max_events: usize) -> Self {
            Self {
                events: Vec::new(),
                by_id: HashMap::new(),
                max_events,
            }
        }

        /// Number of events.
        #[must_use]
        pub fn len(&self) -> usize {
            self.events.len()
        }

        /// True when there are none.
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.events.is_empty()
        }

        /// Records an event once.
        ///
        /// # Errors
        /// [`BillingError::BadName`] for a bad id, tenant or kind; [`BillingError::Conflict`] when
        /// the id was recorded before with different content; [`BillingError::Full`] at the limit.
        pub fn record(&mut self, ev: UsageEvent) -> Result<Recorded, BillingError> {
            if !id_ok(&ev.id) {
                return Err(BillingError::BadName("event id"));
            }
            if !label(&ev.tenant) {
                return Err(BillingError::BadName("tenant"));
            }
            if !label(&ev.kind) {
                return Err(BillingError::BadName("usage kind"));
            }
            if let Some(&i) = self.by_id.get(&ev.id) {
                return if self.events[i] == ev {
                    Ok(Recorded::Duplicate)
                } else {
                    Err(BillingError::Conflict)
                };
            }
            if self.events.len() >= self.max_events {
                return Err(BillingError::Full("events"));
            }
            self.by_id.insert(ev.id.clone(), self.events.len());
            self.events.push(ev);
            Ok(Recorded::New)
        }

        /// Total used of `kind` by `tenant` from `from` (inclusive) to `to` (exclusive).
        #[must_use]
        pub fn usage(&self, tenant: &str, kind: &str, from: u64, to: u64) -> u128 {
            self.events
                .iter()
                .filter(|e| e.tenant == tenant && e.kind == kind && e.at >= from && e.at < to)
                .map(|e| u128::from(e.quantity))
                .sum()
        }

        /// The kinds `tenant` used in the period, sorted.
        #[must_use]
        pub fn kinds(&self, tenant: &str, from: u64, to: u64) -> Vec<String> {
            let mut k: Vec<String> = self
                .events
                .iter()
                .filter(|e| e.tenant == tenant && e.at >= from && e.at < to)
                .map(|e| e.kind.clone())
                .collect();
            k.sort();
            k.dedup();
            k
        }
    }

    /// The answer to "may this usage go ahead?".
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Decision {
        /// Yes, within what is included.
        Allowed,
        /// Yes, but at least 80 percent of what is included is used (percent used shown).
        NearLimit(u64),
        /// Yes, and the part beyond what is included will be charged.
        Billable,
        /// No: it would pass the hard limit (the limit shown).
        Denied(u64),
    }

    /// Whether `add` more of `kind` may go ahead, given `used` already in this period.
    #[must_use]
    pub fn check(plan: &Plan, kind: &str, used: u128, add: u64) -> Decision {
        let after = used + u128::from(add);
        if let Some(&limit) = plan.hard_limit.get(kind) {
            if after > u128::from(limit) {
                return Decision::Denied(limit);
            }
        }
        let included = u128::from(plan.included.get(kind).copied().unwrap_or(0));
        if after > included {
            return Decision::Billable;
        }
        if included > 0 && after * 100 >= included * 80 {
            return Decision::NearLimit(u64::try_from(after * 100 / included).unwrap_or(100));
        }
        Decision::Allowed
    }

    /// One line of an invoice.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Line {
        /// What it is for.
        pub description: String,
        /// Units used.
        pub used: u128,
        /// Units included in the plan.
        pub included: u64,
        /// Units charged for.
        pub billable: u128,
        /// The amount.
        pub amount: Money,
    }

    /// An invoice.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Invoice {
        /// A running number from the [`Invoicer`] (1, 2, 3, ...).
        pub number: u64,
        /// Who is billed.
        pub tenant: String,
        /// The plan's name.
        pub plan: String,
        /// The currency.
        pub currency: String,
        /// Period start (inclusive) and end (exclusive), seconds.
        pub period: (u64, u64),
        /// The lines.
        pub lines: Vec<Line>,
        /// The sum of the lines.
        pub subtotal: Money,
        /// Tax rate in hundredths of a percent (2,000 is 20 percent).
        pub tax_bp: u32,
        /// The tax.
        pub tax: Money,
        /// Subtotal plus tax.
        pub total: Money,
        /// The previous invoice's hash (all zeros for the first).
        pub prev_hash: String,
        /// This invoice's hash.
        pub hash: String,
    }

    impl Invoice {
        /// The invoice as text, the same every time for the same invoice. This is also what the
        /// hash covers (everything except the hash line itself).
        #[must_use]
        pub fn text(&self) -> String {
            let mut t = format!(
                "invoice {}\ntenant {}\nplan {}\ncurrency {}\nperiod {} {}\nprev {}\n",
                self.number,
                self.tenant,
                self.plan,
                self.currency,
                self.period.0,
                self.period.1,
                self.prev_hash
            );
            for l in &self.lines {
                t.push_str(&format!(
                    "line {} | used {} | included {} | billable {} | {}\n",
                    l.description,
                    l.used,
                    l.included,
                    l.billable,
                    l.amount.text()
                ));
            }
            t.push_str(&format!(
                "subtotal {}\ntax {} bp {}\ntotal {}\n",
                self.subtotal.text(),
                self.tax_bp,
                self.tax.text(),
                self.total.text()
            ));
            t
        }

        fn compute_hash(&self) -> String {
            let mut h = Sha256::new();
            h.update(self.text().as_bytes());
            h.finalize().iter().map(|b| format!("{b:02x}")).collect()
        }

        /// Whether the stored hash matches the content.
        #[must_use]
        pub fn hash_ok(&self) -> bool {
            self.hash == self.compute_hash()
        }
    }

    const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    /// Makes invoices and chains them.
    #[derive(Debug, Clone)]
    pub struct Invoicer {
        next: u64,
        last_hash: String,
    }

    impl Default for Invoicer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Invoicer {
        /// A new chain.
        #[must_use]
        pub fn new() -> Self {
            Self {
                next: 1,
                last_hash: ZERO_HASH.into(),
            }
        }

        /// Builds the invoice for `tenant` for `[from, to)` under `plan`, and adds it to the chain.
        ///
        /// # Errors
        /// [`BillingError`] for a bad tenant or period, a tax rate above 100 percent, or amounts too
        /// large.
        pub fn invoice(
            &mut self,
            ledger: &Ledger,
            tenant: &str,
            plan: &Plan,
            from: u64,
            to: u64,
            tax_bp: u32,
        ) -> Result<Invoice, BillingError> {
            if !label(tenant) {
                return Err(BillingError::BadName("tenant"));
            }
            if to <= from {
                return Err(BillingError::BadValue("period"));
            }
            if tax_bp > 10_000 {
                return Err(BillingError::BadValue("tax rate"));
            }
            let mut lines = vec![Line {
                description: format!("{} plan", plan.name),
                used: 1,
                included: 1,
                billable: 0,
                amount: plan.base_fee,
            }];
            let mut subtotal = i128::from(plan.base_fee.0);
            let mut kinds: Vec<String> = ledger.kinds(tenant, from, to);
            for k in plan.included.keys() {
                if !kinds.contains(k) {
                    kinds.push(k.clone());
                }
            }
            kinds.sort();
            for k in kinds {
                let used = ledger.usage(tenant, &k, from, to);
                let included = plan.included.get(&k).copied().unwrap_or(0);
                let billable = used.saturating_sub(u128::from(included));
                let amount = match plan.overage.get(&k) {
                    Some(rate) if billable > 0 => {
                        let b = u64::try_from(billable).map_err(|_| BillingError::Overflow)?;
                        rate.cost(b)?
                    }
                    _ => Money(0),
                };
                if used == 0 && amount.0 == 0 && included == 0 {
                    continue;
                }
                subtotal += i128::from(amount.0);
                lines.push(Line {
                    description: k,
                    used,
                    included,
                    billable,
                    amount,
                });
            }
            let subtotal = checked(subtotal)?;
            let tax = checked(div_round(
                i128::from(subtotal.0) * i128::from(tax_bp),
                10_000,
            ))?;
            let total = checked(i128::from(subtotal.0) + i128::from(tax.0))?;
            let mut inv = Invoice {
                number: self.next,
                tenant: tenant.into(),
                plan: plan.name.clone(),
                currency: plan.currency.clone(),
                period: (from, to),
                lines,
                subtotal,
                tax_bp,
                tax,
                total,
                prev_hash: self.last_hash.clone(),
                hash: String::new(),
            };
            inv.hash = inv.compute_hash();
            self.next += 1;
            self.last_hash = inv.hash.clone();
            Ok(inv)
        }
    }

    /// Checks a run of invoices: numbers count up from 1, each hash matches its content, and each
    /// invoice names the previous one's hash. Returns the 1-based position of the first bad
    /// invoice.
    ///
    /// # Errors
    /// The position of the first invoice that breaks the chain.
    pub fn verify_chain(invoices: &[Invoice]) -> Result<(), usize> {
        let mut prev = ZERO_HASH.to_string();
        for (i, inv) in invoices.iter().enumerate() {
            if inv.number != i as u64 + 1 || inv.prev_hash != prev || !inv.hash_ok() {
                return Err(i + 1);
            }
            prev.clone_from(&inv.hash);
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn ev(id: &str, tenant: &str, kind: &str, q: u64, at: u64) -> UsageEvent {
            UsageEvent {
                id: id.into(),
                tenant: tenant.into(),
                kind: kind.into(),
                quantity: q,
                at,
            }
        }

        fn plan() -> Plan {
            Plan::new("team", "USD", Money::units(20).unwrap())
                .unwrap()
                .kind(
                    "tokens_out",
                    1_000_000,
                    Some(Rate {
                        micros: 1_500,
                        per: 1_000,
                    }),
                    Some(5_000_000),
                )
                .unwrap()
                .kind(
                    "storage_gb_hours",
                    100,
                    Some(Rate {
                        micros: 2_000,
                        per: 1,
                    }),
                    None,
                )
                .unwrap()
        }

        #[test]
        fn money_is_shown_exactly() {
            for (m, t) in [
                (0, "0.00"),
                (1_500_000, "1.50"),
                (3_750, "0.00375"),
                (1, "0.000001"),
                (-2_500_000, "-2.50"),
                (12_000_000, "12.00"),
                (999_999, "0.999999"),
            ] {
                assert_eq!(Money(m).text(), t, "{m}");
            }
            assert_eq!(Money::units(20).unwrap(), Money(20_000_000));
            assert_eq!(Money::units(i64::MAX), Err(BillingError::Overflow));
        }

        #[test]
        fn rates_round_half_up_once_and_refuse_overflow() {
            let r = Rate {
                micros: 1_500,
                per: 1_000,
            };
            assert_eq!(r.cost(0).unwrap(), Money(0));
            assert_eq!(r.cost(1_000).unwrap(), Money(1_500));
            assert_eq!(r.cost(2_500).unwrap(), Money(3_750));
            assert_eq!(r.cost(1).unwrap(), Money(2), "1.5 rounds up to 2");
            assert_eq!(
                Rate { micros: 1, per: 3 }.cost(1).unwrap(),
                Money(0),
                "0.33 rounds down"
            );
            assert_eq!(
                Rate { micros: 1, per: 2 }.cost(1).unwrap(),
                Money(1),
                "0.5 rounds up"
            );
            assert_eq!(
                Rate {
                    micros: MAX_MONEY,
                    per: 1
                }
                .cost(u64::MAX),
                Err(BillingError::Overflow)
            );
            assert!(Rate { micros: -1, per: 1 }.cost(1).is_err());
            assert!(Rate { micros: 1, per: 0 }.cost(1).is_err());
        }

        #[test]
        fn an_event_is_counted_once_and_a_conflict_is_refused() {
            let mut l = Ledger::new();
            assert_eq!(
                l.record(ev("e1", "acme", "tokens_out", 100, 5)),
                Ok(Recorded::New)
            );
            assert_eq!(
                l.record(ev("e1", "acme", "tokens_out", 100, 5)),
                Ok(Recorded::Duplicate)
            );
            assert_eq!(
                l.record(ev("e1", "acme", "tokens_out", 999, 5)),
                Err(BillingError::Conflict)
            );
            assert_eq!(l.len(), 1);
            assert_eq!(l.usage("acme", "tokens_out", 0, 10), 100);
            for bad in [
                ev("", "acme", "k", 1, 0),
                ev("a\nb", "acme", "k", 1, 0),
                ev("x", "Acme", "k", 1, 0),
                ev("x", "acme", "K K", 1, 0),
                ev("x", "", "k", 1, 0),
            ] {
                assert!(matches!(l.record(bad), Err(BillingError::BadName(_))));
            }
        }

        #[test]
        fn usage_is_per_tenant_kind_and_period_with_the_end_excluded() {
            let mut l = Ledger::new();
            l.record(ev("a", "acme", "tokens_out", 10, 100)).unwrap();
            l.record(ev("b", "acme", "tokens_out", 20, 199)).unwrap();
            l.record(ev("c", "acme", "tokens_out", 40, 200)).unwrap();
            l.record(ev("d", "other", "tokens_out", 80, 150)).unwrap();
            l.record(ev("e", "acme", "tokens_in", 160, 150)).unwrap();
            assert_eq!(
                l.usage("acme", "tokens_out", 100, 200),
                30,
                "start included, end excluded"
            );
            assert_eq!(l.usage("other", "tokens_out", 100, 200), 80);
            assert_eq!(l.usage("acme", "tokens_in", 0, 1000), 160);
            assert_eq!(l.usage("nobody", "tokens_out", 0, 1000), 0);
            assert_eq!(l.kinds("acme", 100, 200), vec!["tokens_in", "tokens_out"]);
            let mut big = Ledger::new();
            big.record(ev("x", "acme", "k", u64::MAX, 0)).unwrap();
            big.record(ev("y", "acme", "k", u64::MAX, 0)).unwrap();
            assert_eq!(
                big.usage("acme", "k", 0, 1),
                u128::from(u64::MAX) * 2,
                "no wrap-around"
            );
        }

        #[test]
        fn the_ledger_is_bounded() {
            let mut l = Ledger::with_limit(2);
            l.record(ev("1", "t", "k", 1, 0)).unwrap();
            l.record(ev("2", "t", "k", 1, 0)).unwrap();
            assert_eq!(
                l.record(ev("3", "t", "k", 1, 0)),
                Err(BillingError::Full("events"))
            );
            assert_eq!(
                l.record(ev("2", "t", "k", 1, 0)),
                Ok(Recorded::Duplicate),
                "a repeat still works when full"
            );
        }

        #[test]
        fn plans_are_validated() {
            assert!(Plan::new("Bad Name", "USD", Money(0)).is_err());
            assert!(Plan::new("p", "usd", Money(0)).is_err());
            assert!(Plan::new("p", "US", Money(0)).is_err());
            assert!(Plan::new("p", "USD", Money(-1)).is_err());
            let p = Plan::new("p", "USD", Money(0)).unwrap();
            assert!(p.clone().kind("Bad", 0, None, None).is_err());
            assert!(
                p.clone().kind("k", 100, None, Some(99)).is_err(),
                "limit below included"
            );
            assert!(p
                .clone()
                .kind("k", 0, Some(Rate { micros: -1, per: 1 }), None)
                .is_err());
            assert!(p
                .clone()
                .kind("k", 0, Some(Rate { micros: 1, per: 0 }), None)
                .is_err());
            let mut q = p;
            for i in 0..MAX_KINDS {
                q = q.kind(&format!("k{i}"), 0, None, None).unwrap();
            }
            assert!(q.kind("one_more", 0, None, None).is_err());
        }

        #[test]
        fn quotas_allow_warn_charge_or_deny() {
            let p = plan();
            assert_eq!(check(&p, "tokens_out", 0, 100), Decision::Allowed);
            assert_eq!(check(&p, "tokens_out", 799_999, 1), Decision::NearLimit(80));
            assert_eq!(
                check(&p, "tokens_out", 900_000, 50_000),
                Decision::NearLimit(95)
            );
            assert_eq!(check(&p, "tokens_out", 1_000_000, 1), Decision::Billable);
            assert_eq!(check(&p, "tokens_out", 4_999_999, 1), Decision::Billable);
            assert_eq!(
                check(&p, "tokens_out", 4_999_999, 2),
                Decision::Denied(5_000_000)
            );
            assert_eq!(
                check(&p, "unknown_kind", 0, 5),
                Decision::Billable,
                "nothing included, no limit"
            );
            assert_eq!(
                check(&p, "storage_gb_hours", 0, u64::MAX),
                Decision::Billable
            );
        }

        #[test]
        fn an_invoice_adds_the_base_fee_overage_and_tax() {
            let mut l = Ledger::new();
            l.record(ev("1", "acme", "tokens_out", 2_500_000, 10))
                .unwrap(); // 1,500,000 over
            l.record(ev("2", "acme", "storage_gb_hours", 90, 20))
                .unwrap(); // inside the 100 included
            l.record(ev("3", "acme", "tokens_out", 999, 5_000)).unwrap(); // outside the period
            l.record(ev("4", "other", "tokens_out", 999_999_999, 10))
                .unwrap(); // another tenant
            let mut inv = Invoicer::new();
            let i = inv.invoice(&l, "acme", &plan(), 0, 1_000, 2_000).unwrap();
            assert_eq!(i.number, 1);
            assert_eq!(i.lines[0].amount, Money(20_000_000));
            let out = i
                .lines
                .iter()
                .find(|x| x.description == "tokens_out")
                .unwrap();
            assert_eq!(
                (out.used, out.included, out.billable),
                (2_500_000, 1_000_000, 1_500_000)
            );
            assert_eq!(
                out.amount,
                Money(2_250_000),
                "1,500 per 1,000 tokens: 1,500,000 tokens"
            );
            let st = i
                .lines
                .iter()
                .find(|x| x.description == "storage_gb_hours")
                .unwrap();
            assert_eq!((st.billable, st.amount), (0, Money(0)));
            assert_eq!(i.subtotal, Money(22_250_000));
            assert_eq!(i.tax, Money(4_450_000), "20% of 22.25");
            assert_eq!(i.total, Money(26_700_000));
            assert!(i.text().contains("total 26.70") && i.hash_ok());
        }

        #[test]
        fn no_usage_bills_just_the_base_fee_and_tax_rounds_half_up() {
            let l = Ledger::new();
            let p = Plan::new("tiny", "EUR", Money(1)).unwrap(); // 0.000001
            let mut inv = Invoicer::new();
            let i = inv.invoice(&l, "acme", &p, 0, 10, 5_000).unwrap(); // 50%: 0.5 millionth rounds up
            assert_eq!((i.subtotal, i.tax, i.total), (Money(1), Money(1), Money(2)));
            assert_eq!(i.lines.len(), 1);
            let free = Plan::new("free", "USD", Money(0)).unwrap();
            let j = inv.invoice(&l, "acme", &free, 0, 10, 0).unwrap();
            assert_eq!(j.total, Money(0));
        }

        #[test]
        fn invoices_are_deterministic_and_validated() {
            let mut l = Ledger::new();
            l.record(ev("1", "acme", "tokens_out", 1_234_567, 1))
                .unwrap();
            let a = Invoicer::new()
                .invoice(&l, "acme", &plan(), 0, 10, 700)
                .unwrap();
            let b = Invoicer::new()
                .invoice(&l, "acme", &plan(), 0, 10, 700)
                .unwrap();
            assert_eq!(a, b);
            let mut inv = Invoicer::new();
            assert!(inv.invoice(&l, "Acme", &plan(), 0, 10, 0).is_err());
            assert!(
                inv.invoice(&l, "acme", &plan(), 10, 10, 0).is_err(),
                "an empty period"
            );
            assert!(inv.invoice(&l, "acme", &plan(), 20, 10, 0).is_err());
            assert!(
                inv.invoice(&l, "acme", &plan(), 0, 10, 10_001).is_err(),
                "tax over 100%"
            );
            assert_eq!(
                inv.invoice(&l, "acme", &plan(), 0, 10, 0).unwrap().number,
                1,
                "failures do not use a number"
            );
        }

        #[test]
        fn huge_usage_is_refused_not_wrapped() {
            let mut l = Ledger::new();
            for i in 0..4 {
                l.record(ev(&i.to_string(), "acme", "tokens_out", u64::MAX, 0))
                    .unwrap();
            }
            let r = Invoicer::new().invoice(&l, "acme", &plan(), 0, 10, 0);
            assert_eq!(r, Err(BillingError::Overflow));
        }

        #[test]
        fn the_chain_detects_changes_removals_and_reordering() {
            let mut l = Ledger::new();
            l.record(ev("1", "acme", "tokens_out", 2_000_000, 1))
                .unwrap();
            let mut inv = Invoicer::new();
            let all: Vec<Invoice> = (0..4)
                .map(|_| inv.invoice(&l, "acme", &plan(), 0, 10, 0).unwrap())
                .collect();
            assert_eq!(verify_chain(&all), Ok(()));
            assert_eq!(verify_chain(&[]), Ok(()));
            // Changing an amount without fixing the hash.
            let mut t = all.clone();
            t[1].lines[0].amount = Money(1);
            assert_eq!(verify_chain(&t), Err(2));
            // Changing and re-hashing one invoice breaks the next link.
            let mut t = all.clone();
            t[1].tax_bp = 1;
            t[1].hash = t[1].compute_hash();
            assert_eq!(verify_chain(&t), Err(3));
            // Removing one.
            let mut t = all.clone();
            t.remove(1);
            assert_eq!(verify_chain(&t), Err(2));
            // Reordering.
            let mut t = all.clone();
            t.swap(1, 2);
            assert_eq!(verify_chain(&t), Err(2));
            // Dropping the first.
            assert_eq!(verify_chain(&all[1..]), Err(1));
        }
    }
}

/// Bring-your-own-key management: an encrypted vault for API keys.
///
/// People bring their own keys for hosted models. The vault keeps any number of them on the data
/// disk, **encrypted with a key made from a passphrase** (PBKDF2-HMAC-SHA256, then
/// ChaCha20-Poly1305 per record), so a lost or copied stick does not give the keys away. On top of
/// storage it enforces how a key may be used:
///
/// * a key is **bound to hosts** (`api.openai.com`, `*.example.com`): handing it to any other
///   host is refused, so a compromised agent cannot send it somewhere else;
/// * it can **expire**, be **revoked**, be **rotated** (only a fingerprint of the old key is
///   kept), and have a **daily use limit**;
/// * the secret is only ever handed to a closure ([`keys::Vault::with_secret`]), never returned,
///   and prints as `[redacted]`;
/// * every use and every refusal is written to a hash-chained **audit trail** inside the vault.
///
/// Randomness is never read inside the engine: the host passes an [`keys::Entropy`] source
/// (`/dev/urandom` through [`keys::UrandomEntropy`]). The clock is never read: callers pass `now`.
///
/// Honest limits: the secret is overwritten when dropped, but only as a best effort (the allocator
/// may have copied it, and without `unsafe` the compiler is not forced to keep the writes); the
/// vault is **unlocked in memory** while open; an attacker who can replace the file with an *older
/// valid copy* is not detected (no counter outside the file); PBKDF2 is not memory-hard (a strong
/// passphrase matters).
#[cfg(feature = "keys")]
#[allow(clippy::module_inception)]
pub mod keys {
    #![forbid(unsafe_code)]

    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    use sha2::{Digest, Sha256};
    use std::fmt;

    /// Most keys in a vault.
    pub const MAX_KEYS: usize = 200;
    /// Most audit records kept.
    pub const MAX_AUDIT: usize = 1_000;
    /// Largest vault file accepted, in bytes.
    pub const MAX_FILE: usize = 4 * 1024 * 1024;
    /// The fewest PBKDF2 iterations accepted.
    pub const MIN_ITERATIONS: u32 = 10_000;
    /// The most PBKDF2 iterations accepted when opening (a guard against a file that would hang).
    pub const MAX_ITERATIONS: u32 = 10_000_000;
    /// The iterations used for new vaults.
    pub const STANDARD_ITERATIONS: u32 = 600_000;
    const MAGIC: &str = "mouli-vault 1";

    /// Why a vault call failed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum KeyError {
        /// An input is invalid (the text says which).
        Invalid(&'static str),
        /// No such key.
        NotFound,
        /// The passphrase is wrong (or the file's first record was changed).
        WrongPassphrase,
        /// The file was changed, truncated, reordered or built with another key.
        Tampered,
        /// The file is not a vault (line number, 1-based).
        Format(usize),
        /// The key is revoked.
        Revoked,
        /// The key has expired.
        Expired,
        /// The key may not be sent to this host.
        HostNotAllowed,
        /// The key's daily limit is used up.
        LimitReached,
        /// A limit was reached (the text says which).
        Full(&'static str),
        /// The entropy source failed.
        Entropy,
    }

    impl fmt::Display for KeyError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Invalid(w) => write!(f, "invalid {w}"),
                Self::NotFound => f.write_str("no such key"),
                Self::WrongPassphrase => f.write_str("wrong passphrase"),
                Self::Tampered => f.write_str("the key vault was changed or damaged"),
                Self::Format(n) => write!(f, "the key vault file is malformed (line {n})"),
                Self::Revoked => f.write_str("the key is revoked"),
                Self::Expired => f.write_str("the key has expired"),
                Self::HostNotAllowed => f.write_str("the key may not be sent to that host"),
                Self::LimitReached => f.write_str("the key's daily limit is used up"),
                Self::Full(w) => write!(f, "limit reached: {w}"),
                Self::Entropy => f.write_str("no random numbers available"),
            }
        }
    }

    impl std::error::Error for KeyError {}

    /// A source of random bytes, supplied by the host.
    pub trait Entropy {
        /// Fills `buf` with random bytes.
        ///
        /// # Errors
        /// [`KeyError::Entropy`] when none are available.
        fn fill(&mut self, buf: &mut [u8]) -> Result<(), KeyError>;
    }

    /// Random bytes from `/dev/urandom`.
    #[cfg(unix)]
    #[derive(Debug, Default)]
    pub struct UrandomEntropy;

    #[cfg(unix)]
    impl Entropy for UrandomEntropy {
        fn fill(&mut self, buf: &mut [u8]) -> Result<(), KeyError> {
            use std::io::Read;
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(buf))
                .map_err(|_| KeyError::Entropy)
        }
    }

    // ------------------------------------------------------------------ primitives

    fn sha256(parts: &[&[u8]]) -> [u8; 32] {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p);
        }
        h.finalize().into()
    }

    /// HMAC-SHA256.
    fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&sha256(&[key]));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
        let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
        let inner = sha256(&[&ipad, data]);
        sha256(&[&opad, &inner])
    }

    /// PBKDF2-HMAC-SHA256 producing 32 bytes.
    fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
        let mut s = salt.to_vec();
        s.extend_from_slice(&1u32.to_be_bytes());
        let mut u = hmac(password, &s);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac(password, &u);
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= b;
            }
        }
        t
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Option<Vec<u8>> {
        if !s.len().is_multiple_of(2) || !s.is_ascii() {
            return None;
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect()
    }

    fn esc(s: &str) -> String {
        let mut o = String::with_capacity(s.len());
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || b"-_./:@".contains(&b) {
                o.push(char::from(b));
            } else {
                o.push_str(&format!("%{b:02X}"));
            }
        }
        o
    }

    fn unesc(s: &str) -> Option<String> {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                let h = s.get(i + 1..i + 3)?;
                if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).ok()
    }

    // ------------------------------------------------------------------ secrets

    /// A secret that never prints and is overwritten when dropped (best effort).
    pub struct Secret(String);

    impl Secret {
        fn new(s: String) -> Self {
            Self(s)
        }

        fn expose(&self) -> &str {
            &self.0
        }
    }

    impl fmt::Debug for Secret {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("Secret([redacted])")
        }
    }

    impl Drop for Secret {
        fn drop(&mut self) {
            let mut v = std::mem::take(&mut self.0).into_bytes();
            v.fill(0);
            std::hint::black_box(&v);
        }
    }

    impl Clone for Secret {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }

    /// Cleans up a pasted key: trims it, removes surrounding quotes and a `Bearer ` prefix, and
    /// refuses anything with spaces, control or non-ASCII characters, or a length outside 8 to 512.
    /// The second value lists what was changed, for showing to the person.
    ///
    /// # Errors
    /// [`KeyError::Invalid`] when the text cannot be a key.
    pub fn normalise_secret(raw: &str) -> Result<(String, Vec<&'static str>), KeyError> {
        let mut notes = Vec::new();
        let mut s = raw.trim();
        if s.len() != raw.len() {
            notes.push("spaces around the key were removed");
        }
        if let Some(r) = s
            .strip_prefix("Bearer ")
            .or_else(|| s.strip_prefix("bearer "))
        {
            s = r.trim();
            notes.push("a Bearer prefix was removed");
        }
        for q in ['"', '\''] {
            if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
                s = &s[1..s.len() - 1];
                notes.push("quotes around the key were removed");
                break;
            }
        }
        if s.len() < 8 || s.len() > 512 {
            return Err(KeyError::Invalid("key length"));
        }
        if s.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
            return Err(KeyError::Invalid("key characters"));
        }
        Ok((s.to_string(), notes))
    }

    /// A hint when the key does not look like the provider's usual keys (not an error: providers
    /// change their formats).
    #[must_use]
    pub fn format_hint(provider: &str, secret: &str) -> Option<&'static str> {
        let (prefix, name) = match provider {
            "openai" => ("sk-", "OpenAI keys start with sk-"),
            "anthropic" => ("sk-ant-", "Anthropic keys start with sk-ant-"),
            "openrouter" => ("sk-or-", "OpenRouter keys start with sk-or-"),
            "groq" => ("gsk_", "Groq keys start with gsk_"),
            _ => return None,
        };
        (!secret.starts_with(prefix)).then_some(name)
    }

    fn label_ok(s: &str) -> bool {
        !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_control)
    }

    fn provider_ok(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 40
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    }

    /// A host name a key may be sent to: letters, digits, dots and hyphens; a leading `*.` allows
    /// any sub-domain. IP addresses are refused (a key belongs to a service, not an address).
    fn host_pattern_ok(h: &str) -> bool {
        let body = h.strip_prefix("*.").unwrap_or(h);
        !body.is_empty()
            && body.len() <= 253
            && body.contains('.')
            && body.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && !l.starts_with('-')
                    && !l.ends_with('-')
                    && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            && !body
                .split('.')
                .all(|l| l.bytes().all(|b| b.is_ascii_digit()))
    }

    fn host_matches(pattern: &str, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        match pattern.strip_prefix("*.") {
            Some(rest) => host.len() > rest.len() + 1 && host.ends_with(&format!(".{rest}")),
            None => host == pattern,
        }
    }

    fn fingerprint(provider: &str, secret: &str) -> String {
        hex(&sha256(&[
            b"mouli-key-fp|",
            provider.as_bytes(),
            b"|",
            secret.as_bytes(),
        ])[..6])
    }

    // ------------------------------------------------------------------ the vault

    /// What to store.
    #[derive(Debug, Clone)]
    pub struct NewKey<'a> {
        /// The provider (`openai`, `anthropic`, ...): lowercase letters, digits, `-`, `_`.
        pub provider: &'a str,
        /// A name the person recognises (`work account`).
        pub label: &'a str,
        /// The key itself (cleaned with [`normalise_secret`] first).
        pub secret: &'a str,
        /// The hosts it may be sent to (at least one).
        pub hosts: &'a [&'a str],
        /// When it stops working (seconds), if ever.
        pub expires: Option<u64>,
        /// The most uses per day, if limited.
        pub daily_limit: Option<u64>,
    }

    #[derive(Debug, Clone)]
    struct Entry {
        id: String,
        provider: String,
        label: String,
        secret: Secret,
        hosts: Vec<String>,
        created: u64,
        expires: Option<u64>,
        revoked: Option<u64>,
        daily_limit: Option<u64>,
        used_day: u64,
        used_count: u64,
        last_used: Option<u64>,
        history: Vec<(u64, String)>,
    }

    /// What can be shown about a key (never the secret).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct KeyInfo {
        /// The key's id.
        pub id: String,
        /// The provider.
        pub provider: String,
        /// The label.
        pub label: String,
        /// A short fingerprint to tell keys apart (not the key, and not part of it).
        pub fingerprint: String,
        /// The hosts it may be sent to.
        pub hosts: Vec<String>,
        /// When it was added.
        pub created: u64,
        /// When it expires.
        pub expires: Option<u64>,
        /// When it was revoked.
        pub revoked: Option<u64>,
        /// `active`, `expired` or `revoked`.
        pub status: &'static str,
        /// Uses today.
        pub used_today: u64,
        /// The daily limit.
        pub daily_limit: Option<u64>,
        /// The last use.
        pub last_used: Option<u64>,
        /// Earlier fingerprints from rotations, with the time each was replaced.
        pub rotated: Vec<(u64, String)>,
    }

    /// One line of the audit trail.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AuditRecord {
        /// A running number.
        pub seq: u64,
        /// When.
        pub time: u64,
        /// The key id, or `-`.
        pub id: String,
        /// What happened (`add`, `used`, `denied_host`, `revoked`, ...).
        pub outcome: String,
        /// What the key was used for, as given by the caller.
        pub purpose: String,
        /// The host it was handed to, if any.
        pub host: String,
        /// The previous record's hash.
        pub prev: String,
        /// This record's hash.
        pub hash: String,
    }

    /// The vault, unlocked.
    pub struct Vault {
        iterations: u32,
        salt: [u8; 16],
        key: [u8; 32],
        entries: Vec<Entry>,
        audit: Vec<AuditRecord>,
        audit_base: String,
        next_id: u64,
        next_seq: u64,
    }

    impl fmt::Debug for Vault {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "Vault({} keys, [redacted])", self.entries.len())
        }
    }

    impl Drop for Vault {
        fn drop(&mut self) {
            self.key = [0; 32];
            std::hint::black_box(&self.key);
        }
    }

    const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn derive(passphrase: &str, salt: &[u8; 16], iterations: u32) -> [u8; 32] {
        let master = pbkdf2(passphrase.as_bytes(), salt, iterations);
        sha256(&[b"mouli-vault/enc|", &master])
    }

    fn seal(
        key: &[u8; 32],
        nonce: &[u8; 12],
        aad: &str,
        plain: &[u8],
    ) -> Result<Vec<u8>, KeyError> {
        ChaCha20Poly1305::new(Key::from_slice(key))
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: plain,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| KeyError::Invalid("encryption"))
    }

    fn unseal(key: &[u8; 32], nonce: &[u8], aad: &str, ct: &[u8]) -> Option<Vec<u8>> {
        if nonce.len() != 12 {
            return None;
        }
        ChaCha20Poly1305::new(Key::from_slice(key))
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: aad.as_bytes(),
                },
            )
            .ok()
    }

    fn audit_hash(
        prev: &str,
        seq: u64,
        time: u64,
        id: &str,
        outcome: &str,
        purpose: &str,
        host: &str,
    ) -> String {
        hex(&sha256(&[
            prev.as_bytes(),
            b"|",
            format!("{seq}|{time}|{id}|{outcome}|{purpose}|{host}").as_bytes(),
        ]))
    }

    impl Vault {
        /// Makes a new, empty vault protected by `passphrase` (at least 8 characters).
        ///
        /// # Errors
        /// [`KeyError::Invalid`] for a short passphrase or too few iterations; [`KeyError::Entropy`].
        pub fn create(
            passphrase: &str,
            iterations: u32,
            rng: &mut dyn Entropy,
        ) -> Result<Self, KeyError> {
            if passphrase.chars().count() < 8 {
                return Err(KeyError::Invalid("passphrase (at least 8 characters)"));
            }
            if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
                return Err(KeyError::Invalid("iterations"));
            }
            let mut salt = [0u8; 16];
            rng.fill(&mut salt)?;
            Ok(Self {
                iterations,
                salt,
                key: derive(passphrase, &salt, iterations),
                entries: Vec::new(),
                audit: Vec::new(),
                audit_base: ZERO.into(),
                next_id: 1,
                next_seq: 1,
            })
        }

        fn log(&mut self, time: u64, id: &str, outcome: &str, purpose: &str, host: &str) {
            let prev = self
                .audit
                .last()
                .map_or_else(|| self.audit_base.clone(), |r| r.hash.clone());
            let seq = self.next_seq;
            self.next_seq += 1;
            let clean =
                |s: &str| -> String { s.chars().filter(|c| !c.is_control()).take(80).collect() };
            let (purpose, host) = (clean(purpose), clean(host));
            let hash = audit_hash(&prev, seq, time, id, outcome, &purpose, &host);
            self.audit.push(AuditRecord {
                seq,
                time,
                id: id.into(),
                outcome: outcome.into(),
                purpose,
                host,
                prev,
                hash,
            });
            if self.audit.len() > MAX_AUDIT {
                let first = self.audit.remove(0);
                self.audit_base = first.hash;
            }
        }

        /// Adds a key and returns its id.
        ///
        /// # Errors
        /// [`KeyError`] for a bad provider, label, key, host list, or when the vault is full.
        pub fn add(&mut self, k: &NewKey<'_>, now: u64) -> Result<String, KeyError> {
            if !provider_ok(k.provider) {
                return Err(KeyError::Invalid("provider"));
            }
            if !label_ok(k.label) {
                return Err(KeyError::Invalid("label"));
            }
            let (secret, _) = normalise_secret(k.secret)?;
            if k.hosts.is_empty()
                || k.hosts.len() > 8
                || k.hosts.iter().any(|h| !host_pattern_ok(h))
            {
                return Err(KeyError::Invalid("hosts (one to eight host names)"));
            }
            if k.daily_limit == Some(0) {
                return Err(KeyError::Invalid("daily limit"));
            }
            if self.entries.len() >= MAX_KEYS {
                return Err(KeyError::Full("keys"));
            }
            let id = format!("k{}", self.next_id);
            self.next_id += 1;
            self.entries.push(Entry {
                id: id.clone(),
                provider: k.provider.into(),
                label: k.label.into(),
                secret: Secret::new(secret),
                hosts: k.hosts.iter().map(|h| h.to_ascii_lowercase()).collect(),
                created: now,
                expires: k.expires,
                revoked: None,
                daily_limit: k.daily_limit,
                used_day: now / 86_400,
                used_count: 0,
                last_used: None,
                history: Vec::new(),
            });
            self.log(now, &id, "add", "", "");
            Ok(id)
        }

        fn status(e: &Entry, now: u64) -> &'static str {
            if e.revoked.is_some() {
                "revoked"
            } else if e.expires.is_some_and(|x| x <= now) {
                "expired"
            } else {
                "active"
            }
        }

        /// What is stored, without secrets.
        #[must_use]
        pub fn list(&self, now: u64) -> Vec<KeyInfo> {
            self.entries
                .iter()
                .map(|e| KeyInfo {
                    id: e.id.clone(),
                    provider: e.provider.clone(),
                    label: e.label.clone(),
                    fingerprint: fingerprint(&e.provider, e.secret.expose()),
                    hosts: e.hosts.clone(),
                    created: e.created,
                    expires: e.expires,
                    revoked: e.revoked,
                    status: Self::status(e, now),
                    used_today: if e.used_day == now / 86_400 {
                        e.used_count
                    } else {
                        0
                    },
                    daily_limit: e.daily_limit,
                    last_used: e.last_used,
                    rotated: e.history.clone(),
                })
                .collect()
        }

        /// The first usable key of a provider (most recently added first), if any.
        #[must_use]
        pub fn find_active(&self, provider: &str, now: u64) -> Option<String> {
            self.entries
                .iter()
                .rev()
                .find(|e| e.provider == provider && Self::status(e, now) == "active")
                .map(|e| e.id.clone())
        }

        /// Hands the secret of key `id` to `f`, for sending to `host`, after checking that the key
        /// is active, may go to that host and is within its daily limit. Every call, allowed or
        /// refused, is written to the audit trail with `purpose` (cut to 80 characters).
        ///
        /// # Errors
        /// [`KeyError::NotFound`], [`KeyError::Revoked`], [`KeyError::Expired`],
        /// [`KeyError::HostNotAllowed`], [`KeyError::LimitReached`].
        pub fn with_secret<R>(
            &mut self,
            id: &str,
            host: &str,
            purpose: &str,
            now: u64,
            f: impl FnOnce(&str) -> R,
        ) -> Result<R, KeyError> {
            let Some(i) = self.entries.iter().position(|e| e.id == id) else {
                self.log(now, "-", "denied_unknown", purpose, host);
                return Err(KeyError::NotFound);
            };
            let refuse = |me: &mut Self, why: &str, e: KeyError| -> Result<R, KeyError> {
                me.log(now, id, why, purpose, host);
                Err(e)
            };
            let e = &self.entries[i];
            if e.revoked.is_some() {
                return refuse(self, "denied_revoked", KeyError::Revoked);
            }
            if e.expires.is_some_and(|x| x <= now) {
                return refuse(self, "denied_expired", KeyError::Expired);
            }
            if !e.hosts.iter().any(|p| host_matches(p, host)) {
                return refuse(self, "denied_host", KeyError::HostNotAllowed);
            }
            let day = now / 86_400;
            let used = if e.used_day == day { e.used_count } else { 0 };
            if e.daily_limit.is_some_and(|l| used >= l) {
                return refuse(self, "denied_limit", KeyError::LimitReached);
            }
            let e = &mut self.entries[i];
            e.used_day = day;
            e.used_count = used + 1;
            e.last_used = Some(now);
            let out = f(e.secret.expose());
            self.log(now, id, "used", purpose, host);
            Ok(out)
        }

        /// Replaces a key's secret. Only a fingerprint of the old one is kept.
        ///
        /// # Errors
        /// [`KeyError::NotFound`] or [`KeyError::Invalid`] for a bad new key.
        pub fn rotate(&mut self, id: &str, new_secret: &str, now: u64) -> Result<(), KeyError> {
            let (secret, _) = normalise_secret(new_secret)?;
            let e = self
                .entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or(KeyError::NotFound)?;
            let old = fingerprint(&e.provider, e.secret.expose());
            e.history.push((now, old));
            if e.history.len() > 20 {
                e.history.remove(0);
            }
            e.secret = Secret::new(secret);
            e.revoked = None;
            e.used_count = 0;
            self.log(now, id, "rotated", "", "");
            Ok(())
        }

        /// Stops a key from being used. It stays listed, so the history is not lost.
        ///
        /// # Errors
        /// [`KeyError::NotFound`].
        pub fn revoke(&mut self, id: &str, now: u64) -> Result<(), KeyError> {
            let e = self
                .entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or(KeyError::NotFound)?;
            e.revoked.get_or_insert(now);
            self.log(now, id, "revoked", "", "");
            Ok(())
        }

        /// Removes a key and its secret for good.
        ///
        /// # Errors
        /// [`KeyError::NotFound`].
        pub fn delete(&mut self, id: &str, now: u64) -> Result<(), KeyError> {
            let i = self
                .entries
                .iter()
                .position(|e| e.id == id)
                .ok_or(KeyError::NotFound)?;
            self.entries.remove(i);
            self.log(now, id, "deleted", "", "");
            Ok(())
        }

        /// Changes the hosts a key may be sent to.
        ///
        /// # Errors
        /// [`KeyError::NotFound`] or [`KeyError::Invalid`].
        pub fn set_hosts(&mut self, id: &str, hosts: &[&str], now: u64) -> Result<(), KeyError> {
            if hosts.is_empty() || hosts.len() > 8 || hosts.iter().any(|h| !host_pattern_ok(h)) {
                return Err(KeyError::Invalid("hosts (one to eight host names)"));
            }
            let e = self
                .entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or(KeyError::NotFound)?;
            e.hosts = hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
            self.log(now, id, "hosts_changed", "", "");
            Ok(())
        }

        /// The audit trail, oldest first.
        #[must_use]
        pub fn audit(&self) -> &[AuditRecord] {
            &self.audit
        }

        /// Whether the audit trail is unbroken from its first kept record.
        #[must_use]
        pub fn audit_ok(&self) -> bool {
            let mut prev = self.audit_base.clone();
            for (i, r) in self.audit.iter().enumerate() {
                if r.prev != prev
                    || r.seq == 0
                    || (i > 0 && r.seq != self.audit[i - 1].seq + 1)
                    || r.hash
                        != audit_hash(
                            &r.prev, r.seq, r.time, &r.id, &r.outcome, &r.purpose, &r.host,
                        )
                {
                    return false;
                }
                prev.clone_from(&r.hash);
            }
            true
        }

        /// Protects the vault with a new passphrase (the same keys; new salt).
        ///
        /// # Errors
        /// [`KeyError::Invalid`] for a short passphrase; [`KeyError::Entropy`].
        pub fn change_passphrase(
            &mut self,
            new: &str,
            rng: &mut dyn Entropy,
        ) -> Result<(), KeyError> {
            if new.chars().count() < 8 {
                return Err(KeyError::Invalid("passphrase (at least 8 characters)"));
            }
            let mut salt = [0u8; 16];
            rng.fill(&mut salt)?;
            self.salt = salt;
            self.key = derive(new, &salt, self.iterations);
            Ok(())
        }

        fn entry_text(e: &Entry) -> String {
            let mut t = format!(
                "provider {}\nlabel {}\nsecret {}\ncreated {}\nused_day {}\nused_count {}\n",
                esc(&e.provider),
                esc(&e.label),
                esc(e.secret.expose()),
                e.created,
                e.used_day,
                e.used_count
            );
            t.push_str(&format!(
                "hosts {}\n",
                e.hosts.iter().map(|h| esc(h)).collect::<Vec<_>>().join(" ")
            ));
            if let Some(x) = e.expires {
                t.push_str(&format!("expires {x}\n"));
            }
            if let Some(x) = e.revoked {
                t.push_str(&format!("revoked {x}\n"));
            }
            if let Some(x) = e.daily_limit {
                t.push_str(&format!("daily_limit {x}\n"));
            }
            if let Some(x) = e.last_used {
                t.push_str(&format!("last_used {x}\n"));
            }
            for (time, fp) in &e.history {
                t.push_str(&format!("hist {time} {fp}\n"));
            }
            t
        }

        fn parse_entry(id: &str, text: &str) -> Option<Entry> {
            let mut provider = None;
            let mut label = None;
            let mut secret = None;
            let mut hosts = None;
            let (mut created, mut used_day, mut used_count) = (None, None, None);
            let (mut expires, mut revoked, mut daily_limit, mut last_used) =
                (None, None, None, None);
            let mut history = Vec::new();
            for line in text.lines() {
                let (k, v) = line.split_once(' ').unwrap_or((line, ""));
                match k {
                    "provider" => provider = Some(unesc(v)?),
                    "label" => label = Some(unesc(v)?),
                    "secret" => secret = Some(unesc(v)?),
                    "hosts" => {
                        hosts = Some(
                            v.split(' ')
                                .filter(|h| !h.is_empty())
                                .map(unesc)
                                .collect::<Option<Vec<_>>>()?,
                        )
                    }
                    "created" => created = Some(v.parse().ok()?),
                    "used_day" => used_day = Some(v.parse().ok()?),
                    "used_count" => used_count = Some(v.parse().ok()?),
                    "expires" => expires = Some(v.parse().ok()?),
                    "revoked" => revoked = Some(v.parse().ok()?),
                    "daily_limit" => daily_limit = Some(v.parse().ok()?),
                    "last_used" => last_used = Some(v.parse().ok()?),
                    "hist" => {
                        let (t, fp) = v.split_once(' ')?;
                        history.push((t.parse().ok()?, fp.to_string()));
                    }
                    _ => return None,
                }
            }
            let (provider, label, secret, hosts) = (provider?, label?, secret?, hosts?);
            if !provider_ok(&provider)
                || !label_ok(&label)
                || hosts.is_empty()
                || hosts.iter().any(|h| !host_pattern_ok(h))
            {
                return None;
            }
            Some(Entry {
                id: id.into(),
                provider,
                label,
                secret: Secret::new(secret),
                hosts,
                created: created?,
                expires,
                revoked,
                daily_limit,
                used_day: used_day?,
                used_count: used_count?,
                last_used,
                history,
            })
        }

        fn audit_text(&self) -> String {
            let mut t = format!("base {}\n", self.audit_base);
            for r in &self.audit {
                t.push_str(&format!(
                    "a {} {} {} {} {} {} {} {}\n",
                    r.seq,
                    r.time,
                    esc(&r.id),
                    esc(&r.outcome),
                    esc(&r.purpose),
                    esc(&r.host),
                    r.prev,
                    r.hash
                ));
            }
            t
        }

        fn parse_audit(text: &str) -> Option<(String, Vec<AuditRecord>)> {
            let mut lines = text.lines();
            let base = lines.next()?.strip_prefix("base ")?.to_string();
            let mut out = Vec::new();
            for l in lines {
                let p: Vec<&str> = l.split(' ').collect();
                if p.len() != 9 || p[0] != "a" {
                    return None;
                }
                out.push(AuditRecord {
                    seq: p[1].parse().ok()?,
                    time: p[2].parse().ok()?,
                    id: unesc(p[3])?,
                    outcome: unesc(p[4])?,
                    purpose: unesc(p[5])?,
                    host: unesc(p[6])?,
                    prev: p[7].to_string(),
                    hash: p[8].to_string(),
                });
            }
            Some((base, out))
        }

        /// The vault as text for saving. Every record gets a fresh random nonce, so the same key is
        /// never used twice with one nonce.
        ///
        /// # Errors
        /// [`KeyError::Entropy`] or [`KeyError::Invalid`].
        pub fn save(&self, rng: &mut dyn Entropy) -> Result<String, KeyError> {
            let mut nonce = || -> Result<[u8; 12], KeyError> {
                let mut n = [0u8; 12];
                rng.fill(&mut n)?;
                Ok(n)
            };
            let mut lines: Vec<String> = vec![
                MAGIC.into(),
                format!("kdf pbkdf2-sha256 {} {}", self.iterations, hex(&self.salt)),
            ];
            let n = nonce()?;
            let meta = format!("next_id {}\nnext_seq {}\n", self.next_id, self.next_seq);
            lines.push(format!(
                "meta {} {}",
                hex(&n),
                hex(&seal(&self.key, &n, "meta", meta.as_bytes())?)
            ));
            for e in &self.entries {
                let n = nonce()?;
                let ct = seal(
                    &self.key,
                    &n,
                    &format!("rec|{}", e.id),
                    Self::entry_text(e).as_bytes(),
                )?;
                lines.push(format!("rec {} {} {}", e.id, hex(&n), hex(&ct)));
            }
            let n = nonce()?;
            let ct = seal(&self.key, &n, "audit", self.audit_text().as_bytes())?;
            lines.push(format!("audit {} {}", hex(&n), hex(&ct)));
            let digest = sha256(&[lines.join("\n").as_bytes()]);
            let n = nonce()?;
            let ct = seal(
                &self.key,
                &n,
                &format!("seal|{}", self.entries.len()),
                &digest,
            )?;
            lines.push(format!("seal {} {}", hex(&n), hex(&ct)));
            let mut out = lines.join("\n");
            out.push('\n');
            Ok(out)
        }

        /// Opens a saved vault.
        ///
        /// # Errors
        /// [`KeyError::Format`] for a file that is not a vault; [`KeyError::WrongPassphrase`];
        /// [`KeyError::Tampered`] when a record was changed, removed, added or reordered.
        pub fn open(text: &str, passphrase: &str) -> Result<Self, KeyError> {
            if text.len() > MAX_FILE {
                return Err(KeyError::Format(1));
            }
            let lines: Vec<&str> = text.lines().collect();
            if lines.first() != Some(&MAGIC) {
                return Err(KeyError::Format(1));
            }
            let kdf: Vec<&str> = lines
                .get(1)
                .ok_or(KeyError::Format(2))?
                .split(' ')
                .collect();
            let (iterations, salt) = match kdf.as_slice() {
                ["kdf", "pbkdf2-sha256", it, salt] => {
                    let it: u32 = it.parse().map_err(|_| KeyError::Format(2))?;
                    let salt: [u8; 16] = unhex(salt)
                        .and_then(|v| v.try_into().ok())
                        .ok_or(KeyError::Format(2))?;
                    (it, salt)
                }
                _ => return Err(KeyError::Format(2)),
            };
            if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
                return Err(KeyError::Format(2));
            }
            let key = derive(passphrase, &salt, iterations);
            let field =
                |n: usize, parts: &[&str], want: usize| -> Result<(Vec<u8>, Vec<u8>), KeyError> {
                    if parts.len() != want {
                        return Err(KeyError::Format(n));
                    }
                    let nonce = unhex(parts[want - 2]).ok_or(KeyError::Format(n))?;
                    let ct = unhex(parts[want - 1]).ok_or(KeyError::Format(n))?;
                    Ok((nonce, ct))
                };
            // The meta record doubles as the passphrase check.
            let meta_parts: Vec<&str> = lines
                .get(2)
                .ok_or(KeyError::Format(3))?
                .split(' ')
                .collect();
            if meta_parts.first() != Some(&"meta") {
                return Err(KeyError::Format(3));
            }
            let (n, ct) = field(3, &meta_parts, 3)?;
            let meta = unseal(&key, &n, "meta", &ct).ok_or(KeyError::WrongPassphrase)?;
            let meta = String::from_utf8(meta).map_err(|_| KeyError::Tampered)?;
            let mut it = meta.lines();
            let next_id: u64 = it
                .next()
                .and_then(|l| l.strip_prefix("next_id "))
                .and_then(|v| v.parse().ok())
                .ok_or(KeyError::Tampered)?;
            let next_seq: u64 = it
                .next()
                .and_then(|l| l.strip_prefix("next_seq "))
                .and_then(|v| v.parse().ok())
                .ok_or(KeyError::Tampered)?;
            let mut entries = Vec::new();
            let mut audit_state = None;
            let mut sealed = false;
            let mut seal_index = 0;
            for (i, line) in lines.iter().enumerate().skip(3) {
                let n = i + 1;
                let p: Vec<&str> = line.split(' ').collect();
                match p.first().copied() {
                    Some("rec") if audit_state.is_none() && !sealed => {
                        let (nonce, ct) = field(n, &p, 4)?;
                        if entries.len() >= MAX_KEYS || p[1].len() > 20 {
                            return Err(KeyError::Format(n));
                        }
                        let plain = unseal(&key, &nonce, &format!("rec|{}", p[1]), &ct)
                            .ok_or(KeyError::Tampered)?;
                        let text = String::from_utf8(plain).map_err(|_| KeyError::Tampered)?;
                        let e = Self::parse_entry(p[1], &text).ok_or(KeyError::Tampered)?;
                        if entries.iter().any(|x: &Entry| x.id == e.id) {
                            return Err(KeyError::Tampered);
                        }
                        entries.push(e);
                    }
                    Some("audit") if audit_state.is_none() && !sealed => {
                        let (nonce, ct) = field(n, &p, 3)?;
                        let plain = unseal(&key, &nonce, "audit", &ct).ok_or(KeyError::Tampered)?;
                        let text = String::from_utf8(plain).map_err(|_| KeyError::Tampered)?;
                        audit_state = Some(Self::parse_audit(&text).ok_or(KeyError::Tampered)?);
                    }
                    Some("seal") if audit_state.is_some() && !sealed => {
                        let (nonce, ct) = field(n, &p, 3)?;
                        let digest = sha256(&[lines[..i].join("\n").as_bytes()]);
                        let plain = unseal(&key, &nonce, &format!("seal|{}", entries.len()), &ct)
                            .ok_or(KeyError::Tampered)?;
                        if plain != digest {
                            return Err(KeyError::Tampered);
                        }
                        sealed = true;
                        seal_index = i;
                    }
                    _ => return Err(KeyError::Format(n)),
                }
            }
            let (Some((audit_base, audit)), true) = (audit_state, sealed) else {
                return Err(KeyError::Tampered);
            };
            if seal_index + 1 != lines.len() {
                return Err(KeyError::Format(seal_index + 2));
            }
            let v = Self {
                iterations,
                salt,
                key,
                entries,
                audit,
                audit_base,
                next_id,
                next_seq,
            };
            if !v.audit_ok() {
                return Err(KeyError::Tampered);
            }
            Ok(v)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Counts up, so every call gives different bytes and tests are repeatable.
        struct Counter(u8);
        impl Entropy for Counter {
            fn fill(&mut self, buf: &mut [u8]) -> Result<(), KeyError> {
                for b in buf {
                    self.0 = self.0.wrapping_mul(31).wrapping_add(7);
                    *b = self.0;
                }
                Ok(())
            }
        }

        struct NoEntropy;
        impl Entropy for NoEntropy {
            fn fill(&mut self, _: &mut [u8]) -> Result<(), KeyError> {
                Err(KeyError::Entropy)
            }
        }

        const PASS: &str = "correct horse battery";
        const SECRET: &str = "sk-live-ABCDEF0123456789";

        fn key<'a>(hosts: &'a [&'a str]) -> NewKey<'a> {
            NewKey {
                provider: "openai",
                label: "work account",
                secret: SECRET,
                hosts,
                expires: None,
                daily_limit: None,
            }
        }

        fn vault() -> (Vault, String) {
            let mut rng = Counter(1);
            let mut v = Vault::create(PASS, MIN_ITERATIONS, &mut rng).unwrap();
            let id = v.add(&key(&["api.openai.com"]), 1_000).unwrap();
            (v, id)
        }

        #[test]
        fn hmac_and_pbkdf2_match_published_values() {
            assert_eq!(
                hex(&hmac(&[0x0b; 20], b"Hi There")),
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
            );
            assert_eq!(
                hex(&hmac(
                    &[0xaa; 131],
                    b"Test Using Larger Than Block-Size Key - Hash Key First"
                )),
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
                "a key longer than the block is hashed first"
            );
            assert_eq!(
                hex(&pbkdf2(b"password", b"salt", 1)),
                "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
            );
            assert_eq!(
                hex(&pbkdf2(b"password", b"salt", 2)),
                "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
            );
            assert_eq!(
                hex(&pbkdf2(b"password", b"salt", 4096)),
                "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
            );
        }

        #[test]
        fn a_vault_round_trips_and_the_saved_text_hides_everything() {
            let (v, id) = vault();
            let text = v.save(&mut Counter(9)).unwrap();
            for secret in [
                SECRET,
                "ABCDEF0123456789",
                "openai",
                "work",
                "api.openai.com",
                id.as_str()
                    .trim_start_matches('k')
                    .to_string()
                    .as_str()
                    .to_owned()
                    .as_str(),
            ] {
                if secret.len() > 1 {
                    assert!(
                        !text.contains(secret),
                        "{secret} appears in the saved vault"
                    );
                }
            }
            let back = Vault::open(&text, PASS).unwrap();
            let l = back.list(2_000);
            assert_eq!(l.len(), 1);
            assert_eq!(
                (l[0].provider.as_str(), l[0].label.as_str(), l[0].status),
                ("openai", "work account", "active")
            );
            let mut back = back;
            let got = back
                .with_secret(&id, "api.openai.com", "chat", 2_000, str::to_string)
                .unwrap();
            assert_eq!(got, SECRET);
            assert!(back.audit_ok());
        }

        #[test]
        fn a_wrong_passphrase_is_told_apart_from_damage() {
            let (v, _) = vault();
            let text = v.save(&mut Counter(2)).unwrap();
            assert_eq!(
                Vault::open(&text, "wrong passphrase").unwrap_err(),
                KeyError::WrongPassphrase
            );
            assert_eq!(
                Vault::open(&text, "").unwrap_err(),
                KeyError::WrongPassphrase
            );
        }

        #[test]
        fn any_change_to_the_file_is_detected() {
            let (mut v, _) = vault();
            v.add(
                &NewKey {
                    label: "second",
                    secret: "sk-second-0123456789",
                    ..key(&["api.openai.com"])
                },
                1_001,
            )
            .unwrap();
            let text = v.save(&mut Counter(3)).unwrap();
            assert!(Vault::open(&text, PASS).is_ok());
            let lines: Vec<&str> = text.lines().collect();
            // Flip a hex digit inside each encrypted line in turn.
            for i in 2..lines.len() {
                let mut l: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
                let last = l[i].pop().unwrap();
                l[i].push(if last == '0' { '1' } else { '0' });
                let r = Vault::open(&(l.join("\n") + "\n"), PASS);
                assert!(r.is_err(), "line {} changed but the vault opened", i + 1);
            }
            // Remove a key record.
            let mut l: Vec<&str> = lines.clone();
            let at = l.iter().position(|s| s.starts_with("rec ")).unwrap();
            l.remove(at);
            assert_eq!(
                Vault::open(&(l.join("\n") + "\n"), PASS).unwrap_err(),
                KeyError::Tampered
            );
            // Swap two key records.
            let mut l: Vec<&str> = lines.clone();
            let a = l.iter().position(|s| s.starts_with("rec ")).unwrap();
            l.swap(a, a + 1);
            assert_eq!(
                Vault::open(&(l.join("\n") + "\n"), PASS).unwrap_err(),
                KeyError::Tampered
            );
            // Duplicate a record under another id (the id is bound into the encryption).
            let mut l: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
            let a = l.iter().position(|s| s.starts_with("rec k1 ")).unwrap();
            l[a] = l[a].replacen("rec k1 ", "rec k9 ", 1);
            assert!(Vault::open(&(l.join("\n") + "\n"), PASS).is_err());
            // Truncated: the seal is gone.
            let t: Vec<&str> = lines[..lines.len() - 1].to_vec();
            assert!(Vault::open(&(t.join("\n") + "\n"), PASS).is_err());
            // Text added after the seal.
            assert!(Vault::open(&format!("{text}extra\n"), PASS).is_err());
        }

        #[test]
        fn malformed_files_are_refused_without_panicking() {
            for bad in [
                "",
                "garbage",
                "mouli-vault 2\n",
                "mouli-vault 1\n",
                "mouli-vault 1\nkdf pbkdf2-sha256 100 00\n",
                "mouli-vault 1\nkdf pbkdf2-sha256 99999999999 00112233445566778899aabbccddeeff\n",
                "mouli-vault 1\nkdf pbkdf2-sha256 1000 00112233445566778899aabbccddeeff\n",
                "mouli-vault 1\nkdf pbkdf2-sha256 20000000 00112233445566778899aabbccddeeff\nmeta aa bb\n",
                "mouli-vault 1\nkdf pbkdf2-sha256 10000 00112233445566778899aabbccddeeff\nmeta zz yy\n",
                "mouli-vault 1\nkdf pbkdf2-sha256 10000 00112233445566778899aabbccddeeff\nnotmeta a b\n",
                "mouli-vault 1\nkdf scrypt 10000 00112233445566778899aabbccddeeff\n",
            ] {
                assert!(Vault::open(bad, PASS).is_err(), "{bad:?}");
            }
            assert!(Vault::open(&"x".repeat(MAX_FILE + 1), PASS).is_err());
        }

        #[test]
        fn every_save_uses_fresh_nonces() {
            let (v, _) = vault();
            let a = v.save(&mut Counter(5)).unwrap();
            let b = v.save(&mut Counter(6)).unwrap();
            assert_ne!(a, b, "same content, different ciphertext");
            let n = |t: &str| {
                t.lines()
                    .find(|l| l.starts_with("rec "))
                    .unwrap()
                    .split(' ')
                    .nth(2)
                    .unwrap()
                    .to_string()
            };
            assert_ne!(n(&a), n(&b));
            assert_eq!(v.save(&mut NoEntropy).unwrap_err(), KeyError::Entropy);
        }

        #[test]
        fn creation_rules() {
            assert!(Vault::create("short", MIN_ITERATIONS, &mut Counter(1)).is_err());
            assert!(Vault::create(PASS, MIN_ITERATIONS - 1, &mut Counter(1)).is_err());
            assert!(Vault::create(PASS, MAX_ITERATIONS + 1, &mut Counter(1)).is_err());
            assert_eq!(
                Vault::create(PASS, MIN_ITERATIONS, &mut NoEntropy).unwrap_err(),
                KeyError::Entropy
            );
            assert_eq!(
                format!(
                    "{:?}",
                    Vault::create(PASS, MIN_ITERATIONS, &mut Counter(1)).unwrap()
                ),
                "Vault(0 keys, [redacted])"
            );
        }

        #[test]
        fn a_key_goes_only_to_its_hosts() {
            let (mut v, id) = vault();
            for host in ["api.openai.com", "API.OpenAI.com"] {
                assert!(
                    v.with_secret(&id, host, "chat", 2_000, |_| ()).is_ok(),
                    "{host}"
                );
            }
            for host in [
                "evil.example.com",
                "openai.com",
                "api.openai.com.evil.io",
                "xapi.openai.com",
                "",
                "api.openai.com:8080",
            ] {
                assert_eq!(
                    v.with_secret(&id, host, "chat", 2_000, |_| ()),
                    Err(KeyError::HostNotAllowed),
                    "{host:?}"
                );
            }
            v.set_hosts(&id, &["*.openai.com"], 2_000).unwrap();
            assert!(v
                .with_secret(&id, "eu.api.openai.com", "chat", 2_000, |_| ())
                .is_ok());
            assert!(
                v.with_secret(&id, "openai.com", "chat", 2_000, |_| ())
                    .is_err(),
                "the bare domain is not a sub-domain"
            );
            assert!(v
                .with_secret(&id, "notopenai.com", "chat", 2_000, |_| ())
                .is_err());
        }

        #[test]
        fn host_patterns_and_inputs_are_validated() {
            let mut v = Vault::create(PASS, MIN_ITERATIONS, &mut Counter(1)).unwrap();
            for bad in [
                &[][..],
                &["localhost"][..],
                &["127.0.0.1"][..],
                &["1.2.3.4"][..],
                &["a b.com"][..],
                &["-bad.com"][..],
                &["bad-.com"][..],
                &["a..com"][..],
                &["*"][..],
                &["*.com.*"][..],
                &["http://x.com"][..],
                &["x.com/path"][..],
            ] {
                assert!(v.add(&key(bad), 0).is_err(), "{bad:?}");
            }
            let nine = ["a.com"; 9];
            assert!(v.add(&key(&nine), 0).is_err());
            assert!(v
                .add(
                    &NewKey {
                        provider: "Open AI",
                        ..key(&["a.com"])
                    },
                    0
                )
                .is_err());
            assert!(v
                .add(
                    &NewKey {
                        provider: "",
                        ..key(&["a.com"])
                    },
                    0
                )
                .is_err());
            assert!(v
                .add(
                    &NewKey {
                        label: "",
                        ..key(&["a.com"])
                    },
                    0
                )
                .is_err());
            assert!(v
                .add(
                    &NewKey {
                        label: "a\nb",
                        ..key(&["a.com"])
                    },
                    0
                )
                .is_err());
            assert!(v
                .add(
                    &NewKey {
                        secret: "short",
                        ..key(&["a.com"])
                    },
                    0
                )
                .is_err());
            assert!(v
                .add(
                    &NewKey {
                        daily_limit: Some(0),
                        ..key(&["a.com"])
                    },
                    0
                )
                .is_err());
            assert!(v.add(&key(&["a.com"]), 0).is_ok());
        }

        #[test]
        fn expiry_revocation_and_limits_stop_use() {
            let mut rng = Counter(1);
            let mut v = Vault::create(PASS, MIN_ITERATIONS, &mut rng).unwrap();
            let id = v
                .add(
                    &NewKey {
                        expires: Some(5_000),
                        daily_limit: Some(2),
                        ..key(&["api.openai.com"])
                    },
                    1_000,
                )
                .unwrap();
            let day = 86_400;
            assert!(v
                .with_secret(&id, "api.openai.com", "a", 1_000, |_| ())
                .is_ok());
            assert!(v
                .with_secret(&id, "api.openai.com", "b", 1_001, |_| ())
                .is_ok());
            assert_eq!(
                v.with_secret(&id, "api.openai.com", "c", 1_002, |_| ()),
                Err(KeyError::LimitReached)
            );
            assert_eq!(v.list(1_003)[0].used_today, 2);
            assert_eq!(
                v.with_secret(&id, "api.openai.com", "d", 5_000, |_| ()),
                Err(KeyError::Expired)
            );
            assert_eq!(v.list(5_000)[0].status, "expired");
            // A new day resets the count (a key without an expiry).
            let id2 = v
                .add(
                    &NewKey {
                        label: "two",
                        daily_limit: Some(1),
                        ..key(&["api.openai.com"])
                    },
                    10,
                )
                .unwrap();
            assert!(v
                .with_secret(&id2, "api.openai.com", "", 10, |_| ())
                .is_ok());
            assert!(v
                .with_secret(&id2, "api.openai.com", "", 11, |_| ())
                .is_err());
            assert!(
                v.with_secret(&id2, "api.openai.com", "", day + 5, |_| ())
                    .is_ok(),
                "next day"
            );
            v.revoke(&id2, day + 6).unwrap();
            assert_eq!(
                v.with_secret(&id2, "api.openai.com", "", day + 7, |_| ()),
                Err(KeyError::Revoked)
            );
            assert_eq!(
                v.list(day + 7).iter().find(|k| k.id == id2).unwrap().status,
                "revoked"
            );
            assert_eq!(
                v.with_secret("k99", "api.openai.com", "", 0, |_| ()),
                Err(KeyError::NotFound)
            );
        }

        #[test]
        fn rotation_keeps_only_a_fingerprint_of_the_old_key() {
            let (mut v, id) = vault();
            let before = v.list(0)[0].fingerprint.clone();
            v.revoke(&id, 5).unwrap();
            v.rotate(&id, "sk-new-0123456789ABCDEF", 10).unwrap();
            let info = &v.list(11)[0];
            assert_ne!(info.fingerprint, before);
            assert_eq!(info.rotated, vec![(10, before.clone())]);
            assert_eq!(info.status, "active", "a new key is usable again");
            let got = v
                .with_secret(&id, "api.openai.com", "t", 12, str::to_string)
                .unwrap();
            assert_eq!(got, "sk-new-0123456789ABCDEF");
            let text = v.save(&mut Counter(4)).unwrap();
            assert!(!text.contains(SECRET));
            assert_eq!(
                v.rotate("nope", "sk-new-0123456789ABCDEF", 0),
                Err(KeyError::NotFound)
            );
            assert!(v.rotate(&id, "x", 0).is_err());
        }

        #[test]
        fn find_active_prefers_the_newest_usable_key_of_a_provider() {
            let mut v = Vault::create(PASS, MIN_ITERATIONS, &mut Counter(1)).unwrap();
            let a = v.add(&key(&["api.openai.com"]), 0).unwrap();
            let b = v
                .add(
                    &NewKey {
                        label: "b",
                        secret: "sk-bbbbbbbbbbbb",
                        ..key(&["api.openai.com"])
                    },
                    0,
                )
                .unwrap();
            assert_eq!(v.find_active("openai", 1), Some(b.clone()));
            v.revoke(&b, 2).unwrap();
            assert_eq!(v.find_active("openai", 3), Some(a));
            assert_eq!(v.find_active("anthropic", 3), None);
        }

        #[test]
        fn deleting_removes_the_secret_from_what_is_saved() {
            let (mut v, id) = vault();
            v.delete(&id, 5).unwrap();
            assert!(v.list(6).is_empty());
            assert_eq!(v.delete(&id, 6), Err(KeyError::NotFound));
            let back = Vault::open(&v.save(&mut Counter(8)).unwrap(), PASS).unwrap();
            assert!(back.list(7).is_empty());
            assert!(back.audit().iter().any(|r| r.outcome == "deleted"));
        }

        #[test]
        fn the_passphrase_can_be_changed() {
            let (mut v, id) = vault();
            assert!(v.change_passphrase("short", &mut Counter(2)).is_err());
            v.change_passphrase("a brand new passphrase", &mut Counter(2))
                .unwrap();
            let text = v.save(&mut Counter(3)).unwrap();
            assert_eq!(
                Vault::open(&text, PASS).unwrap_err(),
                KeyError::WrongPassphrase
            );
            let mut back = Vault::open(&text, "a brand new passphrase").unwrap();
            assert_eq!(
                back.with_secret(&id, "api.openai.com", "", 9, str::to_string)
                    .unwrap(),
                SECRET
            );
        }

        #[test]
        fn the_audit_trail_records_uses_and_refusals_and_detects_tampering() {
            let (mut v, id) = vault();
            v.with_secret(&id, "api.openai.com", "chat\nforged line", 10, |_| ())
                .unwrap();
            let _ = v.with_secret(&id, "evil.example.com", "chat", 11, |_| ());
            let _ = v.with_secret("zzz", "api.openai.com", "chat", 12, |_| ());
            let outcomes: Vec<&str> = v.audit().iter().map(|r| r.outcome.as_str()).collect();
            assert_eq!(
                outcomes,
                vec!["add", "used", "denied_host", "denied_unknown"]
            );
            assert!(
                !v.audit()[1].purpose.contains('\n'),
                "control characters are dropped"
            );
            assert!(v.audit_ok());
            assert_eq!(v.audit()[2].host, "evil.example.com");
            assert!(v
                .audit()
                .iter()
                .all(|r| !r.purpose.contains(SECRET) && !r.host.contains(SECRET)));
            // It survives saving and loading, still verifying.
            let back = Vault::open(&v.save(&mut Counter(4)).unwrap(), PASS).unwrap();
            assert_eq!(back.audit(), v.audit());
            // An altered record breaks the chain.
            let mut bad = v;
            bad.audit[1].outcome = "denied_host".into();
            assert!(!bad.audit_ok());
        }

        #[test]
        fn the_audit_trail_is_bounded_and_still_verifies() {
            let (mut v, id) = vault();
            for t in 0..(MAX_AUDIT as u64 + 50) {
                v.with_secret(&id, "api.openai.com", "x", 10 + t, |_| ())
                    .unwrap();
            }
            assert_eq!(v.audit().len(), MAX_AUDIT);
            assert!(
                v.audit_ok(),
                "the chain is checked from the first kept record"
            );
            let back = Vault::open(&v.save(&mut Counter(5)).unwrap(), PASS).unwrap();
            assert_eq!(back.audit().len(), MAX_AUDIT);
        }

        #[test]
        fn the_vault_is_bounded() {
            let mut v = Vault::create(PASS, MIN_ITERATIONS, &mut Counter(1)).unwrap();
            for i in 0..MAX_KEYS {
                v.add(
                    &NewKey {
                        label: &format!("k{i}"),
                        ..key(&["a.com"])
                    },
                    0,
                )
                .unwrap();
            }
            assert_eq!(v.add(&key(&["a.com"]), 0), Err(KeyError::Full("keys")));
        }

        #[test]
        fn secrets_never_print() {
            let (mut v, id) = vault();
            assert!(!format!("{v:?}").contains("sk-"));
            assert_eq!(
                format!("{:?}", Secret::new("sk-xxxxxxxxxx".into())),
                "Secret([redacted])"
            );
            let listed = format!("{:?}", v.list(0));
            assert!(
                !listed.contains(SECRET) && !listed.contains("ABCDEF"),
                "{listed}"
            );
            for e in [
                KeyError::HostNotAllowed,
                KeyError::Revoked,
                KeyError::Tampered,
            ] {
                assert!(!e.to_string().contains("sk-"));
            }
            let _ = v.with_secret(&id, "x.com", "", 0, |_| ());
        }

        #[test]
        fn pasted_keys_are_cleaned_and_odd_ones_refused() {
            assert_eq!(
                normalise_secret("  sk-abcdefgh1234  ").unwrap(),
                (
                    "sk-abcdefgh1234".into(),
                    vec!["spaces around the key were removed"]
                )
            );
            assert_eq!(
                normalise_secret("Bearer sk-abcdefgh1234").unwrap().0,
                "sk-abcdefgh1234"
            );
            assert_eq!(
                normalise_secret("\"sk-abcdefgh1234\"").unwrap().0,
                "sk-abcdefgh1234"
            );
            assert_eq!(
                normalise_secret("'sk-abcdefgh1234'").unwrap().0,
                "sk-abcdefgh1234"
            );
            assert!(normalise_secret("sk-abcdefgh1234").unwrap().1.is_empty());
            for bad in [
                "",
                "short",
                "has space inside1234",
                "tab\there12345678",
                "nul\0abcdefgh",
                "ünïcode-key-12345",
                "line1\nline2-abcdef",
                &"a".repeat(513),
            ] {
                assert!(normalise_secret(bad).is_err(), "{bad:?}");
            }
            assert!(normalise_secret(&"a".repeat(512)).is_ok());
        }

        #[test]
        fn a_hint_appears_when_a_key_does_not_look_right() {
            assert_eq!(format_hint("openai", "sk-abc"), None);
            assert!(format_hint("openai", "abc123456").unwrap().contains("sk-"));
            assert!(format_hint("anthropic", "sk-abc")
                .unwrap()
                .contains("sk-ant-"));
            assert_eq!(format_hint("anthropic", "sk-ant-abc"), None);
            assert_eq!(format_hint("unknownprovider", "whatever"), None);
        }

        #[cfg(unix)]
        #[test]
        fn the_system_entropy_source_gives_different_bytes() {
            let (mut a, mut b) = ([0u8; 16], [0u8; 16]);
            let mut e = UrandomEntropy;
            e.fill(&mut a).unwrap();
            e.fill(&mut b).unwrap();
            assert_ne!(a, b);
            assert_ne!(a, [0u8; 16]);
        }
    }
}

/// Data washing: find personal data and secrets in text and remove, mask or pseudonymise them
/// before the text leaves the machine, with ready-made profiles for GDPR and DPDP.
///
/// What it recognises (by patterns and checksums, not by guessing): private keys, JWTs, secrets
/// (`password=...` style and known key formats), payment cards (Luhn), Aadhaar numbers
/// (Verhoeff), IBANs (mod 97), PAN, GSTIN (checksum), IFSC codes, UPI ids, email addresses,
/// phone numbers and IPv4 addresses.
///
/// What it does **not** do: it cannot find names, addresses, or personal facts written in free
/// text, and it cannot decide what is lawful. A profile is a sensible starting point for the
/// rules each law describes, not a statement that a product is compliant with the law. Treat it
/// as a safety net before text leaves the machine, together with your own legal advice.
///
/// Actions per kind: keep it, mask it (`[EMAIL]`, irreversible), partly mask it (`XXXX XXXX 1234`,
/// irreversible), or pseudonymise it with a token (`[EMAIL_1]`) that the same [`wash::Washer`]
/// can turn back when the person authorises it (GDPR names pseudonymisation explicitly: the
/// mapping stays with the Washer, in memory, on this machine).
#[allow(clippy::module_inception)]
pub mod wash {
    #![forbid(unsafe_code)]

    use std::collections::BTreeMap;
    use std::fmt;

    /// Largest text washed in one call, in bytes.
    pub const MAX_TEXT: usize = 4 * 1024 * 1024;
    /// Most tokens one [`Washer`] hands out; beyond it values are masked instead.
    pub const MAX_TOKENS: usize = 10_000;

    /// Why washing failed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum WashError {
        /// The text is longer than [`MAX_TEXT`].
        TooLarge,
        /// Restoring pseudonyms needs the person's authorisation.
        NotAuthorised,
    }

    impl fmt::Display for WashError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::TooLarge => f.write_str("the text is too large to wash"),
                Self::NotAuthorised => {
                    f.write_str("restoring washed values needs the person's authorisation")
                }
            }
        }
    }

    impl std::error::Error for WashError {}

    /// What was found. Earlier kinds win when two overlap.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Kind {
        /// A PEM private key block.
        PrivateKey,
        /// A JSON web token.
        Jwt,
        /// A credential: `password=...` or a known key format.
        Secret,
        /// A payment card number (13 to 19 digits, Luhn).
        Card,
        /// An Aadhaar number (12 digits, Verhoeff).
        Aadhaar,
        /// An IBAN (mod 97).
        Iban,
        /// A GSTIN (15 characters, checksum).
        Gstin,
        /// A PAN (ten characters, `AAAAA9999A`).
        Pan,
        /// An IFSC code (`AAAA0XXXXXX`).
        Ifsc,
        /// A UPI id (`name@bank`).
        Upi,
        /// An email address.
        Email,
        /// A telephone number.
        Phone,
        /// An IPv4 address.
        Ip,
    }

    impl Kind {
        /// Every kind, highest priority first.
        pub const ALL: [Kind; 13] = [
            Kind::PrivateKey,
            Kind::Jwt,
            Kind::Secret,
            Kind::Card,
            Kind::Aadhaar,
            Kind::Iban,
            Kind::Gstin,
            Kind::Pan,
            Kind::Ifsc,
            Kind::Upi,
            Kind::Email,
            Kind::Phone,
            Kind::Ip,
        ];

        /// The capital-letter name used in replacements (`EMAIL`).
        #[must_use]
        pub fn label(self) -> &'static str {
            match self {
                Kind::PrivateKey => "PRIVATE_KEY",
                Kind::Jwt => "JWT",
                Kind::Secret => "SECRET",
                Kind::Card => "CARD",
                Kind::Aadhaar => "AADHAAR",
                Kind::Iban => "IBAN",
                Kind::Gstin => "GSTIN",
                Kind::Pan => "PAN",
                Kind::Ifsc => "IFSC",
                Kind::Upi => "UPI",
                Kind::Email => "EMAIL",
                Kind::Phone => "PHONE",
                Kind::Ip => "IP",
            }
        }
    }

    /// What to do with something found.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Action {
        /// Leave it as it is (it is still counted in the report).
        Keep,
        /// Replace it with `[KIND]`. Cannot be undone.
        Mask,
        /// Replace most of it with `X`, keeping a few characters people use to recognise it. Cannot
        /// be undone.
        Partial,
        /// Replace it with `[KIND_n]`, the same token for the same value, which the Washer can
        /// restore when authorised.
        Token,
    }

    /// A set of actions, one per kind.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Policy {
        /// The profile's name.
        pub name: &'static str,
        actions: BTreeMap<Kind, Action>,
    }

    impl Policy {
        fn build(name: &'static str, list: &[(Kind, Action)]) -> Self {
            let mut actions: BTreeMap<Kind, Action> =
                Kind::ALL.iter().map(|k| (*k, Action::Keep)).collect();
            for (k, a) in list {
                actions.insert(*k, *a);
            }
            Self { name, actions }
        }

        /// Secrets only: keys, passwords, tokens and private keys are masked, nothing else is
        /// touched.
        #[must_use]
        pub fn secrets_only() -> Self {
            Self::build(
                "secrets only",
                &[
                    (Kind::PrivateKey, Action::Mask),
                    (Kind::Jwt, Action::Mask),
                    (Kind::Secret, Action::Mask),
                ],
            )
        }

        /// A starting point for the EU General Data Protection Regulation: identifiers that point
        /// to a person (email, phone, IP address, IBAN) are **pseudonymised** (GDPR Article 4(5)),
        /// card numbers and secrets are masked.
        #[must_use]
        pub fn gdpr() -> Self {
            Self::build(
                "GDPR",
                &[
                    (Kind::PrivateKey, Action::Mask),
                    (Kind::Jwt, Action::Mask),
                    (Kind::Secret, Action::Mask),
                    (Kind::Card, Action::Mask),
                    (Kind::Iban, Action::Token),
                    (Kind::Email, Action::Token),
                    (Kind::Phone, Action::Token),
                    (Kind::Ip, Action::Token),
                ],
            )
        }

        /// A starting point for India's Digital Personal Data Protection Act: Aadhaar numbers are
        /// masked in the usual way (the first eight digits hidden), PAN, GSTIN, UPI ids, email,
        /// phone and IP addresses are pseudonymised, cards and secrets are masked. IFSC codes name
        /// a bank branch, not a person, and are kept.
        #[must_use]
        pub fn dpdp() -> Self {
            Self::build(
                "DPDP",
                &[
                    (Kind::PrivateKey, Action::Mask),
                    (Kind::Jwt, Action::Mask),
                    (Kind::Secret, Action::Mask),
                    (Kind::Card, Action::Mask),
                    (Kind::Aadhaar, Action::Partial),
                    (Kind::Pan, Action::Token),
                    (Kind::Gstin, Action::Token),
                    (Kind::Upi, Action::Token),
                    (Kind::Email, Action::Token),
                    (Kind::Phone, Action::Token),
                    (Kind::Ip, Action::Token),
                ],
            )
        }

        /// Everything found is masked, for text that goes to a service nobody vouches for.
        #[must_use]
        pub fn strict() -> Self {
            let all: Vec<(Kind, Action)> = Kind::ALL.iter().map(|k| (*k, Action::Mask)).collect();
            Self::build("strict", &all)
        }

        /// Both laws at once: the stricter action of the two for each kind (masking beats a token,
        /// a token beats keeping).
        #[must_use]
        pub fn gdpr_and_dpdp() -> Self {
            let (g, d) = (Self::gdpr(), Self::dpdp());
            let rank = |a: Action| match a {
                Action::Keep => 0,
                Action::Token => 1,
                Action::Partial => 2,
                Action::Mask => 3,
            };
            let list: Vec<(Kind, Action)> = Kind::ALL
                .iter()
                .map(|k| {
                    let (a, b) = (g.action(*k), d.action(*k));
                    (*k, if rank(a) >= rank(b) { a } else { b })
                })
                .collect();
            Self::build("GDPR and DPDP", &list)
        }

        /// This policy with a different action for one kind.
        #[must_use]
        pub fn with(mut self, kind: Kind, action: Action) -> Self {
            self.actions.insert(kind, action);
            self
        }

        /// The action for a kind.
        #[must_use]
        pub fn action(&self, kind: Kind) -> Action {
            self.actions.get(&kind).copied().unwrap_or(Action::Keep)
        }
    }

    // ------------------------------------------------------------------ checksums

    /// The Luhn check used by payment cards.
    #[must_use]
    pub fn luhn_ok(digits: &str) -> bool {
        if digits.len() < 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let mut sum = 0u32;
        for (i, b) in digits.bytes().rev().enumerate() {
            let mut n = u32::from(b - b'0');
            if i % 2 == 1 {
                n *= 2;
                if n > 9 {
                    n -= 9;
                }
            }
            sum += n;
        }
        sum.is_multiple_of(10)
    }

    /// The Verhoeff check used by Aadhaar numbers.
    #[must_use]
    pub fn verhoeff_ok(digits: &str) -> bool {
        const D: [[u8; 10]; 10] = [
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
            [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
            [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
            [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
            [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
            [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
            [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
            [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
            [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
        ];
        const P: [[u8; 10]; 8] = [
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
            [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
            [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
            [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
            [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
            [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
            [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
        ];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let mut c = 0u8;
        for (i, b) in digits.bytes().rev().enumerate() {
            c = D[usize::from(c)][usize::from(P[i % 8][usize::from(b - b'0')])];
        }
        c == 0
    }

    /// The mod-97 check of an IBAN (spaces removed, upper case).
    #[must_use]
    pub fn iban_ok(compact: &str) -> bool {
        if !(15..=34).contains(&compact.len())
            || !compact
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return false;
        }
        let b = compact.as_bytes();
        if !b[0].is_ascii_uppercase()
            || !b[1].is_ascii_uppercase()
            || !b[2].is_ascii_digit()
            || !b[3].is_ascii_digit()
        {
            return false;
        }
        let mut rem = 0u32;
        for &c in b[4..].iter().chain(b[..4].iter()) {
            let v = if c.is_ascii_digit() {
                u32::from(c - b'0')
            } else {
                u32::from(c - b'A') + 10
            };
            rem = if v >= 10 {
                (rem * 100 + v) % 97
            } else {
                (rem * 10 + v) % 97
            };
        }
        rem == 1
    }

    fn gstin_ok(s: &[u8]) -> bool {
        const CH: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        if s.len() != 15 || !s.iter().all(|c| CH.contains(c)) {
            return false;
        }
        let state = u32::from(s[0] - b'0') * 10 + u32::from(s[1].wrapping_sub(b'0'));
        if !s[0].is_ascii_digit()
            || !s[1].is_ascii_digit()
            || !((1..=38).contains(&state) || state == 97 || state == 99)
        {
            return false;
        }
        if !pan_ok(&s[2..12]) || s[12] == b'0' || s[13] != b'Z' {
            return false;
        }
        let mut total = 0usize;
        for (i, c) in s[..14].iter().enumerate() {
            let v = CH.iter().position(|x| x == c).unwrap_or(0) * if i % 2 == 0 { 1 } else { 2 };
            total += v / 36 + v % 36;
        }
        CH[(36 - total % 36) % 36] == s[14]
    }

    fn pan_ok(s: &[u8]) -> bool {
        s.len() == 10
            && s[..5].iter().all(u8::is_ascii_uppercase)
            && s[5..9].iter().all(u8::is_ascii_digit)
            && s[9].is_ascii_uppercase()
            && b"ABCFGHLJPT".contains(&s[3])
    }

    // ------------------------------------------------------------------ finding things

    type Span = (Kind, usize, usize);

    fn word(b: u8) -> bool {
        b.is_ascii_alphanumeric()
    }

    fn secrets(text: &str, out: &mut Vec<Span>) {
        const KEYWORDS: [&str; 9] = [
            "api_key",
            "api-key",
            "apikey",
            "secret",
            "password",
            "passwd",
            "token",
            "private_key",
            "access_key",
        ];
        let b = text.as_bytes();
        let lc = text.to_ascii_lowercase();
        for kw in KEYWORDS {
            for (i, _) in lc.match_indices(kw) {
                let mut j = i + kw.len();
                while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                if j >= b.len() || (b[j] != b':' && b[j] != b'=') {
                    continue;
                }
                j += 1;
                while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                let quote = (j < b.len() && (b[j] == b'"' || b[j] == b'\'')).then(|| b[j]);
                if quote.is_some() {
                    j += 1;
                }
                let start = j;
                while j < b.len()
                    && match quote {
                        Some(q) => b[j] != q && b[j] != b'\n',
                        None => !b[j].is_ascii_whitespace() && b[j] != b',' && b[j] != b';',
                    }
                {
                    j += 1;
                }
                if j > start {
                    out.push((Kind::Secret, start, j));
                }
            }
        }
        let is_tail = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
        let formats: [(&str, usize); 11] = [
            ("sk-", 20),
            ("gsk_", 20),
            ("ghp_", 30),
            ("gho_", 30),
            ("ghs_", 30),
            ("github_pat_", 30),
            ("AKIA", 16),
            ("AIza", 30),
            ("xoxb-", 20),
            ("xoxp-", 20),
            ("glpat-", 20),
        ];
        for (prefix, min_tail) in formats {
            for (i, _) in text.match_indices(prefix) {
                if i > 0 && word(b[i - 1]) {
                    continue;
                }
                let mut j = i + prefix.len();
                while j < b.len() && is_tail(b[j]) {
                    j += 1;
                }
                if j - i - prefix.len() >= min_tail {
                    out.push((Kind::Secret, i, j));
                }
            }
        }
        // JSON web tokens: three base64url parts, the first starting `eyJ`.
        for (i, _) in text.match_indices("eyJ") {
            if i > 0 && (word(b[i - 1]) || b[i - 1] == b'.') {
                continue;
            }
            let mut j = i;
            let mut parts = 0;
            loop {
                let s = j;
                while j < b.len() && (word(b[j]) || b[j] == b'_' || b[j] == b'-') {
                    j += 1;
                }
                if j - s < 8 {
                    break;
                }
                parts += 1;
                if parts == 3 {
                    break;
                }
                if j < b.len() && b[j] == b'.' {
                    j += 1;
                } else {
                    break;
                }
            }
            if parts == 3 {
                out.push((Kind::Jwt, i, j));
            }
        }
        // PEM private keys.
        for (i, _) in text.match_indices("-----BEGIN ") {
            let rest = &text[i..];
            let Some(line_end) = rest
                .find("-----\n")
                .or_else(|| rest.find("-----\r"))
                .map(|p| p + 5)
            else {
                continue;
            };
            if !rest[..line_end].contains("PRIVATE KEY") {
                continue;
            }
            let end = rest.find("-----END ").map_or(text.len(), |p| {
                rest[p + 9..]
                    .find("-----")
                    .map_or(text.len(), |q| i + p + 9 + q + 5)
            });
            out.push((Kind::PrivateKey, i, end));
        }
    }

    fn emails(text: &str, out: &mut Vec<Span>) {
        const PSP: [&str; 24] = [
            "upi",
            "ybl",
            "ibl",
            "axl",
            "paytm",
            "apl",
            "okhdfcbank",
            "okaxis",
            "oksbi",
            "okicici",
            "hdfcbank",
            "icici",
            "sbi",
            "axisbank",
            "kotak",
            "yesbank",
            "idfcbank",
            "pnb",
            "boi",
            "fbl",
            "airtel",
            "jio",
            "postbank",
            "aubank",
        ];
        let b = text.as_bytes();
        for (at, _) in text.match_indices('@') {
            let mut s = at;
            while s > 0 && (word(b[s - 1]) || matches!(b[s - 1], b'.' | b'_' | b'%' | b'+' | b'-'))
            {
                s -= 1;
            }
            let mut e = at + 1;
            while e < b.len() && (word(b[e]) || matches!(b[e], b'.' | b'-')) {
                e += 1;
            }
            while e > at + 1 && matches!(b[e - 1], b'.' | b'-') {
                e -= 1;
            }
            let local = at - s;
            let domain = &text[at + 1..e];
            if local == 0 || local > 64 || domain.is_empty() {
                continue;
            }
            if domain.contains('.') {
                let tld = domain.rsplit('.').next().unwrap_or("");
                if tld.len() >= 2
                    && tld.bytes().all(|c| c.is_ascii_alphabetic())
                    && !domain.contains("..")
                {
                    out.push((Kind::Email, s, e));
                }
            } else if local >= 2 && PSP.contains(&domain.to_ascii_lowercase().as_str()) {
                out.push((Kind::Upi, s, e));
            }
        }
    }

    /// Digit runs: groups of digits with single spaces or dashes between them. Each run is the list
    /// of its groups as `(start, end)` byte ranges.
    fn digit_runs(b: &[u8]) -> Vec<Vec<(usize, usize)>> {
        let mut runs = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if !b[i].is_ascii_digit() || (i > 0 && (word(b[i - 1]) || b[i - 1] == b'+')) {
                i += 1;
                continue;
            }
            let mut groups = Vec::new();
            let mut j = i;
            loop {
                let s = j;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                groups.push((s, j));
                if j + 1 < b.len() && (b[j] == b' ' || b[j] == b'-') && b[j + 1].is_ascii_digit() {
                    j += 1;
                } else {
                    break;
                }
            }
            if j >= b.len() || !word(b[j]) {
                runs.push(groups);
            }
            i = j.max(i + 1);
        }
        runs
    }

    /// Looks at one run of digit groups (a window of a longer run) for a card, Aadhaar or phone.
    fn classify_digits(digits: &str, lens: &[usize], s: usize, e: usize, out: &mut Vec<Span>) {
        let d = digits.as_bytes();
        let mobile = |i: usize| (b'6'..=b'9').contains(&d[i]);
        let uniform4 = lens.iter().all(|g| *g == 4) || lens.len() == 1;
        if (13..=19).contains(&d.len()) && luhn_ok(digits) {
            out.push((Kind::Card, s, e));
        } else if d.len() == 12
            && (b'2'..=b'9').contains(&d[0])
            && (lens == [12] || lens == [4, 4, 4])
            && verhoeff_ok(digits)
        {
            out.push((Kind::Aadhaar, s, e));
        } else if (d.len() == 10 && mobile(0) && (lens == [10] || lens == [5, 5]))
            || (d.len() == 10 && lens == [3, 3, 4])
            || (d.len() == 12 && digits.starts_with("91") && mobile(2) && uniform4)
            || (d.len() == 11 && digits.starts_with('0') && mobile(1))
        {
            out.push((Kind::Phone, s, e));
        }
    }

    fn numbers(text: &str, out: &mut Vec<Span>) {
        let b = text.as_bytes();
        for groups in digit_runs(b) {
            // Every window of consecutive groups with at most 19 digits, so two numbers written side
            // by side are both found, and a very long run costs nothing.
            for g0 in 0..groups.len() {
                let mut digits = String::new();
                let mut lens = Vec::new();
                for &(s, e) in &groups[g0..] {
                    if digits.len() + (e - s) > 19 {
                        break;
                    }
                    digits.push_str(&text[s..e]);
                    lens.push(e - s);
                    classify_digits(&digits, &lens, groups[g0].0, e, out);
                }
            }
        }
        // International numbers: `+` then 8 to 15 digits with spaces, dashes, dots or brackets.
        for (at, _) in text.match_indices('+') {
            if at > 0 && word(b[at - 1]) {
                continue;
            }
            let mut j = at + 1;
            let mut digits = 0;
            let mut last_digit = at;
            while j < b.len()
                && digits < 16
                && (b[j].is_ascii_digit() || matches!(b[j], b' ' | b'-' | b'.' | b'(' | b')'))
            {
                if b[j].is_ascii_digit() {
                    digits += 1;
                    last_digit = j;
                }
                j += 1;
            }
            let end = last_digit + 1;
            if (8..=15).contains(&digits)
                && b[at + 1].is_ascii_digit()
                && (end >= b.len() || !word(b[end]))
            {
                out.push((Kind::Phone, at, end));
            }
        }
    }

    fn codes(text: &str, out: &mut Vec<Span>) {
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if !word(b[i]) || (i > 0 && word(b[i - 1])) {
                i += 1;
                continue;
            }
            let mut e = i;
            while e < b.len() && word(b[e]) {
                e += 1;
            }
            let w = &b[i..e];
            if w.len() == 15 && gstin_ok(w) {
                out.push((Kind::Gstin, i, e));
            } else if w.len() == 10 && pan_ok(w) {
                out.push((Kind::Pan, i, e));
            } else if w.len() == 11
                && w[..4].iter().all(u8::is_ascii_uppercase)
                && w[4] == b'0'
                && w[5..]
                    .iter()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            {
                out.push((Kind::Ifsc, i, e));
            }
            i = e;
        }
    }

    fn ibans(text: &str, out: &mut Vec<Span>) {
        let b = text.as_bytes();
        let mut i = 0;
        while i + 4 < b.len() {
            if !(b[i].is_ascii_uppercase()
                && b[i + 1].is_ascii_uppercase()
                && b[i + 2].is_ascii_digit()
                && b[i + 3].is_ascii_digit())
                || (i > 0 && word(b[i - 1]))
            {
                i += 1;
                continue;
            }
            // Collect up to 34 letters and digits, skipping single spaces between groups.
            let mut chars: Vec<(usize, u8)> = Vec::new();
            let mut j = i;
            while j < b.len() && chars.len() < 34 {
                if b[j].is_ascii_uppercase() || b[j].is_ascii_digit() {
                    chars.push((j, b[j]));
                    j += 1;
                } else if b[j] == b' '
                    && j + 1 < b.len()
                    && (b[j + 1].is_ascii_uppercase() || b[j + 1].is_ascii_digit())
                {
                    j += 1;
                } else {
                    break;
                }
            }
            for l in (15..=chars.len()).rev() {
                let compact: String = chars[..l].iter().map(|c| char::from(c.1)).collect();
                let end = chars[l - 1].0 + 1;
                if iban_ok(&compact) && (end >= b.len() || !word(b[end])) {
                    out.push((Kind::Iban, i, end));
                    break;
                }
            }
            i += 2;
        }
    }

    fn ips(text: &str, out: &mut Vec<Span>) {
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if !b[i].is_ascii_digit()
                || (i > 0
                    && (word(b[i - 1]) || (b[i - 1] == b'.' && i > 1 && b[i - 2].is_ascii_digit())))
            {
                i += 1;
                continue;
            }
            let mut j = i;
            let mut octets = 0;
            let mut ok = true;
            loop {
                let s = j;
                while j < b.len() && b[j].is_ascii_digit() && j - s < 4 {
                    j += 1;
                }
                let o = &text[s..j];
                if o.is_empty()
                    || o.len() > 3
                    || (o.len() > 1 && o.starts_with('0'))
                    || o.parse::<u16>().map_or(true, |v| v > 255)
                {
                    ok = false;
                    break;
                }
                octets += 1;
                if octets == 4 {
                    break;
                }
                if j < b.len() && b[j] == b'.' {
                    j += 1;
                } else {
                    ok = false;
                    break;
                }
            }
            if ok
                && (j >= b.len()
                    || (!word(b[j])
                        && !(b[j] == b'.' && j + 1 < b.len() && b[j + 1].is_ascii_digit())))
            {
                out.push((Kind::Ip, i, j));
                i = j;
            } else {
                i += 1;
            }
        }
    }

    /// Everything sensitive in `text`, as non-overlapping `(kind, start, end)` spans sorted by
    /// position. When two overlap, the earlier kind in [`Kind::ALL`] wins, then the longer span.
    #[must_use]
    pub fn scan(text: &str) -> Vec<(Kind, usize, usize)> {
        let mut all: Vec<Span> = Vec::new();
        secrets(text, &mut all);
        emails(text, &mut all);
        numbers(text, &mut all);
        codes(text, &mut all);
        ibans(text, &mut all);
        ips(text, &mut all);
        all.retain(|(_, s, e)| {
            s < e && *e <= text.len() && text.is_char_boundary(*s) && text.is_char_boundary(*e)
        });
        all.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then((b.2 - b.1).cmp(&(a.2 - a.1)))
                .then(a.1.cmp(&b.1))
        });
        let mut kept: Vec<Span> = Vec::new();
        for s in all {
            if !kept.iter().any(|k| s.1 < k.2 && k.1 < s.2) {
                kept.push(s);
            }
        }
        kept.sort_by_key(|s| s.1);
        kept
    }

    // ------------------------------------------------------------------ washing

    /// One thing found and what was done about it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Hit {
        /// What it was.
        pub kind: Kind,
        /// Where it was in the original text.
        pub start: usize,
        /// Where it ended.
        pub end: usize,
        /// What was done.
        pub action: Action,
    }

    /// The result of washing.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Washed {
        /// The text with replacements made.
        pub text: String,
        /// Everything found, in order of position.
        pub hits: Vec<Hit>,
    }

    impl Washed {
        /// How many of each kind were found (never the values), for logs and consent records.
        #[must_use]
        pub fn report(&self) -> BTreeMap<Kind, usize> {
            let mut m = BTreeMap::new();
            for h in &self.hits {
                *m.entry(h.kind).or_insert(0) += 1;
            }
            m
        }

        /// Whether anything other than `Keep` was changed.
        #[must_use]
        pub fn changed(&self) -> bool {
            self.hits.iter().any(|h| h.action != Action::Keep)
        }
    }

    /// Replaces all but the last `keep` letters and digits with `X`, leaving separators.
    fn mask_keep_last(s: &str, keep: usize) -> String {
        let total = s.chars().filter(|c| c.is_ascii_alphanumeric()).count();
        let mut seen = 0;
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    seen += 1;
                    if seen > total.saturating_sub(keep) {
                        c
                    } else {
                        'X'
                    }
                } else {
                    c
                }
            })
            .collect()
    }

    fn partial(kind: Kind, original: &str) -> String {
        match kind {
            Kind::Card | Kind::Aadhaar | Kind::Phone => mask_keep_last(original, 4),
            Kind::Iban => {
                let head: String = original.chars().take(4).collect();
                let rest: String = original.chars().skip(4).collect();
                format!("{head}{}", mask_keep_last(&rest, 4))
            }
            Kind::Email => match original.split_once('@') {
                Some((l, d)) => {
                    let first: String = l.chars().take(1).collect();
                    format!("{first}***@{d}")
                }
                None => format!("[{}]", kind.label()),
            },
            _ => format!("[{}]", kind.label()),
        }
    }

    /// Washes text under a [`Policy`] and remembers the tokens it handed out.
    #[derive(Debug, Clone)]
    pub struct Washer {
        policy: Policy,
        tokens: BTreeMap<String, String>,
        by_value: BTreeMap<(Kind, String), String>,
        counters: BTreeMap<Kind, u32>,
    }

    impl Washer {
        /// A washer for `policy`.
        #[must_use]
        pub fn new(policy: Policy) -> Self {
            Self {
                policy,
                tokens: BTreeMap::new(),
                by_value: BTreeMap::new(),
                counters: BTreeMap::new(),
            }
        }

        /// The policy in use.
        #[must_use]
        pub fn policy(&self) -> &Policy {
            &self.policy
        }

        fn token_for(&mut self, kind: Kind, value: &str) -> String {
            if let Some(t) = self.by_value.get(&(kind, value.to_string())) {
                return t.clone();
            }
            if self.tokens.len() >= MAX_TOKENS {
                return format!("[{}]", kind.label());
            }
            let n = self.counters.entry(kind).or_insert(0);
            *n += 1;
            let t = format!("[{}_{}]", kind.label(), n);
            self.tokens.insert(t.clone(), value.to_string());
            self.by_value.insert((kind, value.to_string()), t.clone());
            t
        }

        /// Washes `text`.
        ///
        /// # Errors
        /// [`WashError::TooLarge`] for text over [`MAX_TEXT`] bytes.
        pub fn wash(&mut self, text: &str) -> Result<Washed, WashError> {
            if text.len() > MAX_TEXT {
                return Err(WashError::TooLarge);
            }
            let mut out = String::with_capacity(text.len());
            let mut hits = Vec::new();
            let mut at = 0;
            for (kind, s, e) in scan(text) {
                let action = self.policy.action(kind);
                let original = &text[s..e];
                out.push_str(&text[at..s]);
                match action {
                    Action::Keep => out.push_str(original),
                    Action::Mask => {
                        out.push('[');
                        out.push_str(kind.label());
                        out.push(']');
                    }
                    Action::Partial => out.push_str(&partial(kind, original)),
                    Action::Token => {
                        let t = self.token_for(kind, original);
                        out.push_str(&t);
                    }
                }
                hits.push(Hit {
                    kind,
                    start: s,
                    end: e,
                    action,
                });
                at = e;
            }
            out.push_str(&text[at..]);
            Ok(Washed { text: out, hits })
        }

        /// Puts the original values back in place of this washer's tokens (for example in a model's
        /// reply that mentions `[EMAIL_1]`). Only tokens this washer handed out are replaced.
        ///
        /// # Errors
        /// [`WashError::NotAuthorised`] unless `authorised` is true.
        pub fn restore(&self, text: &str, authorised: bool) -> Result<String, WashError> {
            if !authorised {
                return Err(WashError::NotAuthorised);
            }
            let mut out = String::with_capacity(text.len());
            let mut rest = text;
            while let Some(i) = rest.find('[') {
                out.push_str(&rest[..i]);
                let tail = &rest[i..];
                match tail
                    .find(']')
                    .map(|j| &tail[..=j])
                    .and_then(|t| self.tokens.get(t).map(|v| (t.len(), v)))
                {
                    Some((len, value)) => {
                        out.push_str(value);
                        rest = &tail[len..];
                    }
                    None => {
                        out.push('[');
                        rest = &tail[1..];
                    }
                }
            }
            out.push_str(rest);
            Ok(out)
        }

        /// How many tokens are held.
        #[must_use]
        pub fn token_count(&self) -> usize {
            self.tokens.len()
        }

        /// Forgets every token (the values are gone from memory as far as this washer is concerned).
        pub fn forget(&mut self) {
            self.tokens.clear();
            self.by_value.clear();
            self.counters.clear();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn kinds(t: &str) -> Vec<(Kind, String)> {
            scan(t)
                .into_iter()
                .map(|(k, s, e)| (k, t[s..e].to_string()))
                .collect()
        }

        #[test]
        fn the_checksums_accept_real_numbers_and_refuse_near_misses() {
            assert!(luhn_ok("4111111111111111") && luhn_ok("5500005555555559"));
            assert!(
                !luhn_ok("4111111111111112")
                    && !luhn_ok("411111111111")
                    && !luhn_ok("41111111111a1111")
            );
            assert!(verhoeff_ok("234567890124") && verhoeff_ok("987654321096"));
            assert!(!verhoeff_ok("234567890125") && !verhoeff_ok("") && !verhoeff_ok("12a4"));
            assert!(iban_ok("GB82WEST12345698765432") && iban_ok("DE89370400440532013000"));
            assert!(
                !iban_ok("GB82WEST12345698765433")
                    && !iban_ok("GB82")
                    && !iban_ok("gb82west12345698765432")
            );
            assert!(gstin_ok(b"27AAPFU0939F1ZV") && gstin_ok(b"29ABCPE1234F1Z7"));
            assert!(
                !gstin_ok(b"27AAPFU0939F1ZW")
                    && !gstin_ok(b"99AAPFU0939F1ZV")
                    && !gstin_ok(b"00AAPFU0939F1ZV")
            );
            assert!(pan_ok(b"ABCPE1234F") && !pan_ok(b"ABCDE1234F") && !pan_ok(b"abcpe1234f"));
        }

        #[test]
        fn each_kind_is_found_in_ordinary_text() {
            let cases = [
                ("mail me at jane.doe+x@example.co.uk please", Kind::Email, "jane.doe+x@example.co.uk"),
                ("card 4111 1111 1111 1111 expires", Kind::Card, "4111 1111 1111 1111"),
                ("card 4111-1111-1111-1111.", Kind::Card, "4111-1111-1111-1111"),
                ("Aadhaar: 2345 6789 0124 ok", Kind::Aadhaar, "2345 6789 0124"),
                ("id 234567890124", Kind::Aadhaar, "234567890124"),
                ("pay GB82 WEST 1234 5698 7654 32 now", Kind::Iban, "GB82 WEST 1234 5698 7654 32"),
                ("iban DE89370400440532013000.", Kind::Iban, "DE89370400440532013000"),
                ("GSTIN 27AAPFU0939F1ZV here", Kind::Gstin, "27AAPFU0939F1ZV"),
                ("PAN ABCPE1234F here", Kind::Pan, "ABCPE1234F"),
                ("IFSC HDFC0001234 branch", Kind::Ifsc, "HDFC0001234"),
                ("pay ramesh@okhdfcbank today", Kind::Upi, "ramesh@okhdfcbank"),
                ("call +91 98765 43210 now", Kind::Phone, "+91 98765 43210"),
                ("call 9876543210 now", Kind::Phone, "9876543210"),
                ("call 98765 43210 now", Kind::Phone, "98765 43210"),
                ("call +44 20 7946 0958 now", Kind::Phone, "+44 20 7946 0958"),
                ("call 555-123-4567 now", Kind::Phone, "555-123-4567"),
                ("server 192.168.1.20 is down", Kind::Ip, "192.168.1.20"),
                ("see 10.0.0.1.", Kind::Ip, "10.0.0.1"),
                ("password: hunter2hunter2", Kind::Secret, "hunter2hunter2"),
                ("API_KEY=\"abc def ghi\"", Kind::Secret, "abc def ghi"),
                ("key sk-abcdefghijklmnopqrstuv end", Kind::Secret, "sk-abcdefghijklmnopqrstuv"),
                ("aws AKIAABCDEFGHIJKLMNOP end", Kind::Secret, "AKIAABCDEFGHIJKLMNOP"),
                ("Authorization eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk ok", Kind::Jwt, "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            ];
            for (text, kind, value) in cases {
                assert_eq!(kinds(text), vec![(kind, value.to_string())], "{text}");
            }
            let pem = "key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----\nafter";
            let f = kinds(pem);
            assert_eq!(f.len(), 1);
            assert_eq!(f[0].0, Kind::PrivateKey);
            assert!(f[0].1.starts_with("-----BEGIN") && f[0].1.ends_with("PRIVATE KEY-----"));
        }

        #[test]
        fn ordinary_text_and_look_alikes_are_left_alone() {
            for t in [
                "Hello, how are you? Order #12345 shipped on 2024-03-15.",
                "version 1.2.3.4.5 released",
                "1.2.3 and 300.1.1.1 and 01.02.03.04",
                "call 12345",
                "4111 1111 1111 1112",
                "234567890125",
                "ABCDE1234F",
                "user@localhost",
                "a@b",
                "@@@@",
                "me@example.c",
                "price 1,234,567.89",
                "sk-short",
                "password is long",
                "token",
                "-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----",
                "IBAN GB82 WEST 1234 5698 7654 33",
                "eyJabc.def.ghi",
            ] {
                assert!(kinds(t).is_empty(), "{t:?} -> {:?}", kinds(t));
            }
        }

        #[test]
        fn overlaps_resolve_to_the_more_specific_kind() {
            // A GSTIN contains a PAN; the PAN inside is not reported separately.
            assert_eq!(
                kinds("27AAPFU0939F1ZV"),
                vec![(Kind::Gstin, "27AAPFU0939F1ZV".into())]
            );
            // A valid card number is not also a phone number.
            assert_eq!(kinds("4111111111111111")[0].0, Kind::Card);
            // The email wins over the phone-like digits inside it.
            assert_eq!(
                kinds("9876543210@example.com"),
                vec![(Kind::Email, "9876543210@example.com".into())]
            );
            // A secret value that contains an email-like part is one secret.
            let f = kinds("password=jane@example.com");
            assert_eq!(f.len(), 1);
            assert_eq!(f[0].0, Kind::Secret);
        }

        #[test]
        fn the_gdpr_profile_pseudonymises_identifiers_and_masks_cards_and_secrets() {
            let mut w = Washer::new(Policy::gdpr());
            let r = w
                .wash("Mail jane@example.com or +44 20 7946 0958, again jane@example.com. Card 4111 1111 1111 1111, ip 10.0.0.7, password=hunter2hunter2")
                .unwrap();
            assert_eq!(
                r.text,
                "Mail [EMAIL_1] or [PHONE_1], again [EMAIL_1]. Card [CARD], ip [IP_1], password=[SECRET]"
            );
            assert_eq!(r.report()[&Kind::Email], 2);
            assert_eq!(w.token_count(), 3);
            assert!(r.changed());
            // The reply mentions a token; restoring needs authorisation.
            assert_eq!(
                w.restore("Write to [EMAIL_1].", false),
                Err(WashError::NotAuthorised)
            );
            assert_eq!(
                w.restore("Write to [EMAIL_1].", true).unwrap(),
                "Write to jane@example.com."
            );
            assert_eq!(
                w.restore("[CARD] [EMAIL_9] [EMAIL_1] [ [x] ]", true)
                    .unwrap(),
                "[CARD] [EMAIL_9] jane@example.com [ [x] ]"
            );
        }

        #[test]
        fn the_dpdp_profile_partly_masks_aadhaar_and_pseudonymises_indian_identifiers() {
            let mut w = Washer::new(Policy::dpdp());
            let r = w
                .wash("Aadhaar 2345 6789 0124, PAN ABCPE1234F, GSTIN 27AAPFU0939F1ZV, UPI ramesh@okhdfcbank, IFSC HDFC0001234, +91 98765 43210")
                .unwrap();
            assert_eq!(
                r.text,
                "Aadhaar XXXX XXXX 0124, PAN [PAN_1], GSTIN [GSTIN_1], UPI [UPI_1], IFSC HDFC0001234, [PHONE_1]"
            );
            let ifsc = r.hits.iter().find(|h| h.kind == Kind::Ifsc).unwrap();
            assert_eq!(ifsc.action, Action::Keep, "kept, but counted");
            assert_eq!(r.report()[&Kind::Ifsc], 1);
            assert!(
                !r.text.contains("2345") && !r.text.contains("6789"),
                "the first eight digits are gone"
            );
        }

        #[test]
        fn the_combined_and_other_profiles_behave_as_described() {
            let both = Policy::gdpr_and_dpdp();
            assert_eq!(both.action(Kind::Aadhaar), Action::Partial);
            assert_eq!(both.action(Kind::Card), Action::Mask);
            assert_eq!(both.action(Kind::Email), Action::Token);
            assert_eq!(both.action(Kind::Ifsc), Action::Keep);
            let s = Washer::new(Policy::secrets_only())
                .wash("mail a@b.com secret=abcdef1234 and 9876543210")
                .unwrap();
            assert_eq!(s.text, "mail a@b.com secret=[SECRET] and 9876543210");
            let strict = Washer::new(Policy::strict())
                .wash("a@b.com 10.0.0.1 HDFC0001234")
                .unwrap();
            assert_eq!(strict.text, "[EMAIL] [IP] [IFSC]");
            let custom = Washer::new(Policy::gdpr().with(Kind::Email, Action::Partial))
                .wash("jane@example.com")
                .unwrap();
            assert_eq!(custom.text, "j***@example.com");
        }

        #[test]
        fn partial_masking_keeps_only_what_people_use_to_recognise_a_value() {
            let w = |p: Policy, t: &str| Washer::new(p).wash(t).unwrap().text;
            let p = Policy::build(
                "t",
                &[
                    (Kind::Card, Action::Partial),
                    (Kind::Phone, Action::Partial),
                    (Kind::Iban, Action::Partial),
                ],
            );
            assert_eq!(w(p.clone(), "4111-1111-1111-1111"), "XXXX-XXXX-XXXX-1111");
            assert_eq!(w(p.clone(), "+91 98765 43210"), "+XX XXXXX X3210");
            assert_eq!(
                w(p, "GB82 WEST 1234 5698 7654 32"),
                "GB82 XXXX XXXX XXXX XX54 32"
            );
        }

        #[test]
        fn washing_twice_changes_nothing_more_and_a_clean_text_is_returned_unchanged() {
            let mut w = Washer::new(Policy::gdpr_and_dpdp());
            let first = w
                .wash("jane@example.com 4111 1111 1111 1111 2345 6789 0124 secret=abcdef1234")
                .unwrap();
            let second = Washer::new(Policy::gdpr_and_dpdp())
                .wash(&first.text)
                .unwrap();
            assert_eq!(first.text, second.text);
            let clean = "Nothing to see here, 42 apples.";
            let r = w.wash(clean).unwrap();
            assert_eq!(r.text, clean);
            assert!(!r.changed() && r.hits.is_empty());
        }

        #[test]
        fn tokens_are_stable_per_value_and_bounded() {
            let mut w = Washer::new(Policy::gdpr());
            let r = w.wash("a@x.com b@x.com a@x.com").unwrap();
            assert_eq!(r.text, "[EMAIL_1] [EMAIL_2] [EMAIL_1]");
            w.forget();
            assert_eq!(w.token_count(), 0);
            assert_eq!(
                w.restore("[EMAIL_1]", true).unwrap(),
                "[EMAIL_1]",
                "forgotten tokens restore to nothing"
            );
            let mut big = Washer::new(Policy::gdpr());
            let mut text = String::new();
            for i in 0..(MAX_TOKENS + 5) {
                text.push_str(&format!("u{i}@x.com "));
            }
            let r = big.wash(&text).unwrap();
            assert_eq!(big.token_count(), MAX_TOKENS);
            assert!(
                r.text.ends_with("[EMAIL] "),
                "beyond the limit values are masked, not leaked"
            );
            assert!(!r.text.contains("@x.com"));
        }

        #[test]
        fn hostile_and_unusual_text_never_panics_or_leaves_invalid_output() {
            let mut w = Washer::new(Policy::strict());
            let nasty = [
                "",
                "@",
                "+",
                "-----BEGIN PRIVATE KEY-----",
                "-----BEGIN PRIVATE KEY-----\nabc",
                "eyJ.eyJ.eyJ",
                "日本語@例え.jp 電話 090-1234-5678",
                "ünïcode@exämple.com a@é.com",
                "🙂 4111 1111 1111 1111 🙂",
                "%%%%%%%%%%@%%%%%%%%",
                "password=",
                "password:\n",
                "9999999999999999999999999999999999999999999999999999999999999999999999999999",
                "1.2.3.",
                "...1.2.3.4...",
            ];
            for t in nasty {
                let r = w.wash(t).unwrap();
                assert!(std::str::from_utf8(r.text.as_bytes()).is_ok());
            }
            // Pseudo-random mixtures of the interesting characters.
            let alphabet: Vec<char> = "0123456789 -+.@_%=:\n\tabcXYZ日é$".chars().collect();
            let mut x: u64 = 0x2545_F491_4F6C_DD1D;
            for _ in 0..300 {
                let mut s = String::new();
                for _ in 0..200 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    s.push(alphabet[(x % alphabet.len() as u64) as usize]);
                }
                let r = w.wash(&s).unwrap();
                for h in &r.hits {
                    assert!(
                        h.start < h.end
                            && h.end <= s.len()
                            && s.is_char_boundary(h.start)
                            && s.is_char_boundary(h.end)
                    );
                }
            }
        }

        #[test]
        fn very_large_inputs_are_handled_in_reasonable_time_and_the_limit_holds() {
            let mut w = Washer::new(Policy::strict());
            let long_digits = "1".repeat(2_000_000);
            let t = std::time::Instant::now();
            assert!(w.wash(&long_digits).unwrap().hits.is_empty());
            let many_at = "a@".repeat(500_000);
            w.wash(&many_at).unwrap();
            let dots = "1.".repeat(500_000);
            w.wash(&dots).unwrap();
            assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
            assert_eq!(w.wash(&"x".repeat(MAX_TEXT + 1)), Err(WashError::TooLarge));
            assert!(w.wash(&"x".repeat(MAX_TEXT)).is_ok());
        }
    }
}

/// Privacy obligations: consent, requests from people about their data, retention and breach
/// clocks, for GDPR and DPDP.
///
/// This is bookkeeping, not legal advice. It records who agreed to what, works out when a reply to
/// a request or a notification is due, and finds records past their retention. The **time limits
/// are defaults to be confirmed with your own legal advice** (laws, rules and guidance change,
/// and some limits depend on circumstances); every limit can be changed. Nothing here makes a
/// product compliant on its own.
#[allow(clippy::module_inception)]
pub mod privacy {
    #![forbid(unsafe_code)]

    use std::collections::BTreeMap;
    use std::fmt;

    /// Seconds in a day.
    pub const DAY: u64 = 86_400;
    /// Most consent records kept.
    pub const MAX_CONSENTS: usize = 100_000;
    /// Most open requests kept.
    pub const MAX_REQUESTS: usize = 100_000;

    /// Why a privacy call was refused.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PrivacyError {
        /// An input is invalid (the text says which).
        Invalid(&'static str),
        /// A limit was reached (the text says which).
        Full(&'static str),
        /// No such request.
        NotFound,
        /// The request is already closed, or was already extended.
        State(&'static str),
    }

    impl fmt::Display for PrivacyError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Invalid(w) => write!(f, "invalid {w}"),
                Self::Full(w) => write!(f, "limit reached: {w}"),
                Self::NotFound => f.write_str("no such request"),
                Self::State(w) => f.write_str(w),
            }
        }
    }

    impl std::error::Error for PrivacyError {}

    /// Which law a record falls under.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Regime {
        /// The EU General Data Protection Regulation.
        Gdpr,
        /// India's Digital Personal Data Protection Act, 2023.
        Dpdp,
    }

    /// The legal ground for using personal data.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Basis {
        /// The person agreed (GDPR Article 6(1)(a); DPDP consent).
        Consent,
        /// Needed for a contract.
        Contract,
        /// Needed to meet a legal duty.
        LegalObligation,
        /// Needed to protect someone's life.
        VitalInterests,
        /// Needed for a public task.
        PublicTask,
        /// A legitimate interest (GDPR only).
        LegitimateInterests,
        /// One of the "certain legitimate uses" the DPDP Act allows without consent.
        LegitimateUse,
    }

    fn name_ok(s: &str) -> bool {
        !s.is_empty() && s.len() <= 100 && !s.chars().any(char::is_control)
    }

    /// What to record when someone agrees.
    #[derive(Debug, Clone, Copy)]
    pub struct NewConsent<'a> {
        /// Who agreed (an id, not a name).
        pub principal: &'a str,
        /// What for.
        pub purpose: &'a str,
        /// The law.
        pub regime: Regime,
        /// The ground.
        pub basis: Basis,
        /// Which version of the notice they were shown.
        pub notice_version: &'a str,
        /// The language of the notice.
        pub language: &'a str,
    }

    /// One recorded agreement.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Consent {
        /// Who agreed (an id, not a name).
        pub principal: String,
        /// What for (`chat_history`, `hosted_models`, ...).
        pub purpose: String,
        /// The law.
        pub regime: Regime,
        /// The ground.
        pub basis: Basis,
        /// Which version of the notice they were shown.
        pub notice_version: String,
        /// The language of the notice.
        pub language: String,
        /// When they agreed.
        pub given_at: u64,
        /// When they took it back.
        pub withdrawn_at: Option<u64>,
    }

    /// The record of consents.
    #[derive(Debug, Clone, Default)]
    pub struct ConsentLedger {
        records: Vec<Consent>,
    }

    impl ConsentLedger {
        /// An empty ledger.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Records an agreement.
        ///
        /// # Errors
        /// [`PrivacyError`] for empty or odd names, a basis the law does not have, or when the
        /// ledger is full.
        pub fn give(&mut self, c: &NewConsent<'_>, now: u64) -> Result<(), PrivacyError> {
            let (principal, purpose, regime, basis) = (c.principal, c.purpose, c.regime, c.basis);
            let (notice_version, language) = (c.notice_version, c.language);
            if ![principal, purpose, notice_version, language]
                .iter()
                .all(|s| name_ok(s))
            {
                return Err(PrivacyError::Invalid("name"));
            }
            if regime == Regime::Gdpr && basis == Basis::LegitimateUse {
                return Err(PrivacyError::Invalid("basis for GDPR"));
            }
            if regime == Regime::Dpdp && basis == Basis::LegitimateInterests {
                return Err(PrivacyError::Invalid("basis for DPDP"));
            }
            if self.records.len() >= MAX_CONSENTS {
                return Err(PrivacyError::Full("consents"));
            }
            self.records.push(Consent {
                principal: principal.into(),
                purpose: purpose.into(),
                regime,
                basis,
                notice_version: notice_version.into(),
                language: language.into(),
                given_at: now,
                withdrawn_at: None,
            });
            Ok(())
        }

        /// Records that `principal` took back their agreement for `purpose`. Withdrawing is as easy
        /// as giving: one call. Returns how many agreements were withdrawn.
        pub fn withdraw(&mut self, principal: &str, purpose: &str, now: u64) -> usize {
            let mut n = 0;
            for c in &mut self.records {
                if c.principal == principal && c.purpose == purpose && c.withdrawn_at.is_none() {
                    c.withdrawn_at = Some(now);
                    n += 1;
                }
            }
            n
        }

        /// Whether `principal` has a current agreement for `purpose` at `now` (given, and not
        /// withdrawn). Uses that rest on another basis are not decided here.
        #[must_use]
        pub fn allows(&self, principal: &str, purpose: &str, now: u64) -> bool {
            self.records.iter().any(|c| {
                c.principal == principal
                    && c.purpose == purpose
                    && c.basis == Basis::Consent
                    && c.given_at <= now
                    && c.withdrawn_at.is_none_or(|w| w > now)
            })
        }

        /// Everything recorded about one person, oldest first.
        #[must_use]
        pub fn history(&self, principal: &str) -> Vec<&Consent> {
            self.records
                .iter()
                .filter(|c| c.principal == principal)
                .collect()
        }

        /// Removes everything recorded about one person (after an erasure request). Returns how
        /// many records went.
        pub fn erase(&mut self, principal: &str) -> usize {
            let before = self.records.len();
            self.records.retain(|c| c.principal != principal);
            before - self.records.len()
        }
    }

    /// What a person asks for.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum RequestKind {
        /// See the data held about them.
        Access,
        /// Correct data.
        Correction,
        /// Erase data.
        Erasure,
        /// Receive their data in a usable form (GDPR).
        Portability,
        /// Limit how data is used (GDPR).
        Restriction,
        /// Object to a use (GDPR).
        Objection,
        /// Complain to the data fiduciary (DPDP).
        Grievance,
        /// Name someone to act for them (DPDP).
        Nomination,
    }

    impl RequestKind {
        /// Whether the law provides this request.
        #[must_use]
        pub fn allowed_under(self, r: Regime) -> bool {
            matches!(
                (self, r),
                (Self::Access | Self::Correction | Self::Erasure, _)
                    | (
                        Self::Portability | Self::Restriction | Self::Objection,
                        Regime::Gdpr
                    )
                    | (Self::Grievance | Self::Nomination, Regime::Dpdp)
            )
        }
    }

    /// How long the response may take, in days.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Deadlines {
        /// Days to answer.
        pub respond_days: u64,
        /// Extra days allowed once, with a reason (0 when none).
        pub extension_days: u64,
    }

    impl Deadlines {
        /// GDPR Article 12(3): one month (30 days here), extendable by two months for complex or
        /// numerous requests.
        #[must_use]
        pub fn gdpr_default() -> Self {
            Self {
                respond_days: 30,
                extension_days: 60,
            }
        }

        /// A default for DPDP: 90 days, the period set for resolving grievances in the Rules. Check
        /// the current Rules and guidance, which may set other periods for other requests.
        #[must_use]
        pub fn dpdp_default() -> Self {
            Self {
                respond_days: 90,
                extension_days: 0,
            }
        }
    }

    /// Where a request stands.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        /// Received, not answered.
        Open,
        /// Answered.
        Closed(u64),
    }

    /// A request from a person.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Request {
        /// A running number.
        pub id: u64,
        /// Who asked.
        pub principal: String,
        /// What they asked.
        pub kind: RequestKind,
        /// The law.
        pub regime: Regime,
        /// When it arrived.
        pub received: u64,
        /// When an answer is due.
        pub due: u64,
        /// Whether the deadline was extended, and why.
        pub extended: Option<String>,
        /// Where it stands.
        pub status: Status,
    }

    /// The queue of requests.
    #[derive(Debug, Clone)]
    pub struct RequestQueue {
        requests: Vec<Request>,
        next: u64,
        gdpr: Deadlines,
        dpdp: Deadlines,
    }

    impl Default for RequestQueue {
        fn default() -> Self {
            Self::new(Deadlines::gdpr_default(), Deadlines::dpdp_default())
        }
    }

    impl RequestQueue {
        /// A queue with the given deadlines for each law.
        #[must_use]
        pub fn new(gdpr: Deadlines, dpdp: Deadlines) -> Self {
            Self {
                requests: Vec::new(),
                next: 1,
                gdpr,
                dpdp,
            }
        }

        /// Opens a request and works out when it is due.
        ///
        /// # Errors
        /// [`PrivacyError`] for a bad name, a request the law does not provide, or a full queue.
        pub fn open(
            &mut self,
            principal: &str,
            kind: RequestKind,
            regime: Regime,
            now: u64,
        ) -> Result<u64, PrivacyError> {
            if !name_ok(principal) {
                return Err(PrivacyError::Invalid("principal"));
            }
            if !kind.allowed_under(regime) {
                return Err(PrivacyError::Invalid("request for that law"));
            }
            if self.requests.len() >= MAX_REQUESTS {
                return Err(PrivacyError::Full("requests"));
            }
            let d = if regime == Regime::Gdpr {
                self.gdpr
            } else {
                self.dpdp
            };
            let id = self.next;
            self.next += 1;
            self.requests.push(Request {
                id,
                principal: principal.into(),
                kind,
                regime,
                received: now,
                due: now.saturating_add(d.respond_days.saturating_mul(DAY)),
                extended: None,
                status: Status::Open,
            });
            Ok(id)
        }

        fn find(&mut self, id: u64) -> Result<&mut Request, PrivacyError> {
            self.requests
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or(PrivacyError::NotFound)
        }

        /// Extends the deadline once, where the law allows it, with a reason.
        ///
        /// # Errors
        /// [`PrivacyError`] when the request is closed, already extended, the law gives no
        /// extension, or the reason is empty.
        pub fn extend(&mut self, id: u64, reason: &str) -> Result<u64, PrivacyError> {
            if !name_ok(reason) {
                return Err(PrivacyError::Invalid("reason"));
            }
            let (gdpr, dpdp) = (self.gdpr, self.dpdp);
            let r = self.find(id)?;
            let d = if r.regime == Regime::Gdpr { gdpr } else { dpdp };
            if r.status != Status::Open {
                return Err(PrivacyError::State("the request is closed"));
            }
            if r.extended.is_some() {
                return Err(PrivacyError::State("the request was already extended"));
            }
            if d.extension_days == 0 {
                return Err(PrivacyError::State(
                    "the law gives no extension for this request",
                ));
            }
            r.due = r.due.saturating_add(d.extension_days.saturating_mul(DAY));
            r.extended = Some(reason.chars().take(100).collect());
            Ok(r.due)
        }

        /// Records the answer.
        ///
        /// # Errors
        /// [`PrivacyError::NotFound`] or [`PrivacyError::State`] when already closed.
        pub fn close(&mut self, id: u64, now: u64) -> Result<(), PrivacyError> {
            let r = self.find(id)?;
            if r.status != Status::Open {
                return Err(PrivacyError::State("the request is already closed"));
            }
            r.status = Status::Closed(now);
            Ok(())
        }

        /// Open requests past their deadline at `now`, most overdue first.
        #[must_use]
        pub fn overdue(&self, now: u64) -> Vec<&Request> {
            let mut v: Vec<&Request> = self
                .requests
                .iter()
                .filter(|r| r.status == Status::Open && r.due < now)
                .collect();
            v.sort_by_key(|r| r.due);
            v
        }

        /// Open requests due within `days` days (and not yet overdue), soonest first.
        #[must_use]
        pub fn due_within(&self, now: u64, days: u64) -> Vec<&Request> {
            let limit = now.saturating_add(days.saturating_mul(DAY));
            let mut v: Vec<&Request> = self
                .requests
                .iter()
                .filter(|r| r.status == Status::Open && r.due >= now && r.due <= limit)
                .collect();
            v.sort_by_key(|r| r.due);
            v
        }

        /// All requests of one person.
        #[must_use]
        pub fn of(&self, principal: &str) -> Vec<&Request> {
            self.requests
                .iter()
                .filter(|r| r.principal == principal)
                .collect()
        }
    }

    /// How long each category of record may be kept.
    #[derive(Debug, Clone, Default)]
    pub struct Retention {
        days: BTreeMap<String, u64>,
    }

    impl Retention {
        /// No rules yet.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Sets how long records of `category` may be kept.
        ///
        /// # Errors
        /// [`PrivacyError::Invalid`] for an odd name or a zero period.
        pub fn rule(mut self, category: &str, days: u64) -> Result<Self, PrivacyError> {
            if !name_ok(category) || days == 0 {
                return Err(PrivacyError::Invalid("retention rule"));
            }
            self.days.insert(category.into(), days);
            Ok(self)
        }

        /// The ids of records past their retention at `now`: `records` are `(id, category,
        /// created)`. A category with no rule is never reported (nothing says it must go).
        #[must_use]
        pub fn due_for_erasure<'a>(
            &self,
            records: &'a [(String, String, u64)],
            now: u64,
        ) -> Vec<&'a str> {
            records
                .iter()
                .filter(|(_, cat, created)| {
                    self.days
                        .get(cat)
                        .is_some_and(|d| now.saturating_sub(*created) > d.saturating_mul(DAY))
                })
                .map(|(id, _, _)| id.as_str())
                .collect()
        }
    }

    /// A personal data breach.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Breach {
        /// The law.
        pub regime: Regime,
        /// When the organisation became aware of it.
        pub aware_at: u64,
        /// How many people may be affected.
        pub affected: u64,
    }

    impl Breach {
        /// When the authority must be told: 72 hours after becoming aware. This is GDPR Article
        /// 33; for DPDP it is the period for the detailed report to the Data Protection Board under
        /// the Rules (a first intimation is due without delay). Confirm the current Rules.
        #[must_use]
        pub fn authority_deadline(&self) -> u64 {
            self.aware_at.saturating_add(72 * 3_600)
        }

        /// What to tell the affected people, in one sentence.
        #[must_use]
        pub fn people_note(&self) -> &'static str {
            match self.regime {
                Regime::Gdpr => "Tell affected people without undue delay when the breach is likely to put them at high risk.",
                Regime::Dpdp => "Tell each affected person without delay, in plain language, what happened and what they can do.",
            }
        }

        /// Whether the authority deadline has passed at `now`.
        #[must_use]
        pub fn overdue(&self, now: u64) -> bool {
            now > self.authority_deadline()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn nc<'a>(
            principal: &'a str,
            purpose: &'a str,
            regime: Regime,
            basis: Basis,
            notice_version: &'a str,
            language: &'a str,
        ) -> NewConsent<'a> {
            NewConsent {
                principal,
                purpose,
                regime,
                basis,
                notice_version,
                language,
            }
        }

        fn give(l: &mut ConsentLedger, p: &str, purpose: &str, now: u64) {
            l.give(
                &nc(p, purpose, Regime::Dpdp, Basis::Consent, "v1", "en"),
                now,
            )
            .unwrap();
        }

        #[test]
        fn consent_is_given_checked_and_withdrawn() {
            let mut l = ConsentLedger::new();
            assert!(!l.allows("u1", "hosted_models", 10));
            give(&mut l, "u1", "hosted_models", 10);
            assert!(
                !l.allows("u1", "hosted_models", 9),
                "not before it was given"
            );
            assert!(l.allows("u1", "hosted_models", 10));
            assert!(!l.allows("u1", "other_purpose", 10));
            assert!(!l.allows("u2", "hosted_models", 10));
            assert_eq!(l.withdraw("u1", "hosted_models", 20), 1);
            assert!(
                l.allows("u1", "hosted_models", 15),
                "it held until withdrawn"
            );
            assert!(!l.allows("u1", "hosted_models", 20));
            assert_eq!(
                l.withdraw("u1", "hosted_models", 30),
                0,
                "already withdrawn"
            );
            give(&mut l, "u1", "hosted_models", 40);
            assert!(l.allows("u1", "hosted_models", 41), "they can agree again");
            assert_eq!(l.history("u1").len(), 2);
        }

        #[test]
        fn only_consent_counts_as_consent_and_the_basis_must_fit_the_law() {
            let mut l = ConsentLedger::new();
            l.give(&nc("u", "p", Regime::Gdpr, Basis::Contract, "v1", "en"), 0)
                .unwrap();
            assert!(!l.allows("u", "p", 5), "a contract basis is not consent");
            assert!(l
                .give(
                    &nc("u", "p", Regime::Gdpr, Basis::LegitimateUse, "v1", "en"),
                    0
                )
                .is_err());
            assert!(l
                .give(
                    &nc(
                        "u",
                        "p",
                        Regime::Dpdp,
                        Basis::LegitimateInterests,
                        "v1",
                        "en"
                    ),
                    0
                )
                .is_err());
            assert!(l
                .give(
                    &nc("u", "p", Regime::Dpdp, Basis::LegitimateUse, "v1", "en"),
                    0
                )
                .is_ok());
            for (a, b, c, d) in [
                ("", "p", "v1", "en"),
                ("u", "", "v1", "en"),
                ("u", "p", "", "en"),
                ("u", "p", "v1", ""),
                ("u\n", "p", "v1", "en"),
            ] {
                assert!(
                    l.give(&nc(a, b, Regime::Gdpr, Basis::Consent, c, d), 0)
                        .is_err(),
                    "{a:?} {b:?} {c:?} {d:?}"
                );
            }
        }

        #[test]
        fn erasing_a_person_removes_their_consent_records_only() {
            let mut l = ConsentLedger::new();
            give(&mut l, "u1", "a", 1);
            give(&mut l, "u1", "b", 1);
            give(&mut l, "u2", "a", 1);
            assert_eq!(l.erase("u1"), 2);
            assert!(l.history("u1").is_empty());
            assert_eq!(l.history("u2").len(), 1);
            assert_eq!(l.erase("nobody"), 0);
        }

        #[test]
        fn requests_get_the_right_deadline_for_each_law() {
            let mut q = RequestQueue::default();
            let g = q
                .open("u1", RequestKind::Access, Regime::Gdpr, 1_000)
                .unwrap();
            let d = q
                .open("u2", RequestKind::Grievance, Regime::Dpdp, 1_000)
                .unwrap();
            let by = |id| q.of(if id == g { "u1" } else { "u2" })[0].due;
            assert_eq!(by(g), 1_000 + 30 * DAY);
            assert_eq!(by(d), 1_000 + 90 * DAY);
            // Requests the law does not provide are refused.
            assert!(q
                .open("u", RequestKind::Portability, Regime::Dpdp, 0)
                .is_err());
            assert!(q
                .open("u", RequestKind::Grievance, Regime::Gdpr, 0)
                .is_err());
            assert!(q
                .open("u", RequestKind::Nomination, Regime::Gdpr, 0)
                .is_err());
            assert!(q
                .open("u", RequestKind::Objection, Regime::Dpdp, 0)
                .is_err());
            assert!(q.open("", RequestKind::Access, Regime::Gdpr, 0).is_err());
            for k in [
                RequestKind::Access,
                RequestKind::Correction,
                RequestKind::Erasure,
            ] {
                assert!(k.allowed_under(Regime::Gdpr) && k.allowed_under(Regime::Dpdp));
            }
        }

        #[test]
        fn a_gdpr_request_can_be_extended_once_and_a_dpdp_one_not_by_default() {
            let mut q = RequestQueue::default();
            let g = q.open("u1", RequestKind::Erasure, Regime::Gdpr, 0).unwrap();
            assert_eq!(q.extend(g, "complex request"), Ok((30 + 60) * DAY));
            assert!(matches!(q.extend(g, "again"), Err(PrivacyError::State(_))));
            assert!(q.extend(g, "").is_err());
            let d = q.open("u2", RequestKind::Access, Regime::Dpdp, 0).unwrap();
            assert!(matches!(
                q.extend(d, "why not"),
                Err(PrivacyError::State(_))
            ));
            q.close(g, 5).unwrap();
            assert!(matches!(q.close(g, 6), Err(PrivacyError::State(_))));
            assert_eq!(q.extend(999, "x"), Err(PrivacyError::NotFound));
            let c = q.open("u3", RequestKind::Access, Regime::Gdpr, 0).unwrap();
            q.close(c, 1).unwrap();
            assert!(matches!(q.extend(c, "late"), Err(PrivacyError::State(_))));
        }

        #[test]
        fn overdue_and_upcoming_requests_are_listed_in_order() {
            let mut q = RequestQueue::new(
                Deadlines {
                    respond_days: 10,
                    extension_days: 0,
                },
                Deadlines::dpdp_default(),
            );
            let a = q.open("a", RequestKind::Access, Regime::Gdpr, 0).unwrap();
            let b = q
                .open("b", RequestKind::Access, Regime::Gdpr, 5 * DAY)
                .unwrap();
            let c = q
                .open("c", RequestKind::Access, Regime::Gdpr, 50 * DAY)
                .unwrap();
            let now = 20 * DAY;
            assert_eq!(
                q.overdue(now).iter().map(|r| r.id).collect::<Vec<_>>(),
                vec![a, b]
            );
            assert!(q.due_within(now, 5).is_empty());
            assert_eq!(
                q.due_within(now, 50)
                    .iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>(),
                vec![c]
            );
            q.close(a, now).unwrap();
            assert_eq!(
                q.overdue(now).iter().map(|r| r.id).collect::<Vec<_>>(),
                vec![b],
                "answered ones are not overdue"
            );
        }

        #[test]
        fn retention_names_only_what_has_outlived_its_rule() {
            let r = Retention::new()
                .rule("chat", 30)
                .unwrap()
                .rule("logs", 7)
                .unwrap();
            let recs = vec![
                ("c1".to_string(), "chat".to_string(), 0),
                ("c2".to_string(), "chat".to_string(), 20 * DAY),
                ("l1".to_string(), "logs".to_string(), 0),
                ("x1".to_string(), "unruled".to_string(), 0),
            ];
            assert_eq!(r.due_for_erasure(&recs, 31 * DAY), vec!["c1", "l1"]);
            assert_eq!(
                r.due_for_erasure(&recs, 30 * DAY),
                vec!["l1"],
                "exactly 30 days is still allowed"
            );
            assert!(r.due_for_erasure(&recs, 0).is_empty());
            assert!(Retention::new().rule("", 1).is_err());
            assert!(Retention::new().rule("chat", 0).is_err());
        }

        #[test]
        fn a_breach_has_a_seventy_two_hour_authority_clock() {
            let b = Breach {
                regime: Regime::Gdpr,
                aware_at: 1_000,
                affected: 5,
            };
            assert_eq!(b.authority_deadline(), 1_000 + 72 * 3_600);
            assert!(!b.overdue(1_000 + 72 * 3_600));
            assert!(b.overdue(1_000 + 72 * 3_600 + 1));
            assert!(b.people_note().contains("high risk"));
            let d = Breach {
                regime: Regime::Dpdp,
                ..b
            };
            assert!(d.people_note().contains("without delay"));
            assert_eq!(
                Breach {
                    aware_at: u64::MAX,
                    ..d
                }
                .authority_deadline(),
                u64::MAX,
                "no wrap-around"
            );
        }
    }
}
