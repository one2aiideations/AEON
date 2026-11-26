//! Keeps the OS's data graph (`mouli_agentic::graph`) up to date from what happens on the desktop.
//!
//! Conversations, messages, models, files, tool calls and approvals become nodes and timed edges,
//! saved as an append-only log `graph.log` on MOULI-DATA. Without a data disk the graph lives in
//! memory only. A log that cannot be read is **never overwritten**: the graph then stays in memory
//! and the person is told why.
//!
//! What is stored is the person's own data on their own disk: message text (cut to 2,000
//! characters), the commands the model ran and a short result. `/graph.forget` erases it.

use mouli_agentic::agentmem::access::{
    AccessRequest, AccessRule, Effect, MemoryAccessPolicy, MemoryAction,
};
use mouli_agentic::agentmem::provenance::Provenance;
use mouli_agentic::graph::{Graph, NodeId};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The log file name inside the data directory.
pub const LOG_NAME: &str = "graph.log";
/// Longest text kept for a message, a command or a result.
const KEEP: usize = 2000;
/// Lines `/graph` shows.
const SHOW: usize = 12;

/// The graph, where it is saved and where the conversation stands.
pub struct Recorder {
    graph: Graph,
    path: Option<PathBuf>,
    /// Why the log is not being saved, when that is so.
    pub warning: Option<String>,
    conversation: String,
    conv_node: Option<NodeId>,
    last_message: Option<NodeId>,
    last_tool: Option<NodeId>,
    messages: u64,
    tools: u64,
    started: u64,
    /// Who is typing: the source recorded for what they write.
    actor: String,
}

fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

impl Recorder {
    /// A graph that is not saved.
    #[must_use]
    pub fn in_memory(now: u64) -> Self {
        Self {
            graph: Graph::new(),
            path: None,
            warning: None,
            conversation: format!("c{now}"),
            conv_node: None,
            last_message: None,
            last_tool: None,
            messages: 0,
            tools: 0,
            started: now,
            actor: "unknown".to_string(),
        }
    }

    /// Opens the graph saved in `dir`, or starts a new one there.
    ///
    /// A log that cannot be read leaves the graph in memory and the file untouched, with
    /// [`Recorder::warning`] set.
    #[must_use]
    pub fn open(dir: &Path, now: u64) -> Self {
        let path = dir.join(LOG_NAME);
        let mut r = Self::in_memory(now);
        match std::fs::read_to_string(&path) {
            Ok(text) => match Graph::replay(&text) {
                Ok(g) => {
                    r.graph = g;
                    r.path = Some(path);
                }
                Err(e) => {
                    r.warning = Some(format!(
                        "The activity record on the data disk is damaged ({e}); it was left as it is and this session is not saved."
                    ));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => r.path = Some(path),
            Err(e) => {
                r.warning = Some(format!(
                    "The activity record cannot be read ({e}); this session is not saved."
                ))
            }
        }
        r
    }

    /// The graph, for queries.
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    fn save(&mut self) {
        let lines = self.graph.drain_log();
        if lines.is_empty() {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };
        let mut text = lines.join("\n");
        text.push('\n');
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.flush()));
        if let Err(e) = result {
            self.warning = Some(format!("The activity record could not be saved ({e})."));
            self.path = None;
        }
    }

    fn conversation_node(&mut self, now: u64) -> Option<NodeId> {
        if self.conv_node.is_none() {
            let id = self
                .graph
                .node("conversation", &self.conversation.clone(), now)
                .ok()?;
            let _ = self.graph.set(id, "started", &self.started.to_string());
            if let Ok(owner) = self.graph.node("person", "owner", now) {
                let _ = self.graph.link(owner, id, "started", now, &[]);
            }
            self.conv_node = Some(id);
        }
        self.conv_node
    }

    /// A new conversation begins (the chat was cleared).
    pub fn new_conversation(&mut self, now: u64) {
        self.conversation = format!("c{now}");
        self.conv_node = None;
        self.last_message = None;
        self.last_tool = None;
        self.messages = 0;
        self.tools = 0;
        self.started = now;
    }

    /// The person wrote `text` in the chat. Returns the message node.
    pub fn user_message(&mut self, text: &str, now: u64) -> Option<NodeId> {
        let actor = self.actor.clone();
        let id = self.message("user", text, ("user", &actor), now)?;
        self.save();
        Some(id)
    }

