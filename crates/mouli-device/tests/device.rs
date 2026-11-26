//! End-to-end tests of the device controller: chat, approvals, audit, security and patch review.
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

// ---------------------------------------------------------------- applications

#[cfg(feature = "office")]
fn app_workspace() -> (Workspace, mouli_apps::Apps) {
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_role(Permissions::new("clerk", &["sheet.*", "studio.*"]));
    ws.add_role(Permissions::new(
        "viewer",
        &["sheet.get", "sheet.show", "sheet.list"],
    ));
    ws.add_model(model());
    let apps = mouli_apps::register(&mut ws).unwrap();
    (ws, apps)
}

#[cfg(feature = "office")]
fn device_with_apps(user: &str, role: &str) -> (Device<Echo>, mouli_apps::Apps) {
    let (ws, apps) = app_workspace();
    let mut d = Device::new(ws, Echo(vec![]), "acme", user, role).unwrap();
    d.attach_apps(apps.clone());
    (d, apps)
}

fn type_keys(d: &mut Device<Echo>, s: &str) {
    for c in s.chars() {
        d.on_key(Key::Char(c));
    }
}

#[test]
#[cfg(feature = "office")]
fn editing_a_cell_goes_through_the_gate_and_the_screen_shows_the_result() {
    let (mut d, apps) = device_with_apps("owner", "owner");
    d.on_key(Key::F(7)); // Sheet
    type_keys(&mut d, "10");
    d.on_key(Key::Enter);
    d.on_key(Key::Down);
    type_keys(&mut d, "32");
    d.on_key(Key::Enter);
    d.on_key(Key::Down);
    type_keys(&mut d, "=A1+A2");
    d.on_key(Key::Enter);
    assert_eq!(apps.book.borrow().get_raw(0, 2, 0), Some("=A1+A2"));
    let cells = &d.shell.sheet.cells;
    assert_eq!(cells[&(0, 0)].shown, "10");
    assert_eq!(cells[&(2, 0)].shown, "42", "the formula was calculated");
    assert_eq!(cells[&(2, 0)].raw, "=A1+A2");
    let sets = d
        .shell
        .audit
        .rows
        .iter()
        .filter(|r| r.action == "sheet.set" && r.outcome == "ok")
        .count();
    assert_eq!(sets, 3, "each edit is in the audit chain");
    assert_eq!(d.shell.audit.chain, Chain::Verified);
    // Changing an input recalculates the formula on screen.
    d.on_key(Key::Up);
    d.on_key(Key::Up);
    type_keys(&mut d, "20");
    d.on_key(Key::Enter);
    assert_eq!(d.shell.sheet.cells[&(2, 0)].shown, "52");
}

#[test]
#[cfg(feature = "office")]
fn a_value_with_quotes_and_spaces_is_stored_exactly_because_the_call_is_structured() {
    let (mut d, apps) = device_with_apps("owner", "owner");
    d.on_key(Key::F(7));
    type_keys(&mut d, "he said \"hi\" = x");
    d.on_key(Key::Enter);
    assert_eq!(
        apps.book.borrow().get_raw(0, 0, 0),
        Some("he said \"hi\" = x")
    );
}

#[test]
#[cfg(feature = "office")]
fn clearing_a_range_needs_a_second_person_and_then_updates_the_screen() {
    let (mut ws, apps) = app_workspace();
    // Alice (a clerk) fills two cells and asks to clear them: the clear is held.
    let alice = ws.open_session("acme", "alice", "clerk").unwrap();
    ws.submit("acme", &alice, "/sheet.set cell=A1 value=1")
        .unwrap();
    ws.submit("acme", &alice, "/sheet.set cell=A2 value=2")
        .unwrap();
    let held = ws
        .submit("acme", &alice, "/sheet.clear range=A1:A2")
        .unwrap();
    assert!(matches!(
        held,
        mouli_interface::Reply::AwaitingApproval { .. }
    ));
    assert_eq!(
        apps.book.borrow().sheets[0].cells.len(),
        2,
        "nothing is erased while held"
    );

    let mut d = Device::new(ws, Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.attach_apps(apps.clone());
    assert_eq!(d.shell.sheet.cells.len(), 2);
    assert_eq!(d.shell.approvals.rows.len(), 1);
    assert_eq!(d.shell.approvals.rows[0].capability, "sheet.clear");
    d.on_key(Key::F(3));
    d.on_key(Key::Char('a'));
    assert!(
        apps.book.borrow().sheets[0].cells.is_empty(),
        "approved: erased"
    );
    assert!(d.shell.sheet.cells.is_empty(), "and the screen follows");
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "approve" && r.outcome == "approved"));
}

#[test]
#[cfg(feature = "office")]
fn a_clear_asked_for_from_the_chat_by_the_owner_waits_for_someone_else() {
    let (mut d, apps) = device_with_apps("owner", "owner");
    d.on_key(Key::F(1));
    type_line(&mut d, "/sheet.set cell=B2 value=5");
    type_line(&mut d, "/sheet.clear range=A1:C3");
    assert_eq!(
        apps.book.borrow().get_raw(0, 1, 1),
        Some("5"),
        "not erased: held"
    );
    assert_eq!(d.shell.approvals.rows.len(), 1);
    assert!(
        !d.shell.approvals.rows[0].can_decide,
        "the owner cannot approve their own request"
    );
}

#[test]
#[cfg(feature = "office")]
fn a_role_without_permission_cannot_edit_and_is_told_why() {
    let (mut d, apps) = device_with_apps("vic", "viewer");
    d.on_key(Key::F(7));
    type_keys(&mut d, "99");
    d.on_key(Key::Enter);
    assert!(apps.book.borrow().sheets[0].cells.is_empty());
    assert!(
        d.shell.sheet.message.contains("lacks permission sheet.set"),
        "{}",
        d.shell.sheet.message
    );
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "sheet.set" && r.outcome.starts_with("denied")));
    assert!(d.shell.security.findings.iter().any(|f| f.id == "denied"));
}

#[test]
#[cfg(feature = "office")]
fn a_bad_formula_is_shown_as_an_error_value_and_a_bad_cell_name_cannot_happen_from_the_screen() {
    let (mut d, _apps) = device_with_apps("owner", "owner");
    d.on_key(Key::F(7));
    type_keys(&mut d, "=1/0");
    d.on_key(Key::Enter);
    assert!(d.shell.sheet.cells[&(0, 0)].shown.starts_with('#'));
    assert!(
        d.shell.sheet.message.is_empty(),
        "an error value is not a failed call"
    );
}

#[test]
#[cfg(feature = "office")]
fn studio_blocks_are_added_from_the_screen_and_shown() {
    let (mut d, apps) = device_with_apps("owner", "owner");
    d.on_key(Key::F(8));
    type_keys(&mut d, "# Plan");
    d.on_key(Key::Enter);
    type_keys(&mut d, "First step.");
    d.on_key(Key::Enter);
    assert_eq!(apps.doc.borrow().blocks.len(), 2);
    assert_eq!(d.shell.studio.blocks.len(), 2);
    assert!(d.shell.studio.blocks[0].heading && d.shell.studio.blocks[0].text == "Plan");
    assert!(!d.shell.studio.blocks[1].heading);
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "studio.heading"));
}

#[test]
#[cfg(feature = "office")]
fn the_applications_are_also_callable_from_the_chat_with_the_same_results() {
    let (mut d, apps) = device_with_apps("owner", "owner");
    d.on_key(Key::F(1));
    type_line(&mut d, "/sheet.set cell=A1 value=7");
    type_line(&mut d, "/sheet.get cell=A1");
    let last = chat_texts(&d)
        .into_iter()
        .rev()
        .find(|(r, _)| *r == Role::Tool)
        .unwrap()
        .1;
    assert!(last.contains("A1 = 7"), "{last}");
    assert_eq!(apps.book.borrow().get_raw(0, 0, 0), Some("7"));
    assert_eq!(
        d.shell.sheet.cells[&(0, 0)].shown,
        "7",
        "a chat call also updates the screen"
    );
}

// ---------------------------------------------------------------- saving

#[cfg(feature = "office")]
fn data_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mouli-device-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
#[cfg(feature = "office")]
fn edits_are_saved_as_they_are_made_and_found_again_after_a_restart() {
    let dir = data_dir("restart");
    {
        let (mut d, _apps) = device_with_apps("owner", "owner");
        d.keep_apps_in(&dir).unwrap();
        d.on_key(Key::F(7));
        type_keys(&mut d, "21");
        d.on_key(Key::Enter);
        d.on_key(Key::Down);
        type_keys(&mut d, "=A1*2");
        d.on_key(Key::Enter);
    }
    // A new device, new workspace, new applications: only the directory carries over.
    let (mut d, apps) = device_with_apps("owner", "owner");
    assert_eq!(apps.book.borrow().get_raw(0, 0, 0), None);
    d.keep_apps_in(&dir).unwrap();
    assert_eq!(apps.book.borrow().get_raw(0, 1, 0), Some("=A1*2"));
    d.on_key(Key::F(7));
    assert_eq!(d.shell.sheet.cells[&(1, 0)].shown, "42");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(feature = "office")]
