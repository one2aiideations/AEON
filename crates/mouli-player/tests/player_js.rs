//! Runs the YouTube page's script against a fake YouTube player (needs `node`; skipped without it).

use std::process::Command;

#[test]
fn the_page_script_does_what_the_engine_says_and_reports_what_the_player_reports() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("node is not installed: skipping the page script test");
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("node")
        .arg(format!("{dir}/tests/player_js_harness.js"))
        .arg(format!("{dir}/src/player.js"))
        .output()
        .expect("node runs");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