    /// The model answered `text`. `model` is the loaded model's name.
    pub fn assistant_message(&mut self, text: &str, model: &str, now: u64) -> Option<NodeId> {
        let id = self.message("assistant", text, ("model", model), now)?;
        if !model.is_empty() {
            if let Ok(m) = self.graph.node("model", model, now) {
                let _ = self.graph.link(id, m, "generated_by", now, &[]);
            }
        }
        self.save();
        Some(id)
    }

    /// Who is typing, so what they write is recorded under their name.
    pub fn set_actor(&mut self, user: &str) {
        self.actor = short(user.trim(), 64);
        if self.actor.is_empty() {
            self.actor = "unknown".to_string();
        }
    }

    fn message(
        &mut self,
        role: &str,
        text: &str,
        source: (&str, &str),
        now: u64,
    ) -> Option<NodeId> {
        let conv = self.conversation_node(now)?;
        self.messages += 1;
        let key = format!("{}/{}", self.conversation, self.messages);
        let id = self.graph.node("message", &key, now).ok()?;
        let _ = self.graph.set(id, "role", role);
        let _ = self.graph.set(id, "text", &short(text, KEEP));
        // Where it came from, as the agent-memory provenance record describes it.
        let p = provenance_for(source.0, source.1, now);
        let _ = self.graph.set(id, "source_kind", &p.source_kind);
        let _ = self
            .graph
            .set(id, "source_id", &short(&p.source_id.to_string(), 64));
        let _ = self.graph.set(id, "source_name", &short(source.1, 64));
        let _ = self
            .graph
            .set(id, "authority_rank", &p.authority_rank.to_string());
        let _ = self
            .graph
            .set(id, "confidence_milli", &p.confidence_milli.to_string());
        let _ = self.graph.link(conv, id, "said", now, &[]);
        if let Some(prev) = self.last_message {
            let _ = self.graph.link(prev, id, "next", now, &[]);
        }
        self.last_message = Some(id);
        Some(id)
    }

    /// A tool was run: by the model (`by` = `agent:chat`) or by the person typing a command.
    /// `command` is the slash command, `output` what came back. Files named by `path=` become
    /// file nodes. Returns the tool-call node.
    pub fn tool_call(&mut self, by: &str, command: &str, output: &str, now: u64) -> Option<NodeId> {
        self.tools += 1;
        let key = format!("{}/{}", self.conversation, self.tools);
        let call = self.graph.node("toolcall", &key, now).ok()?;
        let name = command
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        let _ = self.graph.set(call, "tool", name);
        let _ = self.graph.set(call, "command", &short(command, KEEP));
        let _ = self.graph.set(call, "result", &short(output, 500));
        if let Some(from) = self.last_message {
            let _ = self.graph.link(from, call, "called", now, &[]);
        }
        let kind = if by.starts_with("agent") {
            "agent"
        } else {
            "person"
        };
        if let Ok(who) = self.graph.node(kind, by, now) {
            let _ = self.graph.link(call, who, "run_by", now, &[]);
        }
        if let Some(path) = command
            .split_whitespace()
            .find_map(|w| w.strip_prefix("path="))
        {
            let how = name.strip_prefix("code.").unwrap_or(name);
            if let Ok(f) = self.graph.node("file", path, now) {
                let _ = self.graph.link(call, f, "touched", now, &[("how", how)]);
            }
        }
        self.last_tool = Some(call);
        self.save();
        Some(call)
    }

    /// The last tool call asked for approval `id` of `capability`.
    pub fn approval_held(&mut self, id: &str, capability: &str, now: u64) {
        if let Ok(a) = self.graph.node("approval", id, now) {
            let _ = self.graph.set(a, "capability", capability);
            let _ = self.graph.set(a, "status", "waiting");
            if let Some(call) = self.last_tool {
                let _ = self.graph.link(call, a, "held_for", now, &[]);
            }
        }
        self.save();
    }

    /// The person approved or refused approval `id`.
    pub fn approval_decided(&mut self, id: &str, approved: bool, by: &str, now: u64) {
        if let Ok(a) = self.graph.node("approval", id, now) {
            let verdict = if approved { "approved" } else { "refused" };
            let _ = self.graph.set(a, "status", verdict);
            if let Ok(p) = self.graph.node("person", by, now) {
                let _ = self
                    .graph
                    .link(a, p, "decided_by", now, &[("verdict", verdict)]);
            }
        }
        self.save();
    }