fn a_refused_edit_is_not_saved() {
    let dir = data_dir("refused");
    let (mut d, _apps) = device_with_apps("viewer", "viewer");
    d.keep_apps_in(&dir).unwrap();
    d.on_key(Key::F(7));
    type_keys(&mut d, "99");
    d.on_key(Key::Enter);
    let text = std::fs::read_to_string(dir.join("sheet.mouli")).unwrap_or_default();
    assert!(!text.contains("99"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(feature = "office")]
fn a_damaged_saved_file_is_reported_and_never_overwritten() {
    let dir = data_dir("damaged");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sheet.mouli"), "not a sheet").unwrap();
    let (mut d, _apps) = device_with_apps("owner", "owner");
    assert!(d.keep_apps_in(&dir).is_err());
    d.on_key(Key::F(7));
    type_keys(&mut d, "5");
    d.on_key(Key::Enter);
    assert_eq!(
        std::fs::read_to_string(dir.join("sheet.mouli")).unwrap(),
        "not a sheet"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(feature = "office")]
fn keeping_apps_without_attaching_them_is_an_error() {
    let (ws, _apps) = app_workspace();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "owner", "owner").unwrap();
    assert!(d.keep_apps_in(&data_dir("none")).is_err());
}

// ---------------------------------------------------------------- code through the gate

fn code_workspace() -> (Workspace, mouli_apps::SharedCode) {
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_role(Permissions::new("dev", &["code.*"]));
    ws.add_role(Permissions::new("viewer", &["code.status"]));
    ws.add_model(model());
    let code = mouli_apps::register_code(&mut ws).unwrap();
    (ws, code)
}

fn read(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("src/tax.rs")).unwrap()
}

#[test]
fn applying_from_the_review_screen_waits_for_a_second_person() {
    let (dir, diff, new) = project();
    let old = read(&dir);
    let (ws, code) = code_workspace();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "dana", "dev").unwrap();
    d.attach_code(code);
    d.propose_patch(dir.path(), &diff).unwrap();
    d.on_key(Key::F(6));
    assert_eq!(d.shell.review.state, PatchState::Proposed);

    d.on_key(Key::Char('a'));
    assert_eq!(read(&dir), old, "held, not written");
    assert_eq!(d.shell.review.state, PatchState::Proposed);
    assert!(
        d.shell.review.message.contains("Held for approval"),
        "{}",
        d.shell.review.message
    );
    assert_eq!(d.shell.approvals.rows.len(), 1);
    assert_eq!(d.shell.approvals.rows[0].capability, "code.apply");
    assert!(
        !d.shell.approvals.rows[0].can_decide,
        "the requester cannot approve their own change"
    );
    let _ = new;
}

#[test]
fn an_owner_approving_a_held_apply_writes_the_files_and_updates_the_screen() {
    let (dir, diff, new) = project();
    let (mut ws, code) = code_workspace();
    code.borrow_mut().open(dir.path()).unwrap();
    code.borrow_mut().propose(&diff).unwrap();
    let dana = ws.open_session("acme", "dana", "dev").unwrap();
    let held = ws
        .call("acme", &dana, "code.apply", Default::default())
        .unwrap();
    assert!(matches!(
        held,
        mouli_interface::Reply::AwaitingApproval { .. }
    ));

    let mut d = Device::new(ws, Echo(vec![]), "acme", "olga", "owner").unwrap();
    d.attach_code(code);
    d.on_key(Key::F(3));
    d.on_key(Key::Char('a'));
    assert_eq!(read(&dir), new);
    d.on_key(Key::F(6));
    assert_eq!(d.shell.review.state, PatchState::Applied);
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "approve" && r.outcome == "approved"));
}

#[test]
fn undoing_is_also_held_and_a_viewer_cannot_even_propose() {
    let (dir, diff, _new) = project();
    let (mut ws, code) = code_workspace();
    let viewer = ws.open_session("acme", "vic", "viewer").unwrap();
    code.borrow_mut().open(dir.path()).unwrap();
    let r = ws.call(
        "acme",
        &viewer,
        "code.propose",
        [("diff".to_string(), diff.clone())].into_iter().collect(),
    );
    assert!(!matches!(r, Ok(mouli_interface::Reply::Done(_))), "{r:?}");
    assert!(code.borrow().view().is_none());

    // The rest as one person who may do everything but still needs a second to approve.
    code.borrow_mut().propose(&diff).unwrap();
    let dana = ws.open_session("acme", "dana", "dev").unwrap();
    ws.call("acme", &dana, "code.apply", Default::default())
        .unwrap();
    // Approve the apply as the owner, straight on the workspace.
    let id = ws.pending_approvals()[0].0.to_string();
    ws.decide("acme", &id, "olga", "owner", true).unwrap();
    assert_eq!(
        code.borrow().view().unwrap().stage,
        mouli_apps::Stage::Applied
    );

    let held = ws
        .call("acme", &dana, "code.rollback", Default::default())
        .unwrap();
    assert!(matches!(
        held,
        mouli_interface::Reply::AwaitingApproval { .. }
    ));
    assert_eq!(
        code.borrow().view().unwrap().stage,
        mouli_apps::Stage::Applied,
        "still applied while the undo is held"
    );
}

#[test]
fn a_call_cannot_point_the_application_at_another_folder() {
    let (mut ws, code) = code_workspace();
    let dana = ws.open_session("acme", "dana", "dev").unwrap();
    // No folder was chosen by the host: a model's call cannot choose one.
    let r = ws.call(
        "acme",
        &dana,
        "code.propose",
        [
            (
                "diff".to_string(),
                "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n".to_string(),
            ),
            ("root".to_string(), "/etc".to_string()),
        ]
        .into_iter()
        .collect(),
    );
    assert!(r.is_err(), "{r:?}");
    assert!(code.borrow().view().is_none());
}

// ---------------------------------------------------------------- the code editor

fn editor_device(dir: &tempfile::TempDir, user: &str, role: &str) -> Device<Echo> {
    let (ws, code) = code_workspace();
    let mut d = Device::new(ws, Echo(vec![]), "acme", user, role).unwrap();
    d.attach_code(code);
    d.open_project(dir.path()).unwrap();
    d
}

#[test]
fn the_editor_lists_the_project_opens_a_file_and_sends_an_edit_for_review() {
    let (dir, _diff, _new) = project();
    let old = read(&dir);
    let mut d = editor_device(&dir, "dana", "dev");
    assert_eq!(d.shell.editor.files, vec!["src/tax.rs"]);
    d.on_key(Key::F(9));
    assert_eq!(d.shell.active, Tab::Editor);
    d.on_key(Key::Enter); // open the only file
    assert_eq!(d.shell.editor.path(), Some("src/tax.rs"));
    assert_eq!(d.shell.editor.text(), old);

    // Edit: put a comment on the first line, then send it.
    type_keys(&mut d, "// tax\n");
    assert!(d.shell.editor.is_dirty());
    d.on_key(Key::Ctrl('s'));
    assert!(!d.shell.editor.is_dirty(), "sent");
    assert_eq!(read(&dir), old, "nothing is written by saving");

    d.on_key(Key::F(6));
    assert_eq!(d.shell.review.state, PatchState::Proposed);
    assert_eq!(d.shell.review.files[0].path, "src/tax.rs");
    assert_eq!(d.shell.review.files[0].added, 1);
    assert!(d.shell.review.all_checks_pass());
}

#[test]
fn an_edit_from_the_editor_is_written_only_after_a_second_person_approves_the_apply() {
    let (dir, _diff, _new) = project();
    let old = read(&dir);
    let (mut ws, code) = code_workspace();
    code.borrow_mut().open(dir.path()).unwrap();
    // Dana edits and asks to apply, as the editor and review screens would.
    let dana = ws.open_session("acme", "dana", "dev").unwrap();
    let edit: std::collections::BTreeMap<String, String> = [
        ("path".to_string(), "src/tax.rs".to_string()),
        ("text".to_string(), format!("// reviewed\n{old}")),
    ]
    .into_iter()
    .collect();
    ws.call("acme", &dana, "code.edit", edit).unwrap();
    let held = ws
        .call("acme", &dana, "code.apply", Default::default())
        .unwrap();
    assert!(matches!(
        held,
        mouli_interface::Reply::AwaitingApproval { .. }
    ));
    assert_eq!(read(&dir), old);
    let id = ws.pending_approvals()[0].0.to_string();
    ws.decide("acme", &id, "olga", "owner", true).unwrap();
    assert_eq!(read(&dir), format!("// reviewed\n{old}"));
}

