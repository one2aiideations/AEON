//! Runs the real `mouli-ops` binary the way an operator would.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mouli-ops"))
}

fn db() -> String {
    std::env::temp_dir()
        .join(format!(
            "mouli-ops-cli-{}-{}.db",
            std::process::id(),
            mouli_ops::common::new_id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = bin().args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn version_and_usage_exit_codes() {
    let (code, out, _) = run(&["version"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("mouli-ops "));
    let (code, _, err) = run(&[]);
    assert_eq!(code, 2);
    assert!(err.contains("usage:"));
    assert_eq!(run(&["frobnicate"]).0, 2);
    assert_eq!(run(&["demo"]).0, 2);
}

#[test]
fn init_bootstrap_demo_and_report_work_on_a_fresh_file() {
    let d = db();
    assert_eq!(run(&["init", "--db", &d]).0, 0);
    let (code, _, err) = run(&["bootstrap", "acme", "root", "--db", &d]);
    assert_eq!(code, 0, "{err}");
    // A second bootstrap is refused with exit 1, not a panic.
    let (code, _, err) = run(&["bootstrap", "acme", "mallory", "--db", &d]);
    assert_eq!(code, 1);
    assert!(err.contains("already bootstrapped"), "{err}");
    let (code, out, err) = run(&["demo", "acme", "--db", &d]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"items\": 20"), "{out}");
    let (code, _, err) = run(&["demo", "acme", "--db", &d]);
    assert_eq!(code, 1);
    assert!(err.contains("already has demo data"), "{err}");
    let (code, out, err) = run(&["report", "open_orders", "acme", "root", "--db", &d]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"count\": 1"), "{out}");
    let (code, _, err) = run(&["report", "nonsense", "acme", "root", "--db", &d]);
    assert_eq!(code, 1);
    assert!(err.contains("valid reports are"), "{err}");
    let _ = std::fs::remove_file(&d);
}

#[test]
fn commands_work_without_a_separate_init() {
    let d = db();
    assert_eq!(run(&["bootstrap", "acme", "root", "--db", &d]).0, 0);
    assert_eq!(
        run(&["report", "audit_integrity", "acme", "root", "--db", &d]).0,
        0
    );
    let _ = std::fs::remove_file(&d);
}

#[test]
fn the_self_test_command_passes() {
    let (code, out, err) = run(&["test"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"status\": \"PASS\""), "{out}");
}

#[test]
fn serve_refuses_a_remote_address_by_default() {
    let d = db();
    let out = bin()
        .args(["serve", "--bind", "203.0.113.5:9", "--db", &d])
        .env_remove("MOULI_OPS_ALLOW_REMOTE")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to bind"));
    let _ = std::fs::remove_file(&d);
}