    /// A model file was removed from the disk by the person.
    pub fn model_removed(&mut self, name: &str, now: u64) {
        if let Ok(m) = self.graph.node("model", name, now) {
            let _ = self.graph.set(m, "status", "removed");
            if let Ok(p) = self.graph.node("person", "owner", now) {
                let _ = self.graph.link(p, m, "removed", now, &[]);
            }
        }
        self.save();
    }

    /// Erases the saved log and the in-memory graph. Returns a message for the person.
    pub fn forget(&mut self, now: u64) -> String {
        let removed = match &self.path {
            Some(p) => match std::fs::remove_file(p) {
                Ok(()) => "The activity record was erased from the data disk.".to_string(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    "There was nothing saved to erase.".to_string()
                }
                Err(e) => return format!("The activity record could not be erased: {e}"),
            },
            None => "The activity record was cleared from memory (nothing was saved).".to_string(),
        };
        self.graph = Graph::new();
        self.new_conversation(now);
        removed
    }

    /// The recent activity as lines of text, newest first, for `/graph`.
    #[must_use]
    pub fn describe_recent(&self) -> String {
        let edges = self.graph.recent(SHOW * 3);
        if edges.is_empty() {
            return "Nothing recorded yet.".into();
        }
        let mut lines = Vec::new();
        for e in edges {
            // Show what happened to files, tools and approvals, not the bookkeeping edges.
            if matches!(e.kind.as_str(), "next" | "said" | "started" | "run_by") {
                continue;
            }
            let (Some(a), Some(b)) = (self.graph.get(e.from), self.graph.get(e.to)) else {
                continue;
            };
            let extra = e
                .props
                .iter()
                .map(|(k, v)| format!(" {k}={v}"))
                .collect::<String>();
            lines.push(format!(
                "{} {}:{} -{}-> {}:{}{extra}",
                e.time,
                a.kind,
                short(&a.key, 40),
                e.kind,
                b.kind,
                short(&b.key, 40)
            ));
            if lines.len() >= SHOW {
                break;
            }
        }
        if lines.is_empty() {
            "Only conversation bookkeeping so far.".into()
        } else {
            lines.join("\n")
        }
    }

    /// What is known about one file, for `/graph.file path`.
    #[must_use]
    pub fn describe_file(&self, path: &str) -> String {
        let Some(f) = self.graph.find("file", path) else {
            return format!("Nothing recorded about {}.", short(path, 80));
        };
        let mut lines = vec![format!("{} first seen at {}", short(&f.key, 80), f.created)];
        for e in self.graph.history(f.id) {
            if let Some(call) = self.graph.get(e.from).filter(|n| n.kind == "toolcall") {
                let who = self
                    .graph
                    .out_edges(call.id, Some("run_by"))
                    .first()
                    .and_then(|x| self.graph.get(x.to))
                    .map_or("?", |n| n.key.as_str());
                let how = e.props.get("how").map_or("", String::as_str);
                lines.push(format!("{} {how} by {who}", e.time));
            }
        }
        lines.join("\n")
    }
}

// ------------------------------------------------------------------ provenance and access

/// The tenant the device's own record belongs to (a device has one).
pub const TENANT: u64 = 0;
/// Permission to read the activity record (`/graph`, `/graph.file`).
pub const PERM_READ: &str = "memory.read";
/// Permission to erase the activity record (`/graph.forget`).
pub const PERM_FORGET: &str = "memory.forget";

/// How much a kind of source is trusted, as the agent-memory provenance record expresses it:
/// what the person typed outranks a tool's output, which outranks what a model said. These are
/// the device's own choices, not measurements; they are stored with each message so a reader can
/// weigh them, and nothing here decides which source is true.
#[must_use]
pub fn provenance_for(kind: &str, id: &str, now: u64) -> Provenance {
    let (authority_rank, confidence_milli) = match kind {
        "user" => (100, 1000),
        "tool" => (50, 900),
        "model" => (10, 500),
        _ => (0, 0),
    };
    Provenance {
        source_id: u128::from(principal_id(id)),
        source_kind: kind.to_string(),
        observed_at_ns: now.saturating_mul(1_000_000_000),
        confidence_milli,
        authority_rank,
    }
}