#[test]
fn the_editor_shows_why_it_cannot_continue() {
    let (dir, _diff, _new) = project();
    // A viewer may not even read files.
    let mut d = editor_device(&dir, "vic", "viewer");
    assert!(d.shell.editor.files.is_empty());
    assert!(
        !d.shell.editor.message.is_empty(),
        "{}",
        d.shell.editor.message
    );

    // A device without the Code application says so instead of failing silently.
    let mut plain = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    plain.on_key(Key::F(9));
    plain.on_key(Key::Ctrl('r'));
    assert!(plain.shell.editor.message.contains("not connected"));
    let _ = &mut d;
}

#[test]
fn a_second_edit_of_another_file_is_refused_while_one_is_under_review() {
    let (dir, _diff, _new) = project();
    std::fs::write(dir.path().join("src/other.rs"), "fn o() {}\n").unwrap();
    let mut d = editor_device(&dir, "dana", "dev");
    d.on_key(Key::F(9));
    d.on_key(Key::Enter); // src/other.rs sorts first
    type_keys(&mut d, "x");
    d.on_key(Key::Ctrl('s'));
    d.on_key(Key::Esc);
    d.on_key(Key::Down);
    d.on_key(Key::Enter); // src/tax.rs
    assert_eq!(d.shell.editor.path(), Some("src/tax.rs"));
    type_keys(&mut d, "y");
    d.on_key(Key::Ctrl('s'));
    assert!(d.shell.editor.is_dirty(), "not sent");
    assert!(
        d.shell.editor.message.contains("under review"),
        "{}",
        d.shell.editor.message
    );
}

#[test]
fn reading_a_file_in_the_editor_does_not_copy_it_into_the_conversation() {
    let (dir, _diff, _new) = project();
    let big = format!("// {}\n", "z".repeat(40)).repeat(1_000);
    std::fs::write(dir.path().join("src/big.rs"), &big).unwrap();
    let mut d = editor_device(&dir, "dana", "dev");
    d.on_key(Key::F(9));
    d.on_key(Key::Enter); // src/big.rs sorts first
    assert_eq!(d.shell.editor.path(), Some("src/big.rs"));
    assert_eq!(d.shell.editor.text(), big);
    // The chat (which is sent to models and saved) holds only short lines.
    let longest = d
        .workspace()
        .session("acme", d.session())
        .unwrap()
        .messages()
        .iter()
        .map(|m| m.content.len())
        .max()
        .unwrap_or(0);
    assert!(longest < 500, "{longest}");
}

// ---------------------------------------------------------------- the player

#[derive(Clone, Default)]
#[cfg(feature = "office")]
struct PlayLog(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

#[cfg(feature = "office")]
impl mouli_player::Backend for PlayLog {
    fn load(&mut self, s: &mouli_player::Source) -> Result<(), String> {
        self.0.borrow_mut().push(format!("load {}", s.label()));
        Ok(())
    }
    fn pause(&mut self, p: bool) {
        self.0.borrow_mut().push(format!("pause {p}"));
    }
    fn seek(&mut self, s: u32) {
        self.0.borrow_mut().push(format!("seek {s}"));
    }
    fn set_volume(&mut self, v: u8) {
        self.0.borrow_mut().push(format!("vol {v}"));
    }
    fn set_speed(&mut self, v: u16) {
        self.0.borrow_mut().push(format!("speed {v}"));
    }
    fn stop(&mut self) {
        self.0.borrow_mut().push("stop".into());
    }
}

#[cfg(feature = "office")]
fn player_device(user: &str, role: &str) -> (Device<Echo>, PlayLog) {
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_role(Permissions::new(
        "viewer",
        &["player.status", "player.queue"],
    ));
    ws.add_model(model());
    let log = PlayLog::default();
    let player = mouli_apps::register_player(&mut ws, Box::new(log.clone())).unwrap();
    let mut d = Device::new(ws, Echo(vec![]), "acme", user, role).unwrap();
    d.attach_player(player);
    (d, log)
}

#[cfg(feature = "office")]
fn last_load(log: &PlayLog) -> String {
    log.0
        .borrow()
        .iter()
        .rev()
        .find(|l| l.starts_with("load"))
        .cloned()
        .unwrap_or_default()
}

#[cfg(feature = "office")]
fn add_link(d: &mut Device<Echo>, url: &str) {
    d.on_key(Key::Char('o'));
    type_keys(d, url);
    d.on_key(Key::Enter);
}

#[test]
#[cfg(feature = "office")]
fn a_pasted_link_joins_the_queue_and_space_starts_it() {
    let (mut d, log) = player_device("owner", "owner");
    d.on_key(Key::F(10));
    assert_eq!(d.shell.active, Tab::Player);
    add_link(&mut d, "https://www.youtube.com/shorts/dQw4w9WgXcQ");
    assert_eq!(d.shell.player.view.queue.len(), 1);
    assert_eq!(d.shell.player.view.queue[0].kind, "short");
    assert!(
        d.shell.player.message.contains("Added"),
        "{}",
        d.shell.player.message
    );
    d.on_key(Key::Char(' '));
    assert_eq!(last_load(&log), "load YouTube Short dQw4w9WgXcQ");
    assert_eq!(d.shell.player.view.state, "loading");
    d.player_event(mouli_player::Event::Loaded(Some(31)));
    d.player_event(mouli_player::Event::Position(12));
    let v = &d.shell.player.view;
    assert_eq!(
        (v.state.as_str(), v.position, v.duration),
        ("playing", 12, Some(31))
    );
    assert!(v.queue[0].current);
}

#[test]
#[cfg(feature = "office")]
fn shorts_play_one_after_another_from_the_keyboard_with_no_scrolling() {
    let (mut d, log) = player_device("owner", "owner");
    d.on_key(Key::F(10));
    for id in ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"] {
        add_link(&mut d, &format!("https://www.youtube.com/shorts/{id}"));
    }
    d.on_key(Key::Char('f')); // the Shorts feed
    assert!(d.shell.player.view.shorts && d.shell.player.view.autoplay);
    d.on_key(Key::Char(' '));
    for _ in 0..3 {
        d.player_event(mouli_player::Event::Loaded(None));
        d.player_event(mouli_player::Event::Ended);
    }
    let loads: Vec<String> = log
        .0
        .borrow()
        .iter()
        .filter(|l| l.starts_with("load"))
        .cloned()
        .collect();
    assert_eq!(
        loads,
        [
            "load YouTube Short aaaaaaaaaaa",
            "load YouTube Short bbbbbbbbbbb",
            "load YouTube Short ccccccccccc"
        ]
    );
    assert_eq!(d.shell.player.view.state, "ended");
}

#[test]
#[cfg(feature = "office")]
fn thumbs_down_skips_and_the_keys_change_volume_and_speed_through_the_gate() {
    let (mut d, log) = player_device("owner", "owner");
    d.on_key(Key::F(10));
    add_link(&mut d, "/m/a.mp4");
    add_link(&mut d, "/m/b.mp4");
    d.on_key(Key::Char(' '));
    d.player_event(mouli_player::Event::Loaded(None));
    d.on_key(Key::Char('d'));
    assert_eq!(last_load(&log), "load b.mp4", "down skips to the next");
    d.on_key(Key::Char('-'));
    d.on_key(Key::Char(']'));
    assert_eq!(d.shell.player.view.volume, 75);
    assert_eq!(d.shell.player.view.speed, 125);
    assert!(
        d.shell
            .audit
            .rows
            .iter()
            .any(|r| r.action == "player.add" && r.outcome == "ok"),
        "calls are audited"
    );
    assert!(d.shell.audit.rows.iter().any(|r| r.action == "player.rate"));
}

#[test]
#[cfg(feature = "office")]
fn a_viewer_cannot_control_the_player_and_is_told_why() {
    let (mut d, log) = player_device("vic", "viewer");
    d.on_key(Key::F(10));
    add_link(&mut d, "/m/a.mp4");
    assert!(d.shell.player.view.queue.is_empty());
    assert!(
        !d.shell.player.message.is_empty(),
        "{}",
        d.shell.player.message
    );
    d.on_key(Key::Char(' '));
    assert!(log.0.borrow().is_empty(), "nothing was played");
}

