use mouli_code::diff::render;
use mouli_code::make::make_patch;
use mouli_device::{Backend, Device, Outcome};
use mouli_interface::routing::{ModelEntry, Route};
use mouli_interface::{
    Application, Call, CapabilitySpec, Error, Output, Permissions, Policy, Risk, Workspace,
};
use mouli_screen::key::Key;
use mouli_screen::screen::{Action, Tab};
use mouli_screen::screens::{Chain, PatchState, Role};
use std::collections::BTreeSet;

struct Echo(Vec<String>);
impl Backend for Echo {
    fn complete(&mut self, route: &Route, prompt: &str) -> Result<String, String> {
        self.0.push(prompt.to_string());
        if prompt.contains("explode") {
            return Err("out of memory".into());
        }
        Ok(format!("[{}] you said: {prompt}", route.model))
    }
}

struct Notes;
impl Application for Notes {
    fn name(&self) -> &str {
        "note"
    }
    fn capabilities(&self) -> Vec<CapabilitySpec> {
        vec![
            CapabilitySpec::new("note.read", "read a note", Risk::Read),
            CapabilitySpec::new("note.send", "send a note", Risk::Consequential).requires(&["to"]),
        ]
    }
    fn invoke(&mut self, call: &Call) -> Result<Output, Error> {
        Ok(Output {
            text: format!("ran {} {:?}", call.capability, call.args.get("to")),
            artifacts: vec![],
        })
    }
}

fn model() -> ModelEntry {
    ModelEntry {
        id: "local-small".into(),
        provider: "local".into(),
        local: true,
        caps: BTreeSet::from(["text".to_string()]),
        context: 4096,
        cost_per_1k: 0,
        quality: 50,
    }
}

fn workspace() -> Workspace {
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*", "approve"]));
    ws.add_role(Permissions::new("clerk", &["note.read", "note.send"]));
    ws.register_app(Box::new(Notes)).unwrap();
    ws.add_model(model());
    ws
}

fn type_line(d: &mut Device<Echo>, s: &str) {
    for c in s.chars() {
        d.on_key(Key::Char(c));
    }
    d.on_key(Key::Enter);
}

fn chat_texts(d: &Device<Echo>) -> Vec<(Role, String)> {
    d.shell
        .chat
        .messages
        .iter()
        .map(|m| (m.role, m.text.clone()))
        .collect()
}

#[test]
fn a_chat_message_is_routed_answered_and_audited() {
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "hello there");
    let msgs = chat_texts(&d);
    assert_eq!(msgs[0], (Role::User, "hello there".into()));
    assert_eq!(
        msgs[1],
        (
            Role::Assistant,
            "[local-small] you said: hello there".into()
        )
    );
    let rows = &d.shell.audit.rows;
    assert!(rows
        .iter()
        .any(|r| r.action == "chat" && r.outcome.starts_with("ok via local-small")));
    assert_eq!(d.shell.audit.chain, Chain::Verified);
}

#[test]
fn a_model_failure_is_reported_and_audited_not_hidden() {
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "please explode");
    let msgs = chat_texts(&d);
    assert!(msgs.last().unwrap().1.contains("out of memory"));
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.outcome.contains("failed: out of memory")));
    assert!(d.shell.security.findings.iter().any(|f| f.id == "failures"));
}

#[test]
fn a_read_capability_runs_at_once() {
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "/note.read");
    assert_eq!(chat_texts(&d).last().unwrap().0, Role::Tool);
}