/// A stable number for a user name (FNV-1a), the form the access policy asks for.
#[must_use]
pub fn principal_id(user: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for b in user.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The access policy for whoever is signed in, built from the permissions their role holds
/// (`may` answers "does the role hold this permission?"). A role with neither permission gets a
/// policy with no rules, which denies everything by default.
#[must_use]
pub fn policy_for(may: &dyn Fn(&str) -> bool) -> MemoryAccessPolicy {
    let mut policy = MemoryAccessPolicy::new(8);
    let mut allow = |action| {
        let _ = policy.add_rule(AccessRule {
            tenant_id: TENANT,
            principal_id: None,
            memory_id: None,
            action,
            effect: Effect::Allow,
            priority: 0,
            expires_at_ns: None,
        });
    };
    if may(PERM_READ) {
        allow(MemoryAction::Read);
        allow(MemoryAction::Search);
    }
    if may(PERM_FORGET) {
        allow(MemoryAction::Delete);
    }
    policy
}

/// Whether `user` may do `action` to the activity record under `policy`.
#[must_use]
pub fn allowed(policy: &MemoryAccessPolicy, user: &str, action: MemoryAction) -> bool {
    policy
        .authorize(AccessRequest {
            tenant_id: TENANT,
            principal_id: principal_id(user),
            memory_id: 0,
            action,
            now_ns: 0,
        })
        .allowed
}

/// The outcome of a `/graph` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// What to show the person.
    pub text: String,
    /// Set when the command was refused: (action, subject) for the audit trail.
    pub refused: Option<(String, String)>,
}