#[test]
#[cfg(feature = "office")]
fn a_bad_link_is_refused_with_a_reason_on_the_screen() {
    let (mut d, log) = player_device("owner", "owner");
    d.on_key(Key::F(10));
    add_link(&mut d, "https://youtube.com.evil.com/watch?v=dQw4w9WgXcQ");
    assert!(d.shell.player.view.queue.is_empty());
    assert!(
        d.shell.player.message.contains("not a YouTube"),
        "{}",
        d.shell.player.message
    );
    assert!(log.0.borrow().is_empty());
}

#[test]
#[cfg(feature = "office")]
fn without_a_player_the_screen_says_so() {
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.on_key(Key::F(10));
    d.on_key(Key::Char(' '));
    assert!(
        d.shell.player.message.contains("not connected"),
        "{}",
        d.shell.player.message
    );
    d.player_event(mouli_player::Event::Ended); // harmless
}

// ---------------------------------------------------------------- the model acting as an agent

fn agent_device(dir: &tempfile::TempDir) -> Device<Echo> {
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_role(Permissions::new("agent", &["code.*"]));
    ws.add_model(model());
    let code = mouli_apps::register_code(&mut ws).unwrap();
    ws.register_app(Box::new(Notes)).unwrap();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "dana", "owner").unwrap();
    d.attach_code(code);
    d.open_project(dir.path()).unwrap();
    d
}

#[test]
fn the_agent_can_read_but_a_change_waits_for_the_person_who_can_approve_it() {
    let (dir, _diff, _new) = project();
    let old = read(&dir);
    let mut d = agent_device(&dir);

    let files = d.run_agent("/code.files");
    assert!(files.contains("src/tax.rs"), "{files}");
    let text = d.run_agent("/code.read path=src/tax.rs");
    assert!(text.contains(old.lines().next().unwrap()), "{text}");

    // The model proposes a whole-file edit; nothing is written yet.
    let held = d.run_agent("/code.edit path=src/tax.rs text=changed-by-the-agent");
    assert!(!held.is_empty());
    assert_eq!(read(&dir), old, "proposing writes nothing");

    // Applying needs approval and is held; the file is still untouched.
    let wait = d.run_agent("/code.apply");
    assert!(wait.contains("held for the person's approval"), "{wait}");
    assert_eq!(read(&dir), old);
    assert_eq!(d.shell.approvals.rows.len(), 1);
    let row = d.shell.approvals.rows[0].clone();
    assert_eq!(row.capability, "code.apply");
    assert_eq!(row.requester, "agent:chat");
    assert!(
        row.can_decide,
        "the person may decide the agent's request: {row:?}"
    );

    // The person approves; only then is the file written.
    d.run(Action::Decide {
        id: row.id.clone(),
        approve: true,
    });
    assert_eq!(read(&dir), "changed-by-the-agent");
    assert!(d.shell.approvals.rows.is_empty());
}

#[test]
fn the_person_can_refuse_what_the_agent_asked_for() {
    let (dir, _d, _n) = project();
    let old = read(&dir);
    let mut d = agent_device(&dir);
    d.run_agent("/code.edit path=src/tax.rs text=nope");
    d.run_agent("/code.apply");
    let id = d.shell.approvals.rows[0].id.clone();
    d.run(Action::Decide { id, approve: false });
    assert_eq!(read(&dir), old, "refused: nothing written");
}

#[test]
fn the_agent_is_limited_to_the_code_tools() {
    let (dir, _d, _n) = project();
    let mut d = agent_device(&dir);
    let denied = d.run_agent("/note.send to=everyone");
    assert!(
        denied.to_lowercase().contains("lacks permission")
            || denied.to_lowercase().contains("denied"),
        "{denied}"
    );
    let unknown = d.run_agent("/no.such.tool");
    assert!(!unknown.is_empty());
    assert!(d.shell.approvals.rows.is_empty(), "nothing was queued");
}

#[test]
fn the_agent_cannot_act_when_the_host_defined_no_agent_role() {
    let (dir, _d, _n) = project();
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_model(model());
    let code = mouli_apps::register_code(&mut ws).unwrap();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "dana", "owner").unwrap();
    d.attach_code(code);
    d.open_project(dir.path()).unwrap();
    assert!(d.run_agent("/code.files").contains("cannot act"));
}

#[test]
fn held_approvals_and_the_audit_chain_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut ws = workspace();
        mouli_device::restore(&mut ws, dir.path()).unwrap();
        let alice = ws.open_session("acme", "alice", "clerk").unwrap();
        ws.submit("acme", &alice, "/note.send to=bob").unwrap();
        let mut d = Device::new(ws, Echo(vec![]), "acme", "owner", "owner").unwrap();
        d.keep_state_in(dir.path()).unwrap();
        type_line(&mut d, "hello");
    }
    // The device is gone; start again from the files alone.
    let mut ws = workspace();
    let r = mouli_device::restore(&mut ws, dir.path()).unwrap();
    assert!(r.chain_ok);
    assert_eq!(r.pending, 1);
    assert!(r.records >= 3);
    let mut d = Device::new(ws, Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.keep_state_in(dir.path()).unwrap();
    assert_eq!(d.shell.approvals.rows.len(), 1);
    assert_eq!(d.shell.approvals.rows[0].requester, "alice");
    // Deciding it runs the original call and empties the saved queue.
    d.on_key(Key::F(3));
    d.on_key(Key::Char('a'));
    assert!(d.shell.approvals.rows.is_empty());
    let saved = std::fs::read_to_string(dir.path().join("pending.txt")).unwrap();
    assert!(saved.is_empty());
    // The file only grew, and still verifies after a second restart.
    let mut ws = workspace();
    let r2 = mouli_device::restore(&mut ws, dir.path()).unwrap();
    assert!(r2.chain_ok && r2.records > r.records && r2.pending == 0);
}

#[test]
fn an_edited_audit_file_loads_but_raises_the_security_finding() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
        d.keep_state_in(dir.path()).unwrap();
        type_line(&mut d, "hello");
    }
    let path = dir.path().join("audit.log");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("\towner\t", "\tmallory\t", 1)).unwrap();
    let mut ws = workspace();
    let r = mouli_device::restore(&mut ws, dir.path()).unwrap();
    assert!(!r.chain_ok);
    let d = Device::new(ws, Echo(vec![]), "acme", "owner", "owner").unwrap();
    assert!(d
        .shell
        .security
        .findings
        .iter()
        .any(|f| f.id == "audit-chain"));
}

#[test]
fn a_damaged_state_file_is_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("audit.log"), "garbage\n").unwrap();
    let mut ws = workspace();
    let e = mouli_device::restore(&mut ws, dir.path()).unwrap_err();
    assert!(e.contains("audit.log line 1"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("audit.log")).unwrap(),
        "garbage\n"
    );
}

#[test]
fn default_roles_split_asking_from_deciding() {
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    mouli_device::add_default_roles(&mut ws);
    assert!(ws.role_can(mouli_device::OWNER, "approve"));
    assert!(ws.role_can(mouli_device::CLERK, "code.propose"));
    assert!(!ws.role_can(mouli_device::CLERK, "code.apply"));
    assert!(!ws.role_can(mouli_device::CLERK, "approve"));
    assert!(ws.role_can(mouli_device::VIEWER, "code.read"));
    assert!(!ws.role_can(mouli_device::VIEWER, "code.propose"));
}

#[test]
fn a_second_person_signs_in_on_the_same_device_and_approves() {
    let mut ws = workspace();
    ws.add_role(Permissions::new("viewer", &["note.read"]));
    let mut d = Device::new(ws, Echo(vec![]), "acme", "alice", "clerk").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "/note.send to=bob");
    assert_eq!(d.shell.approvals.rows.len(), 1);
    assert!(!d.shell.approvals.rows[0].can_decide);

    // Bad sign-ins change nothing.
    assert!(d.switch_user("  ", "owner").is_err());
    assert!(d.switch_user("bob", "nonsense").is_err());
    assert_eq!(d.user(), "alice");

    d.switch_user("bob", "owner").unwrap();
    assert_eq!(d.user(), "bob");
    assert!(d.shell.chat.messages.is_empty());
    assert!(d.shell.approvals.rows[0].can_decide);
    d.on_key(Key::F(3));
    d.on_key(Key::Char('a'));
    assert!(d.shell.approvals.rows.is_empty());
    let a = d.workspace().audit();
    assert!(a.verify().is_ok());
    assert!(a
        .records()
        .iter()
        .any(|r| r.action == "signin" && r.actor == "bob" && r.subject.contains("was alice")));
    assert!(a
        .records()
        .iter()
        .any(|r| r.action == "approve" && r.actor == "bob"));
}