#[test]
fn nobody_approves_their_own_request_but_a_second_person_can() {
    let mut ws = workspace();
    let alice = ws.open_session("acme", "alice", "clerk").unwrap();
    let held = ws.submit("acme", &alice, "/note.send to=bob").unwrap();
    assert!(matches!(
        held,
        mouli_interface::Reply::AwaitingApproval { .. }
    ));

    let mut d = Device::new(ws, Echo(vec![]), "acme", "owner", "owner").unwrap();
    // The owner sees alice's request and may decide it.
    assert_eq!(d.shell.approvals.rows.len(), 1);
    assert!(d.shell.approvals.rows[0].can_decide);
    assert_eq!(d.shell.approvals.rows[0].requester, "alice");

    // The owner's own request is held and cannot be decided by the owner.
    d.on_key(Key::F(1));
    type_line(&mut d, "/note.send to=carol");
    assert_eq!(d.shell.approvals.rows.len(), 2);
    let own = d
        .shell
        .approvals
        .rows
        .iter()
        .position(|r| r.requester == "owner")
        .unwrap();
    assert!(!d.shell.approvals.rows[own].can_decide);
    d.on_key(Key::F(3));
    // Select the owner's row and try to approve: nothing runs.
    d.shell.approvals.cursor.selected = own;
    d.on_key(Key::Char('a'));
    assert!(d.shell.approvals.message.contains("yourself"));
    assert_eq!(d.shell.approvals.rows.len(), 2);

    // Approve alice's: it runs once, leaves the queue, and is audited.
    let alices = d
        .shell
        .approvals
        .rows
        .iter()
        .position(|r| r.requester == "alice")
        .unwrap();
    d.shell.approvals.cursor.selected = alices;
    d.on_key(Key::Char('a'));
    assert_eq!(d.shell.approvals.rows.len(), 1);
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "approve" && r.outcome == "approved"));
    assert!(chat_texts(&d)
        .iter()
        .any(|(r, t)| *r == Role::Tool && t.contains("note.send")));
}

#[test]
fn a_role_without_the_approve_permission_cannot_decide() {
    let mut ws = workspace();
    ws.add_role(Permissions::new("viewer", &["note.read"]));
    let alice = ws.open_session("acme", "alice", "clerk").unwrap();
    ws.submit("acme", &alice, "/note.send to=bob").unwrap();
    let d = Device::new(ws, Echo(vec![]), "acme", "vic", "viewer").unwrap();
    let row = &d.shell.approvals.rows[0];
    assert!(!row.can_decide);
    assert!(row.why_not.contains("viewer"));
}

#[test]
fn refused_requests_raise_the_security_view() {
    let mut ws = workspace();
    ws.add_role(Permissions::new("guest", &[]));
    let mut d = Device::new(ws, Echo(vec![]), "acme", "gus", "guest").unwrap();
    assert_eq!(d.shell.security.score, 0);
    d.on_key(Key::F(1));
    type_line(&mut d, "/note.send to=x");
    assert!(chat_texts(&d)
        .last()
        .unwrap()
        .1
        .contains("lacks permission"));
    assert!(
        d.shell.security.score >= 20,
        "score {}",
        d.shell.security.score
    );
    assert!(d.shell.security.findings.iter().any(|f| f.id == "denied"));
}

#[test]
fn unknown_role_and_host_actions() {
    assert!(Device::new(workspace(), Echo(vec![]), "acme", "x", "nope").is_err());
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.shell.models.rows.push(mouli_screen::screens::ModelRow {
        name: "m.gguf".into(),
        size_mb: 10,
        arch: None,
        needs_mb: 20,
        fits: true,
    });
    d.on_key(Key::F(2));
    assert_eq!(d.on_key(Key::Enter), Outcome::Host(Action::ChooseModel(0)));
    assert_eq!(d.on_key(Key::F(1)), Outcome::Handled);
    assert_eq!(d.shell.active, Tab::Chat);
}

#[test]
fn a_new_conversation_clears_the_screen_but_not_the_audit() {
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "one");
    let before = d.shell.audit.rows.len();
    d.on_key(Key::Ctrl('l'));
    assert!(d.shell.chat.messages.is_empty());
    assert!(d.shell.audit.rows.len() > before);
}

fn project() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let old = "fn tax(base: i64, rate: i64) -> i64 {\n    base * rate / 100\n}\n";
    let new = "fn tax(base: i64, rate: i64) -> i64 {\n    (base * rate + 50) / 100\n}\n";
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/tax.rs"), old).unwrap();
    let patch = make_patch("src/tax.rs", Some(old), Some(new), 3)
        .unwrap()
        .unwrap();
    (dir, render(&[patch]), new.to_string())
}