/// Carries out `/graph<arg>` for `user` under `policy`: reading needs the read permission and
/// erasing the forget permission. A refusal changes nothing and is reported for the audit trail.
pub fn command(
    graph: &mut Recorder,
    policy: &MemoryAccessPolicy,
    user: &str,
    arg: &str,
    now: u64,
) -> Reply {
    let arg = arg.trim();
    let (action, permission, what) = if arg == ".forget" {
        (MemoryAction::Delete, PERM_FORGET, "erase")
    } else {
        (MemoryAction::Read, PERM_READ, "read")
    };
    if !allowed(policy, user, action) {
        return Reply {
            text: format!(
                "{user} is not allowed to {what} the activity record (it needs the {permission} permission)."
            ),
            refused: Some((format!("graph.{what}"), format!("{user} needs {permission}"))),
        };
    }
    let text = if let Some(path) = arg.strip_prefix(".file") {
        graph.describe_file(path.trim())
    } else if arg == ".forget" {
        graph.forget(now)
    } else if arg.is_empty() {
        graph.describe_recent()
    } else {
        "Use /graph (recent activity), /graph.file PATH (history of a file) or /graph.forget (erase the record).".to_string()
    };
    Reply {
        text,
        refused: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-graph-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_conversation_becomes_linked_messages_and_a_model() {
        let mut r = Recorder::in_memory(100);
        let u = r.user_message("hello", 101).unwrap();
        let a = r.assistant_message("hi there", "tiny", 102).unwrap();
        let g = r.graph();
        let conv = g.find("conversation", "c100").unwrap().id;
        assert_eq!(g.out_edges(conv, Some("said")).len(), 2);
        assert_eq!(
            g.out_edges(u, Some("next"))[0].to,
            a,
            "messages are chained"
        );
        let model = g.find("model", "tiny").unwrap().id;
        assert_eq!(g.out_edges(a, Some("generated_by"))[0].to, model);
        assert_eq!(g.get(u).unwrap().props["text"], "hello");
        assert_eq!(g.get(u).unwrap().props["role"], "user");
        assert!(g.find("person", "owner").is_some());
    }

    #[test]
    fn a_tool_call_links_the_message_the_agent_and_the_file() {
        let mut r = Recorder::in_memory(0);
        let m = r
            .assistant_message("TOOL: /code.read path=src/lib.rs", "tiny", 1)
            .unwrap();
        let call = r
            .tool_call(
                "agent:chat",
                "/code.read path=src/lib.rs",
                "fn main() {}",
                2,
            )
            .unwrap();
        r.approval_held("apr-1", "code.apply", 3);
        r.approval_decided("apr-1", true, "owner", 4);
        let g = r.graph();
        assert_eq!(g.out_edges(m, Some("called"))[0].to, call);
        let who = g.out_edges(call, Some("run_by"))[0].to;
        assert_eq!(g.get(who).unwrap().kind, "agent");
        let file = g.find("file", "src/lib.rs").unwrap().id;
        assert_eq!(g.out_edges(call, Some("touched"))[0].props["how"], "read");
        let apr = g.find("approval", "apr-1").unwrap();
        assert_eq!(apr.props["status"], "approved");
        assert_eq!(g.out_edges(call, Some("held_for"))[0].to, apr.id);
        assert_eq!(
            g.out_edges(apr.id, Some("decided_by"))[0].props["verdict"],
            "approved"
        );
        // Why does the file have history? Walk backwards: call, message, conversation.
        let kinds: Vec<String> = g
            .causes(file, 6)
            .iter()
            .map(|(id, _)| g.get(*id).unwrap().kind.clone())
            .collect();
        assert!(
            kinds.starts_with(&["toolcall".to_string(), "message".to_string()]),
            "{kinds:?}"
        );
    }

    #[test]
    fn the_person_typing_a_command_is_recorded_as_a_person() {
        let mut r = Recorder::in_memory(0);
        r.tool_call("owner", "/code.files", "src/main.rs", 1)
            .unwrap();
        let g = r.graph();
        let call = g.find("toolcall", "c0/1").unwrap().id;
        let who = g.out_edges(call, Some("run_by"))[0].to;
        assert_eq!(g.get(who).unwrap().kind, "person");
    }

    #[test]
    fn the_record_is_saved_and_comes_back_after_a_restart() {
        let dir = tmp("save");
        {
            let mut r = Recorder::open(&dir, 50);
            assert!(r.warning.is_none());
            r.user_message("remember me", 51);
            r.tool_call("agent:chat", "/code.read path=a.rs", "ok", 52);
        }
        let text = std::fs::read_to_string(dir.join(LOG_NAME)).unwrap();
        assert!(text.contains("N 1 ") && text.lines().all(|l| l.len() < 4000));
        let mut again = Recorder::open(&dir, 99);
        assert!(again.warning.is_none());
        assert_eq!(again.graph().nodes_of("message").len(), 1);
        assert_eq!(
            again
                .graph()
                .get(again.graph().find("message", "c50/1").unwrap().id)
                .unwrap()
                .props["text"],
            "remember me"
        );
        // The new session adds to the same file rather than replacing it.
        again.user_message("second session", 100);
        let n = Recorder::open(&dir, 200).graph().nodes_of("message").len();
        assert_eq!(n, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_record_is_left_alone() {
        let dir = tmp("damaged");
        std::fs::write(dir.join(LOG_NAME), "N 1 5 file a\nthis is not a record\n").unwrap();
        let mut r = Recorder::open(&dir, 1);
        assert!(
            r.warning.as_ref().unwrap().contains("line 2"),
            "{:?}",
            r.warning
        );
        r.user_message("must not overwrite", 2);
        assert_eq!(
            std::fs::read_to_string(dir.join(LOG_NAME)).unwrap(),
            "N 1 5 file a\nthis is not a record\n",
            "the damaged file is untouched"
        );
        assert_eq!(
            r.graph().nodes_of("message").len(),
            1,
            "still works in memory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_removed_model_is_recorded() {
        let mut r = Recorder::in_memory(0);
        r.model_removed("old.gguf", 5);
        let g = r.graph();
        let m = g.find("model", "old.gguf").unwrap();
        assert_eq!(m.props["status"], "removed");
        assert_eq!(g.in_edges(m.id, Some("removed")).len(), 1);
    }

    #[test]
    fn forgetting_erases_the_file_and_the_graph() {
        let dir = tmp("forget");
        let mut r = Recorder::open(&dir, 1);
        r.user_message("secret", 2);
        assert!(dir.join(LOG_NAME).exists());
        let msg = r.forget(3);
        assert!(msg.contains("erased"), "{msg}");
        assert!(!dir.join(LOG_NAME).exists());
        assert_eq!(r.graph().node_count(), 0);
        assert!(
            r.forget(4).contains("nothing saved"),
            "forgetting twice is harmless"
        );
        r.user_message("after", 5);
        assert!(dir.join(LOG_NAME).exists(), "recording continues");
        assert!(!std::fs::read_to_string(dir.join(LOG_NAME))
            .unwrap()
            .contains("secret"));
        let _ = std::fs::remove_dir_all(&dir);
        // In memory only.
        let mut m = Recorder::in_memory(1);
        m.user_message("x", 2);
        assert!(m.forget(3).contains("memory"));
    }

    #[test]
    fn a_new_conversation_starts_a_new_chain() {
        let mut r = Recorder::in_memory(10);
        r.user_message("one", 11);
        r.new_conversation(20);
        let m = r.user_message("two", 21).unwrap();
        let g = r.graph();
        assert!(g.find("conversation", "c10").is_some() && g.find("conversation", "c20").is_some());
        assert_eq!(g.get(m).unwrap().key, "c20/1");
        assert!(
            g.in_edges(m, Some("next")).is_empty(),
            "no link across conversations"
        );
    }

    #[test]
    fn long_and_hostile_text_is_kept_safely() {
        let dir = tmp("hostile");
        let mut r = Recorder::open(&dir, 1);
        let nasty = format!("N 99 1 file x\nE 1 1 1 forged {}", "é".repeat(5000));
        r.user_message(&nasty, 2);
        r.tool_call("agent:chat", "/code.read path=a b\nc", &nasty, 3);
        let back = Recorder::open(&dir, 9);
        assert!(back.warning.is_none(), "{:?}", back.warning);
        assert_eq!(
            back.graph().node_count(),
            r.graph().node_count(),
            "nothing forged"
        );
        let t = &back
            .graph()
            .get(back.graph().find("message", "c1/1").unwrap().id)
            .unwrap()
            .props["text"];
        assert_eq!(t.chars().count(), KEEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_summaries_are_readable() {
        let mut r = Recorder::in_memory(0);
        assert_eq!(r.describe_recent(), "Nothing recorded yet.");
        r.assistant_message("x", "tiny", 1);
        r.tool_call("agent:chat", "/code.edit path=a.rs text=1", "proposed", 2);
        r.tool_call("agent:chat", "/code.read path=a.rs", "ok", 3);
        let recent = r.describe_recent();
        assert!(
            recent.contains("toolcall:c0/2 -touched-> file:a.rs how=read"),
            "{recent}"
        );
        assert!(!recent.contains("-next->"), "bookkeeping hidden");
        let f = r.describe_file("a.rs");
        assert!(
            f.contains("2 edit by agent:chat") && f.contains("3 read by agent:chat"),
            "{f}"
        );
        assert!(r
            .describe_file("nope.rs")
            .starts_with("Nothing recorded about nope.rs"));
    }

    // ---- provenance and access ----

    fn may_all(_: &str) -> bool {
        true
    }

    fn may_none(_: &str) -> bool {
        false
    }

    #[test]
    fn what_is_written_carries_where_it_came_from_and_how_far_it_is_trusted() {
        let mut r = Recorder::in_memory(100);
        r.set_actor("dana");
        let u = r.user_message("is the build green?", 101).unwrap();
        let a = r
            .assistant_message("yes", "qwen2.5-1.5b.gguf", 102)
            .unwrap();
        let g = r.graph();
        let (up, ap) = (&g.get(u).unwrap().props, &g.get(a).unwrap().props);
        assert_eq!(
            (up["source_kind"].as_str(), up["source_name"].as_str()),
            ("user", "dana")
        );
        assert_eq!(
            (ap["source_kind"].as_str(), ap["source_name"].as_str()),
            ("model", "qwen2.5-1.5b.gguf")
        );
        let rank = |p: &std::collections::BTreeMap<String, String>| {
            p["authority_rank"].parse::<u16>().unwrap()
        };
        assert!(
            rank(up) > rank(ap),
            "what the person typed outranks what a model said"
        );
        // Without an actor the source says so, rather than guessing a name.
        let mut anon = Recorder::in_memory(1);
        let n = anon.user_message("hi", 2).unwrap();
        assert_eq!(anon.graph().get(n).unwrap().props["source_name"], "unknown");
        // A blank or hostile name is kept short and never empty.
        anon.set_actor("   ");
        assert_eq!(anon.actor, "unknown");
        anon.set_actor(&"x".repeat(500));
        assert_eq!(anon.actor.chars().count(), 64);
    }

    #[test]
    fn provenance_ranks_the_kinds_of_source_and_unknown_kinds_get_no_authority() {
        let p = |k| provenance_for(k, "n", 5);
        assert!(p("user").authority_rank > p("tool").authority_rank);
        assert!(p("tool").authority_rank > p("model").authority_rank);
        let unknown = p("mystery");
        assert_eq!((unknown.authority_rank, unknown.confidence_milli), (0, 0));
        assert_eq!(p("user").observed_at_ns, 5_000_000_000);
        assert_eq!(
            provenance_for("user", "dana", 1).source_id,
            provenance_for("model", "dana", 9).source_id,
            "same name, same id"
        );
        assert_ne!(principal_id("dana"), principal_id("dan"));
    }

    #[test]
    fn a_role_with_the_permissions_can_read_and_erase_and_one_without_cannot_do_either() {
        let owner = policy_for(&may_all);
        let nobody = policy_for(&may_none);
        assert_eq!(owner.rule_count(), 3);
        assert_eq!(nobody.rule_count(), 0);
        assert!(allowed(&owner, "dana", MemoryAction::Read));
        assert!(allowed(&owner, "dana", MemoryAction::Delete));
        assert!(
            !allowed(&nobody, "dana", MemoryAction::Read),
            "no rule means denied"
        );
        assert!(!allowed(&nobody, "dana", MemoryAction::Delete));
        // Read and erase are separate permissions.
        let read_only = policy_for(&|p| p == PERM_READ);
        assert!(allowed(&read_only, "x", MemoryAction::Search));
        assert!(!allowed(&read_only, "x", MemoryAction::Delete));
        let forget_only = policy_for(&|p| p == PERM_FORGET);
        assert!(!allowed(&forget_only, "x", MemoryAction::Read));
        assert!(allowed(&forget_only, "x", MemoryAction::Delete));
    }

    #[test]
    fn graph_commands_are_refused_without_the_permission_and_change_nothing() {
        let mut r = Recorder::in_memory(1);
        r.set_actor("sam");
        r.user_message("a private question", 2);
        let nobody = policy_for(&may_none);
        let read = command(&mut r, &nobody, "sam", "", 3);
        assert!(
            read.text.contains("not allowed to read") && read.text.contains(PERM_READ),
            "{}",
            read.text
        );
        assert_eq!(
            read.refused,
            Some(("graph.read".into(), format!("sam needs {PERM_READ}")))
        );
        assert!(
            !read.text.contains("private question"),
            "a refusal must not leak the content"
        );
        let file = command(&mut r, &nobody, "sam", ".file src/a.rs", 3);
        assert!(file.refused.is_some());
        let erase = command(&mut r, &nobody, "sam", ".forget", 4);
        assert_eq!(erase.refused.map(|x| x.0), Some("graph.erase".to_string()));
        assert_eq!(r.graph().nodes_of("message").len(), 1, "nothing was erased");
    }

    #[test]
    fn graph_commands_work_for_a_role_that_holds_the_permissions() {
        let mut r = Recorder::in_memory(1);
        r.set_actor("dana");
        r.user_message("remember the tax rounding", 2);
        let owner = policy_for(&may_all);
        let shown = command(&mut r, &owner, "dana", "", 3);
        assert!(shown.refused.is_none());
        assert_eq!(
            shown.text,
            r.describe_recent(),
            "the permitted command shows the record"
        );
        assert!(command(&mut r, &owner, "dana", "oops", 3)
            .text
            .starts_with("Use /graph"));
        let erased = command(&mut r, &owner, "dana", ".forget", 4);
        assert!(erased.refused.is_none());
        assert_eq!(
            r.graph().nodes_of("message").len(),
            0,
            "the record was erased"
        );
        // Read-only: can look, cannot erase.
        r.user_message("another note", 5);
        let read_only = policy_for(&|p| p == PERM_READ);
        assert!(command(&mut r, &read_only, "lee", "", 6).refused.is_none());
        assert!(command(&mut r, &read_only, "lee", ".forget", 7)
            .refused
            .is_some());
        assert_eq!(r.graph().nodes_of("message").len(), 1);
    }
}