#[test]
fn the_signin_command_and_menu_switch_the_person_without_reaching_the_model() {
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "alice", "clerk").unwrap();
    d.on_key(Key::F(1));
    assert_eq!(d.run(Action::SwitchUser), Outcome::Handled);
    let notice = chat_texts(&d).pop().unwrap().1;
    assert!(notice.contains("alice") && notice.contains("owner"));

    type_line(&mut d, "/signin bob owner");
    assert_eq!(d.user(), "bob");
    type_line(&mut d, "/signin bob");
    type_line(&mut d, "/signin eve wizard");
    assert_eq!(d.user(), "bob");
    let texts = chat_texts(&d);
    assert!(texts.iter().any(|(_, t)| t.contains("Usage: /signin")));
    assert!(texts.iter().any(|(_, t)| t.contains("Could not sign in")));
    // Nothing was sent to the model: only the person's own lines and notices are on screen.
    assert!(!texts.iter().any(|(r, _)| *r == Role::Assistant));
}

#[test]
fn each_persons_conversation_is_kept_apart_and_comes_back_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut d = Device::new(workspace(), Echo(vec![]), "acme", "alice", "clerk").unwrap();
        d.keep_state_in(dir.path()).unwrap();
        d.on_key(Key::F(1));
        type_line(&mut d, "alice's secret plan");
        d.switch_user("bob", "owner").unwrap();
        assert!(chat_texts(&d).is_empty());
        type_line(&mut d, "bob here");
        d.switch_user("alice", "clerk").unwrap();
        assert!(chat_texts(&d)
            .iter()
            .any(|(_, t)| t.contains("secret plan")));
        assert!(!chat_texts(&d).iter().any(|(_, t)| t.contains("bob here")));
    }
    // Restart: bob signs in first and sees only his own.
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "bob", "owner").unwrap();
    d.keep_state_in(dir.path()).unwrap();
    let t = chat_texts(&d);
    assert!(t.iter().any(|(r, x)| *r == Role::User && x == "bob here"));
    assert!(!t.iter().any(|(_, x)| x.contains("secret")));
    assert_eq!(t.iter().filter(|(_, x)| x.contains("restored")).count(), 1);
    drop(d);
    // Restoring again does not pile up notices.
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "bob", "owner").unwrap();
    d.keep_state_in(dir.path()).unwrap();
    assert_eq!(
        chat_texts(&d)
            .iter()
            .filter(|(_, x)| x.contains("restored"))
            .count(),
        1
    );
}

#[test]
fn help_lists_screens_and_only_the_capabilities_the_role_may_run() {
    let mut ws = workspace();
    ws.add_role(Permissions::new("reader", &["note.read"]));
    let mut d = Device::new(ws, Echo(vec![]), "acme", "rita", "reader").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "/help");
    let help = chat_texts(&d).pop().unwrap().1;
    assert!(help.contains("Chat") && help.contains("Audit"), "{help}");
    assert!(
        help.contains("/note.read") && !help.contains("/note.send"),
        "{help}"
    );
    assert!(help.contains("/signin"));
    d.switch_user("olga", "owner").unwrap();
    type_line(&mut d, "/help");
    assert!(chat_texts(&d).pop().unwrap().1.contains("/note.send"));
}

#[test]
fn apps_and_files_commands_show_what_exists_and_what_the_role_may_do() {
    let mut ws = workspace();
    ws.add_role(Permissions::new("reader", &["note.read"]));
    let s = ws.open_session("acme", "zed", "owner").unwrap();
    ws.put_artifact("acme", &s, "report.txt", "text/plain", b"v1")
        .unwrap();
    ws.put_artifact("acme", &s, "report.txt", "text/plain", b"version two")
        .unwrap();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "rita", "reader").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "/apps");
    let apps = chat_texts(&d).pop().unwrap().1;
    assert!(apps.contains("note.read  reads; you may"), "{apps}");
    assert!(
        apps.contains("note.send  needs approval; not for your role"),
        "{apps}"
    );
    type_line(&mut d, "/files");
    let files = chat_texts(&d).pop().unwrap().1;
    assert!(
        files.contains("1 file(s)") && files.contains("report.txt  v2"),
        "{files}"
    );
    assert!(files.contains("11 bytes"), "{files}");
}

#[test]
fn save_writes_the_last_answer_to_a_new_file_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "alice", "clerk").unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "/save");
    assert!(chat_texts(&d).pop().unwrap().1.contains("no data disk"));
    d.set_export_dir(&dir.path().join("exports"));
    type_line(&mut d, "/save");
    assert!(chat_texts(&d).pop().unwrap().1.contains("no answer"));

    type_line(&mut d, "first question");
    type_line(&mut d, "second question");
    type_line(&mut d, "/save");
    let a1 = std::fs::read_to_string(dir.path().join("exports/answer-1.txt")).unwrap();
    assert!(a1.contains("second question"), "{a1}");
    type_line(&mut d, "/save");
    assert!(dir.path().join("exports/answer-2.txt").exists());
    type_line(&mut d, "/save notes.md");
    assert!(dir.path().join("exports/notes.md").exists());
    type_line(&mut d, "/save notes.md");
    assert!(chat_texts(&d).pop().unwrap().1.contains("already exists"));
    for bad in ["../x", ".hidden", "a b/c", "x/y"] {
        type_line(&mut d, &format!("/save {bad}"));
        assert!(
            chat_texts(&d).pop().unwrap().1.contains("Not saved"),
            "{bad}"
        );
    }
    assert!(!dir.path().join("x").exists());
    assert!(d
        .workspace()
        .audit()
        .records()
        .iter()
        .any(|r| r.action == "answer.save" && r.subject == "notes.md"));
}

fn finding<'a>(d: &'a Device<Echo>, id: &str) -> Option<&'a mouli_screen::screens::Finding> {
    d.shell.security.findings.iter().find(|f| f.id == id)
}

#[test]
fn five_refusals_lock_a_person_out_until_an_approver_unlocks_them() {
    let mut ws = workspace();
    ws.add_role(Permissions::new("guest", &["note.read"]));
    let mut d = Device::new(ws, Echo(vec![]), "acme", "gus", "guest").unwrap();
    d.on_key(Key::F(1));
    for _ in 0..5 {
        type_line(&mut d, "/note.send to=bob");
    }
    let f = finding(&d, "lockout-gus").expect("a lockout finding");
    assert!(f.detail.contains("/unlock gus"));
    type_line(&mut d, "/note.read");
    assert!(chat_texts(&d).iter().any(|(_, t)| t.contains("locked out")));
    type_line(&mut d, "/unlock");
    assert!(chat_texts(&d).pop().unwrap().1.contains("Locked out: gus"));
    // The locked person cannot unlock themselves.
    type_line(&mut d, "/unlock gus");
    assert!(chat_texts(&d).pop().unwrap().1.contains("Not unlocked"));
    assert!(finding(&d, "lockout-gus").is_some());
    // An owner can.
    d.switch_user("boss", "owner").unwrap();
    type_line(&mut d, "/unlock gus");
    assert!(chat_texts(&d).pop().unwrap().1.contains("can act again"));
    assert!(finding(&d, "lockout-gus").is_none());
    d.switch_user("gus", "guest").unwrap();
    type_line(&mut d, "/note.read");
    assert!(chat_texts(&d).pop().unwrap().1.contains("ran note.read"));
}

#[test]
fn a_burst_of_refusals_between_successes_is_reported_as_an_anomaly() {
    let mut ws = workspace();
    ws.add_role(Permissions::new("guest", &["note.read"]));
    let mut d = Device::new(ws, Echo(vec![]), "acme", "gus", "guest").unwrap();
    d.on_key(Key::F(1));
    for _ in 0..4 {
        type_line(&mut d, "/note.send to=bob");
        type_line(&mut d, "/note.read"); // a success in between: never locked out
    }
    assert!(finding(&d, "lockout-gus").is_none());
    let f = finding(&d, "burst-gus").expect("a burst finding");
    assert_eq!(f.severity, mouli_screen::screens::Severity::High);
    // A calm log has none.
    let calm = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    assert!(calm
        .shell
        .security
        .findings
        .iter()
        .all(|f| !f.id.starts_with("burst")));
}