#[test]
fn a_patch_is_reviewed_applied_audited_and_undone_exactly() {
    let (dir, diff, new) = project();
    let old = std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.propose_patch(dir.path(), &diff).unwrap();
    d.on_key(Key::F(6));
    assert_eq!(d.shell.review.state, PatchState::Proposed);
    assert_eq!(d.shell.review.files.len(), 1);
    assert_eq!(
        (
            d.shell.review.files[0].added,
            d.shell.review.files[0].removed
        ),
        (1, 1)
    );
    assert!(d.shell.review.all_checks_pass());
    // Proposing changes nothing on disk.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap(),
        old
    );

    d.on_key(Key::Char('a'));
    assert_eq!(d.shell.review.state, PatchState::Applied);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap(),
        new
    );
    // Applying twice does nothing more.
    d.on_key(Key::Char('a'));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap(),
        new
    );

    d.on_key(Key::Char('u'));
    assert_eq!(d.shell.review.state, PatchState::RolledBack);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap(),
        old
    );

    let actions: Vec<_> = d
        .shell
        .audit
        .rows
        .iter()
        .map(|r| r.action.as_str())
        .collect();
    for a in ["patch.propose", "patch.apply", "patch.rollback"] {
        assert!(actions.contains(&a), "{a} missing from {actions:?}");
    }
    assert_eq!(d.shell.audit.chain, Chain::Verified);
}

#[test]
fn undo_refuses_when_the_file_was_edited_after_applying() {
    let (dir, diff, new) = project();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.propose_patch(dir.path(), &diff).unwrap();
    d.on_key(Key::F(6));
    d.on_key(Key::Char('a'));
    let edited = format!("{new}// my own edit\n");
    std::fs::write(dir.path().join("src/tax.rs"), &edited).unwrap();
    d.on_key(Key::Char('u'));
    assert_eq!(d.shell.review.state, PatchState::Applied, "still applied");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap(),
        edited
    );
    assert!(d.shell.review.message.contains("edited"));
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "patch.rollback" && r.outcome.starts_with("failed")));
}

#[test]
fn a_rejected_patch_is_dropped_and_cannot_be_applied() {
    let (dir, diff, _new) = project();
    let old = std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.propose_patch(dir.path(), &diff).unwrap();
    d.on_key(Key::F(6));
    d.on_key(Key::Char('x'));
    assert_eq!(d.shell.review.state, PatchState::Rejected);
    d.on_key(Key::Char('a'));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap(),
        old
    );
}

#[test]
fn a_patch_that_fails_policy_is_refused_by_the_screen_and_by_the_controller() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    let old = "fn t() {\n    assert_eq!(1, 1);\n}\n";
    std::fs::write(dir.path().join("tests/a.rs"), old).unwrap();
    let p = make_patch("tests/a.rs", Some(old), Some("fn t() {}\n"), 3)
        .unwrap()
        .unwrap();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.propose_patch(dir.path(), &render(&[p])).unwrap();
    assert!(!d.shell.review.all_checks_pass());
    d.on_key(Key::F(6));
    // Through the screen: the screen refuses.
    d.on_key(Key::Char('a'));
    let on_disk = || std::fs::read_to_string(dir.path().join("tests/a.rs")).unwrap();
    assert_eq!(on_disk(), old);
    // Straight to the controller: it refuses on its own, and says so in the audit chain.
    d.run(Action::ApplyPatch);
    assert_eq!(on_disk(), old);
    assert_eq!(d.shell.review.state, PatchState::Proposed);
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "patch.apply" && r.outcome.starts_with("denied")));
    // Rollback with nothing applied is harmless.
    d.run(Action::RollbackPatch);
    assert_eq!(d.shell.review.state, PatchState::Proposed);
}

#[test]
fn bad_diffs_and_misfits_are_reported_without_touching_anything() {
    let (dir, diff, _new) = project();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    assert!(d.propose_patch(dir.path(), "this is not a diff").is_err());
    std::fs::write(dir.path().join("src/tax.rs"), "completely different\n").unwrap();
    assert!(d.propose_patch(dir.path(), &diff).is_err());
    assert!(d.shell.review.files.is_empty());
    let escape = "--- a/../../etc/passwd\n+++ b/../../etc/passwd\n@@ -1 +1 @@\n-a\n+b\n";
    assert!(d.propose_patch(dir.path(), escape).is_err());
}