#[test]
fn a_finding_can_be_marked_seen_and_returns_if_it_changes_or_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |dir: &std::path::Path| {
        let mut ws = workspace();
        mouli_device::restore(&mut ws, dir).unwrap();
        ws.add_role(Permissions::new("guest", &["note.read"]));
        let mut d = Device::new(ws, Echo(vec![]), "acme", "gus", "guest").unwrap();
        d.keep_state_in(dir).unwrap();
        d
    };
    let mut d = mk(dir.path());
    d.on_key(Key::F(1));
    type_line(&mut d, "/note.send to=bob");
    let before = d.shell.security.score;
    assert!(before > 0);
    assert!(!finding(&d, "denied").unwrap().acknowledged);

    d.on_key(Key::F(5));
    let at = d
        .shell
        .security
        .findings
        .iter()
        .position(|f| f.id == "denied")
        .unwrap();
    d.shell.security.cursor.selected = at;
    d.on_key(Key::Char('a'));
    assert!(finding(&d, "denied").unwrap().acknowledged);
    assert!(
        d.shell.security.score < before,
        "a seen finding stops counting"
    );
    // Again: nothing new to do.
    d.on_key(Key::Char('a'));
    assert!(d.shell.security.message.contains("Already"));
    assert!(d
        .workspace()
        .audit()
        .records()
        .iter()
        .any(|r| r.action == "finding.ack"
            && r.actor == "gus"
            && r.subject.starts_with("denied|")));

    // More refusals change what it says, so it needs a fresh look.
    d.on_key(Key::F(1));
    type_line(&mut d, "/note.send to=bob");
    assert!(!finding(&d, "denied").unwrap().acknowledged);
    d.on_key(Key::F(5));
    let at = d
        .shell
        .security
        .findings
        .iter()
        .position(|f| f.id == "denied")
        .unwrap();
    d.shell.security.cursor.selected = at;
    d.on_key(Key::Char('a'));
    assert!(finding(&d, "denied").unwrap().acknowledged);
    drop(d);

    // After a restart the acknowledgement is still there (it lives in the audit chain).
    let d = mk(dir.path());
    assert!(finding(&d, "denied").unwrap().acknowledged);
}

#[test]
fn a_broken_chain_can_be_marked_seen_but_still_counts() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
        d.keep_state_in(dir.path()).unwrap();
        d.on_key(Key::F(1));
        type_line(&mut d, "hello");
    }
    let path = dir.path().join("audit.log");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("\towner\t", "\tmallory\t", 1)).unwrap();
    let mut ws = workspace();
    mouli_device::restore(&mut ws, dir.path()).unwrap();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "owner", "owner").unwrap();
    let before = d.shell.security.score;
    assert!(before >= 70);
    d.on_key(Key::F(5));
    let at = d
        .shell
        .security
        .findings
        .iter()
        .position(|f| f.id == "audit-chain")
        .unwrap();
    d.shell.security.cursor.selected = at;
    d.on_key(Key::Char('a'));
    assert!(finding(&d, "audit-chain").unwrap().acknowledged);
    assert!(d.shell.security.message.contains("still counts"));
    assert!(d.shell.security.score >= 70);
}

fn run_once(dir: &std::path::Path, line: &str) {
    // Each start opens a session, so each call adds at least one record.
    let mut d = reopen_as(
        dir,
        if line.starts_with("/signin") {
            "zed"
        } else {
            "owner"
        },
    );
    type_line(&mut d, line);
}

fn reopen_as(dir: &std::path::Path, user: &str) -> Device<Echo> {
    let mut ws = workspace();
    mouli_device::restore(&mut ws, dir).unwrap();
    let mut d = Device::new(ws, Echo(vec![]), "acme", user, "owner").unwrap();
    d.keep_state_in(dir).unwrap();
    d
}

fn reopen(dir: &std::path::Path) -> Device<Echo> {
    reopen_as(dir, "owner")
}

fn has_checkpoint_alert(d: &Device<Echo>) -> bool {
    d.shell
        .security
        .findings
        .iter()
        .any(|f| f.id == "checkpoint")
}

#[test]
fn a_normal_restart_raises_no_checkpoint_alert() {
    let dir = tempfile::tempdir().unwrap();
    run_once(dir.path(), "hello");
    assert!(dir.path().join("checkpoint.txt").exists());
    let d = reopen(dir.path());
    assert!(!has_checkpoint_alert(&d));
    drop(d);
    assert!(!has_checkpoint_alert(&reopen(dir.path())));
}

#[test]
fn cutting_records_off_the_log_is_caught_and_stays_caught() {
    let dir = tempfile::tempdir().unwrap();
    run_once(dir.path(), "hello");
    run_once(dir.path(), "hello");
    run_once(dir.path(), "hello");
    let path = dir.path().join("audit.log");
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().collect();
    assert!(kept.len() >= 2, "need records to cut: {}", kept.len());
    // Cut two: the start itself adds a record, and an identical one would hide a cut of one.
    // The chain still verifies after the cut; only the checkpoint knows records are missing.
    std::fs::write(&path, format!("{}\n", kept[..kept.len() - 2].join("\n"))).unwrap();
    let d = reopen(dir.path());
    assert!(has_checkpoint_alert(&d));
    drop(d);
    // The alert is in the chain, so it is still there after the next start.
    assert!(has_checkpoint_alert(&reopen(dir.path())));
}

#[test]
fn swapping_in_another_valid_log_is_caught() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        run_once(a.path(), "hello");
        run_once(b.path(), "/signin zed viewer");
    }
    std::fs::copy(b.path().join("audit.log"), a.path().join("audit.log")).unwrap();
    assert!(has_checkpoint_alert(&reopen(a.path())));
}

#[test]
fn deleting_or_forging_the_checkpoint_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    run_once(dir.path(), "hello");
    let cp = dir.path().join("checkpoint.txt");
    let good = std::fs::read_to_string(&cp).unwrap();
    std::fs::remove_file(&cp).unwrap();
    assert!(has_checkpoint_alert(&reopen(dir.path())));

    let dir2 = tempfile::tempdir().unwrap();
    run_once(dir2.path(), "hello");
    run_once(dir2.path(), "hello");
    let cp2 = dir2.path().join("checkpoint.txt");
    let mut parts: Vec<String> = std::fs::read_to_string(&cp2)
        .unwrap()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    parts[0] = "1".into(); // claim fewer records; the tag no longer fits
    std::fs::write(&cp2, parts.join(" ")).unwrap();
    assert!(has_checkpoint_alert(&reopen(dir2.path())));
    assert!(!good.is_empty());
}

#[test]
fn the_first_start_and_an_upgrade_from_older_state_are_quiet() {
    let dir = tempfile::tempdir().unwrap();
    run_once(dir.path(), "hello");
    // State saved before checkpoints existed: a log, no key, no checkpoint.
    std::fs::remove_file(dir.path().join("checkpoint.txt")).unwrap();
    std::fs::remove_file(dir.path().join("checkpoint.key")).unwrap();
    let d = reopen(dir.path());
    assert!(!has_checkpoint_alert(&d));
    assert!(dir.path().join("checkpoint.txt").exists());
}

#[cfg(unix)]
#[test]
fn the_checkpoint_key_is_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    run_once(dir.path(), "hello");
    let mode = std::fs::metadata(dir.path().join("checkpoint.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);
}

fn gus_with_refusals(n: usize) -> Device<Echo> {
    let mut ws = workspace();
    ws.add_role(Permissions::new("guest", &["note.read"]));
    let mut d = Device::new(ws, Echo(vec![]), "acme", "gus", "guest").unwrap();
    d.on_key(Key::F(1));
    for _ in 0..n {
        type_line(&mut d, "/note.send to=bob");
    }
    d
}

#[test]
fn e_on_the_security_screen_saves_the_evidence_for_a_finding() {
    let out = tempfile::tempdir().unwrap();
    let mut d = gus_with_refusals(5);
    d.set_export_dir(out.path());
    d.on_key(Key::F(5));
    let i = d
        .shell
        .security
        .findings
        .iter()
        .position(|f| f.id == "lockout-gus")
        .unwrap();
    d.shell.security.cursor.selected = i;
    d.on_key(Key::Char('e'));
    let msg = d.shell.security.message.clone();
    assert!(msg.starts_with("Evidence saved to "), "{msg}");
    let file = out.path().join("evidence-lockout-gus-1.txt");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("finding:   lockout-gus (high)"));
    assert!(text.contains("chain check: every record matches"));
    // The five refusals are in it, and nothing from other people.
    assert_eq!(text.matches("\tgus\t").count(), 5, "{text}");
    assert!(text.contains("chain head: ") && !text.contains("(empty)"));
    // The export is itself audited, and a second one gets a new file.
    assert!(d
        .shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == "finding.export" && r.target.starts_with("lockout-gus|")));
    d.on_key(Key::F(5));
    d.shell.security.cursor.selected = d
        .shell
        .security
        .findings
        .iter()
        .position(|f| f.id == "lockout-gus")
        .unwrap();
    d.on_key(Key::Char('e'));
    assert!(out.path().join("evidence-lockout-gus-2.txt").exists());
}

#[test]
fn evidence_says_so_when_there_is_nowhere_to_save() {
    let mut d = gus_with_refusals(2);
    d.on_key(Key::F(5));
    d.on_key(Key::Char('e'));
    assert!(d
        .shell
        .security
        .message
        .starts_with("Evidence not saved: there is no data disk"));
}

#[test]
fn evidence_for_a_broken_chain_reports_the_break() {
    let dir = tempfile::tempdir().unwrap();
    run_once(dir.path(), "hello");
    run_once(dir.path(), "hello");
    let path = dir.path().join("audit.log");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("\towner\t", "\tmallory\t", 1)).unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut d = reopen(dir.path());
    d.set_export_dir(out.path());
    d.on_key(Key::F(5));
    let i = d
        .shell
        .security
        .findings
        .iter()
        .position(|f| f.id == "audit-chain")
        .unwrap();
    d.shell.security.cursor.selected = i;
    d.on_key(Key::Char('e'));
    let text = std::fs::read_to_string(out.path().join("evidence-audit-chain-1.txt")).unwrap();
    assert!(text.contains("chain check: BROKEN at record 1"), "{text}");
}

#[test]
fn a_locked_out_person_cannot_clear_the_lockout_with_their_own_host_actions() {
    let out = tempfile::tempdir().unwrap();
    let mut d = gus_with_refusals(5);
    d.set_export_dir(out.path());
    d.record_event("screenshot.save", "shot.png", "ok");
    d.on_key(Key::F(5));
    d.shell.security.cursor.selected = d
        .shell
        .security
        .findings
        .iter()
        .position(|f| f.id == "lockout-gus")
        .unwrap();
    d.on_key(Key::Char('a'));
    d.on_key(Key::Char('e'));
    assert!(finding(&d, "lockout-gus").is_some());
}

// ---- accounts: passphrase sign-in and administration ----

const OWNER_PASS: &str = "correct horse battery";

fn last_note(d: &Device<Echo>) -> String {
    chat_texts(d).pop().map(|m| m.1).unwrap_or_default()
}

/// A device on `dir` with one owner account (`owner`), then restarted so it is locked.
fn locked_device(dir: &std::path::Path) -> Device<Echo> {
    {
        let mut d = reopen(dir);
        d.on_key(Key::F(1));
        type_line(&mut d, "/passwd");
        type_line(&mut d, OWNER_PASS);
        assert!(last_note(&d).starts_with("Saved."), "{}", last_note(&d));
    }
    let mut d = reopen(dir);
    d.lock();
    d
}

fn sign_in(d: &mut Device<Echo>, name: &str, pass: &str) {
    type_line(d, &format!("/signin {name}"));
    type_line(d, pass);
}

fn audit_has(d: &Device<Echo>, action: &str, outcome_starts: &str) -> bool {
    d.shell
        .audit
        .rows
        .iter()
        .any(|r| r.action == action && r.outcome.starts_with(outcome_starts))
}

#[test]
fn without_accounts_the_device_is_open_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = reopen(dir.path());
    d.on_key(Key::F(1));
    assert!(!d.has_accounts());
    d.lock(); // nothing to lock against
    assert!(!d.is_locked());
    type_line(&mut d, "/signin amy clerk");
    assert!(
        last_note(&d).contains("Signed in as amy (clerk)"),
        "{}",
        last_note(&d)
    );
}

#[test]
fn the_first_account_is_set_up_by_the_owner_and_then_the_device_starts_locked() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = locked_device(dir.path());
    assert!(d.has_accounts() && d.is_locked());
    // Locked: a message does not reach the model, and other screens cannot be opened.
    type_line(&mut d, "hello");
    assert!(chat_texts(&d).iter().all(|(_, t)| !t.contains("you said")));
    assert!(last_note(&d).starts_with("Locked."), "{}", last_note(&d));
    d.on_key(Key::F(3));
    assert_eq!(d.shell.active, Tab::Chat, "no other screen is reachable");
    assert_eq!(
        d.run(Action::Call {
            capability: "note.read".into(),
            args: vec![]
        }),
        Outcome::Handled
    );
    assert!(last_note(&d).starts_with("Locked."));
    // The right passphrase signs in with the account's role.
    sign_in(&mut d, "owner", OWNER_PASS);
    assert!(!d.is_locked());
    assert!(
        last_note(&d).contains("Signed in as owner (owner)"),
        "{}",
        last_note(&d)
    );
    assert!(audit_has(&d, "signin", "ok"));
    type_line(&mut d, "hello");
    assert!(chat_texts(&d).iter().any(|(_, t)| t.contains("you said")));
}

#[test]
fn a_wrong_passphrase_or_a_name_nobody_has_is_refused_and_audited_without_telling_which() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = locked_device(dir.path());
    sign_in(&mut d, "owner", "not the passphrase");
    let wrong = last_note(&d);
    sign_in(&mut d, "ghost", OWNER_PASS);
    assert_eq!(last_note(&d), wrong, "the same message for both");
    assert!(wrong.contains("Wrong passphrase, or there is no such account"));
    assert!(d.is_locked());
    assert!(audit_has(&d, "signin", "denied: wrong passphrase"));
}

#[test]
fn the_passphrase_is_never_shown_stored_in_the_chat_or_written_anywhere_readable() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = locked_device(dir.path());
    sign_in(&mut d, "owner", OWNER_PASS);
    sign_in(&mut d, "owner", "wrong passphrase 123");
    for (_, text) in chat_texts(&d) {
        assert!(
            !text.contains("correct horse") && !text.contains("wrong passphrase 123"),
            "{text}"
        );
    }
    for row in &d.shell.audit.rows {
        let all = format!(
            "{} {} {} {}",
            row.actor, row.action, row.target, row.outcome
        );
        assert!(
            !all.contains("correct horse") && !all.contains("wrong passphrase 123"),
            "{all}"
        );
    }
    // Nothing on the data disk holds it either: not the accounts file, not the chat, not the log.
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("correct horse") && !text.contains("wrong passphrase 123"),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn once_accounts_exist_nobody_can_choose_their_own_role() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = locked_device(dir.path());
    sign_in(&mut d, "owner", OWNER_PASS);
    type_line(&mut d, "/signin mallory owner");
    assert!(
        last_note(&d).starts_with("Type /signin <name>"),
        "{}",
        last_note(&d)
    );
    // An administrator adds amy as a clerk; amy signs in as a clerk, whatever she types.
    type_line(&mut d, "/adduser amy clerk");
    type_line(&mut d, "amy long passphrase");
    assert!(last_note(&d).starts_with("Saved."), "{}", last_note(&d));
    sign_in(&mut d, "amy", "amy long passphrase");
    assert!(
        last_note(&d).contains("Signed in as amy (clerk)"),
        "{}",
        last_note(&d)
    );
    // A clerk cannot manage accounts: refused and audited.
    for cmd in [
        "/adduser bob clerk",
        "/users",
        "/setrole amy owner",
        "/deluser owner",
        "/resetpass owner",
    ] {
        type_line(&mut d, cmd);
        assert!(
            last_note(&d).starts_with("Not allowed"),
            "{cmd}: {}",
            last_note(&d)
        );
    }
    assert!(audit_has(&d, "accounts.admin", "denied"));
    type_line(&mut d, "/passwd");
    assert!(
        d.shell.chat.is_secret(),
        "a clerk can change their own passphrase"
    );
}

#[test]
fn five_wrong_passphrases_lock_the_name_out_until_an_approver_unlocks_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = locked_device(dir.path());
    for _ in 0..5 {
        sign_in(&mut d, "owner", "nope nope nope");
    }
    // Even the right passphrase cannot be tried now: no hidden entry is offered at all.
    type_line(&mut d, "/signin owner");
    assert!(last_note(&d).contains("locked out"), "{}", last_note(&d));
    assert!(!d.shell.chat.is_secret());
    assert!(d.is_locked());
    // The restart does not clear it (it lives in the audit chain).
    let mut d = reopen(dir.path());
    d.lock();
    type_line(&mut d, "/signin owner");
    assert!(last_note(&d).contains("locked out"), "{}", last_note(&d));
    assert!(!d.shell.chat.is_secret());
}

#[test]
fn administration_adds_changes_resets_and_removes_accounts_and_keeps_one_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = locked_device(dir.path());
    sign_in(&mut d, "owner", OWNER_PASS);
    type_line(&mut d, "/adduser amy clerk");
    type_line(&mut d, "short");
    assert!(last_note(&d).starts_with("Not saved"), "{}", last_note(&d));
    type_line(&mut d, "/adduser amy nosuchrole");
    assert!(
        last_note(&d).contains("There is no role"),
        "{}",
        last_note(&d)
    );
    type_line(&mut d, "/adduser bad!name clerk");
    assert!(last_note(&d).starts_with("Names use"), "{}", last_note(&d));
    type_line(&mut d, "/adduser amy clerk");
    type_line(&mut d, "amy long passphrase");
    type_line(&mut d, "/adduser amy clerk");
    assert!(
        last_note(&d).contains("already has an account"),
        "{}",
        last_note(&d)
    );
    type_line(&mut d, "/users");
    let list = last_note(&d);
    assert!(
        list.contains("amy  clerk") && list.contains("owner  owner"),
        "{list}"
    );
    type_line(&mut d, "/resetpass amy");
    type_line(&mut d, "a new long passphrase");
    type_line(&mut d, "/setrole amy owner");
    assert!(
        last_note(&d).contains("now has the owner role"),
        "{}",
        last_note(&d)
    );
    // The last owner is protected; with a second owner the first can be removed.
    type_line(&mut d, "/setrole amy clerk");
    type_line(&mut d, "/setrole owner clerk");
    assert!(
        last_note(&d).starts_with("Not changed"),
        "{}",
        last_note(&d)
    );
    type_line(&mut d, "/deluser owner");
    assert!(
        last_note(&d).starts_with("Not removed"),
        "{}",
        last_note(&d)
    );
    type_line(&mut d, "/deluser ghost");
    assert!(
        last_note(&d).starts_with("Not removed"),
        "{}",
        last_note(&d)
    );
    assert!(
        audit_has(&d, "account.add", "ok")
            && audit_has(&d, "passphrase.reset", "ok")
            && audit_has(&d, "account.role", "ok")
    );
    // The reset passphrase works and the old one does not; changes survive a restart.
    let mut d = reopen(dir.path());
    d.lock();
    sign_in(&mut d, "amy", "amy long passphrase");
    assert!(d.is_locked(), "the old passphrase no longer works");
    sign_in(&mut d, "amy", "a new long passphrase");
    assert!(
        last_note(&d).contains("Signed in as amy (clerk)"),
        "{}",
        last_note(&d)
    );
}

#[test]
fn a_damaged_accounts_file_keeps_the_device_locked_and_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    drop(locked_device(dir.path()));
    let path = dir.path().join("users.txt");
    std::fs::write(&path, "garbage that is not an account\n").unwrap();
    let mut d = reopen(dir.path());
    assert!(
        d.has_accounts(),
        "a file that cannot be read counts as accounts"
    );
    d.lock();
    assert!(d.is_locked());
    assert!(
        last_note(&d).contains("cannot be read"),
        "{}",
        last_note(&d)
    );
    sign_in(&mut d, "owner", OWNER_PASS);
    assert!(d.is_locked());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "garbage that is not an account\n"
    );
}

#[test]
fn only_someone_who_can_administer_may_set_up_the_first_account() {
    let dir = tempfile::tempdir().unwrap();
    let mut ws = workspace();
    mouli_device::restore(&mut ws, dir.path()).unwrap();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "amy", "clerk").unwrap();
    d.keep_state_in(dir.path()).unwrap();
    d.on_key(Key::F(1));
    type_line(&mut d, "/passwd");
    assert!(
        last_note(&d).starts_with("Not allowed"),
        "{}",
        last_note(&d)
    );
    assert!(!d.shell.chat.is_secret());
    assert!(!dir.path().join("users.txt").exists());
}

fn origin_rows(d: &Device<Echo>) -> Vec<String> {
    d.shell
        .audit
        .rows
        .iter()
        .filter(|r| r.action == "patch.origin" || r.action == "patch.propose")
        .map(|r| format!("{} | {} | {}", r.actor, r.action, r.target))
        .collect()
}

#[test]
fn the_audit_trail_says_which_model_proposed_a_change_and_who_applied_it() {
    let (dir, diff, _new) = project();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    d.propose_patch_by(dir.path(), &diff, "qwen2.5-1.5b-q4_k_m.gguf")
        .unwrap();
    d.on_key(Key::F(6));
    d.on_key(Key::Char('a'));
    // Applying twice must not write a second origin line.
    d.on_key(Key::Char('a'));
    let rows = origin_rows(&d);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].contains("patch.propose")
            && rows[0].contains("[proposed by qwen2.5-1.5b-q4_k_m.gguf]"),
        "{rows:?}"
    );
    assert_eq!(
        rows[1],
        "owner | patch.origin | proposed by qwen2.5-1.5b-q4_k_m.gguf, applied by owner"
    );
    assert_eq!(d.shell.audit.chain, Chain::Verified);
}

#[test]
fn a_hostile_or_empty_label_cannot_forge_audit_lines_and_a_rejected_change_has_no_origin() {
    let (dir, diff, _new) = project();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    let hostile = format!("evil\n9 | owner | patch.origin | forged{}", "x".repeat(200));
    d.propose_patch_by(dir.path(), &diff, &hostile).unwrap();
    d.on_key(Key::F(6));
    d.on_key(Key::Char('x'));
    let rows = origin_rows(&d);
    assert_eq!(rows.len(), 1, "only the proposal: {rows:?}");
    assert!(!rows[0].contains('\n'));
    let label = rows[0]
        .split("[proposed by ")
        .nth(1)
        .unwrap()
        .trim_end_matches(']');
    assert!(label.chars().count() <= 64, "{label}");
    // An empty label and the plain call both say unknown.
    d.propose_patch_by(dir.path(), &diff, "  \n ").unwrap();
    d.propose_patch(dir.path(), &diff).unwrap();
    d.on_key(Key::F(1)); // any key refreshes the audit rows
    let rows = origin_rows(&d);
    assert_eq!(
        rows.iter()
            .filter(|r| r.contains("[proposed by unknown]"))
            .count(),
        2,
        "{rows:?}"
    );
}

#[test]
fn a_change_applied_after_a_second_persons_approval_names_its_proposer() {
    let (dir, diff, new) = project();
    let (ws, code) = code_workspace();
    let mut d = Device::new(ws, Echo(vec![]), "acme", "dana", "dev").unwrap();
    d.attach_code(code);
    d.propose_patch_by(dir.path(), &diff, "llama-3.2-3b.gguf")
        .unwrap();
    d.on_key(Key::F(6));
    d.on_key(Key::Char('a'));
    assert!(
        origin_rows(&d).iter().all(|r| !r.contains("patch.origin")),
        "held, not applied yet"
    );
    // The owner approves from the approvals screen of the same device.
    d.switch_user("owner", "owner").unwrap();
    d.on_key(Key::F(3));
    let row = d
        .shell
        .approvals
        .rows
        .iter()
        .position(|r| r.capability == "code.apply")
        .expect("the held apply");
    d.shell.approvals.cursor.selected = row;
    d.on_key(Key::Char('a'));
    assert_eq!(read(&dir), new, "approved apply wrote the file");
    let rows = origin_rows(&d);
    assert!(
        rows.iter().any(|r| r == "owner | patch.origin | proposed by llama-3.2-3b.gguf, applied on approval by owner"),
        "{rows:?}"
    );
}

#[test]
fn review_comments_on_lines_are_kept_in_the_audit_trail_under_the_reviewer() {
    use mouli_device::CommentError;
    let (dir, diff, _new) = project();
    let mut d = Device::new(workspace(), Echo(vec![]), "acme", "owner", "owner").unwrap();
    assert_eq!(
        d.comment_on_patch("src/tax.rs", 2, "why +50?"),
        Err(CommentError::NoChange)
    );
    d.propose_patch_by(dir.path(), &diff, "user").unwrap();
    d.comment_on_patch("src/tax.rs", 2, "rounds half up,\n  please add a test")
        .unwrap();
    assert_eq!(
        d.comment_on_patch("src/other.rs", 1, "x"),
        Err(CommentError::NotInChange("src/other.rs".into()))
    );
    assert_eq!(
        d.comment_on_patch("src/tax.rs", 0, "x"),
        Err(CommentError::BadLine)
    );
    assert_eq!(
        d.comment_on_patch("src/tax.rs", 1, " \n\t "),
        Err(CommentError::Empty)
    );
    let long = "y".repeat(2000);
    d.comment_on_patch("src/tax.rs", 3, &long).unwrap();
    d.on_key(Key::F(1)); // refresh the audit rows
    let rows: Vec<_> = d
        .shell
        .audit
        .rows
        .iter()
        .filter(|r| r.action == "patch.comment")
        .collect();
    assert_eq!(rows.len(), 2, "refused comments leave no trace");
    assert_eq!(rows[0].actor, "owner");
    assert_eq!(
        rows[0].target,
        "src/tax.rs:2 rounds half up, please add a test"
    );
    assert_eq!(
        rows[1].target.chars().count(),
        "src/tax.rs:3 ".len() + mouli_device::MAX_COMMENT_CHARS
    );
    assert_eq!(d.shell.audit.chain, Chain::Verified);
}
