//! Renders each screen with sample data to a PNG: `cargo run -p mouli-screen --example screenshots -- <dir>`.

use mouli_screen::grid::Grid;
use mouli_screen::key::Key;
use mouli_screen::raster::{render, to_png, RasterCfg};
use mouli_screen::screen::Screen;
use mouli_screen::screens::{
    ApprovalRow, AuditRow, Chain, Check, FileChange, Finding, ModelRow, Role, Severity,
};
use mouli_screen::shell::Shell;
use std::path::PathBuf;

fn shot(shell: &Shell, dir: &std::path::Path, name: &str) -> std::io::Result<()> {
    let cfg = RasterCfg::default();
    let (cols, rows) = cfg.grid_for(1280, 800);
    let mut grid = Grid::new(cols, rows);
    shell.draw(&mut grid);
    let png = to_png(&render(&grid, cfg));
    std::fs::write(dir.join(format!("{name}.png")), png)?;
    println!("wrote {name}.png ({cols}x{rows} cells)");
    Ok(())
}

fn main() -> std::io::Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    std::fs::create_dir_all(&dir)?;
    let mut shell = Shell::new();
    shell.status = "eth0 10.0.2.15   RAM 1.6 GB free".into();
    let row = |name: &str, size: u64, needs: u64, fits: bool, arch: &str| ModelRow {
        name: name.into(),
        size_mb: size,
        arch: Some(arch.into()),
        needs_mb: needs,
        fits,
    };
    shell.models.rows = vec![
        row("qwen2.5-1.5b-instruct-q4_k_m", 1100, 1500, true, "qwen2"),
        row("llama-3.2-3b-instruct-q4_k_m", 2000, 2600, true, "llama"),
        row("mistral-7b-instruct-q4_k_m", 4400, 5300, false, "llama"),
        row("llama-3.1-8b-instruct-q8_0", 8500, 9900, false, "llama"),
    ];
    shell.models.budget_mb = 4096;
    shell.models.ctx = 2048;
    shell.models.on_key(Key::Down);
    shot(&shell, &dir, "models")?;

    shell.on_key(Key::F(1));
    shell.chat.model = "llama-3.2-3b-instruct-q4_k_m".into();
    shell.chat.push(
        Role::User,
        "Summarise what a unified diff is in two sentences.",
    );
    shell.chat.push(Role::Assistant, "A unified diff is a text format that lists the lines removed from and added to a file, with a few unchanged lines around each change for context. Tools like git and patch use it to show and apply changes.");
    shell.chat.push(
        Role::User,
        "/security.score likelihood=4 impact=5 velocity=2 exposure=3",
    );
    shell.chat.push(
        Role::Tool,
        "security.score: inherent 76.47, residual 76.47 => High",
    );
    shell.chat.push(
        Role::Notice,
        "shell was refused: role owner lacks permission shell",
    );
    shell.chat.busy = true;
    for ch in "Now explain how to roll one back".chars() {
        shell.chat.on_key(Key::Char(ch));
    }
    shot(&shell, &dir, "chat")?;

    shell.on_key(Key::F(3));
    let apr = |id: &str, cap: &str, who: &str, detail: &str, can: bool| ApprovalRow {
        id: id.into(),
        capability: cap.into(),
        requester: who.into(),
        detail: detail.into(),
        can_decide: can,
        why_not: if can {
            String::new()
        } else {
            "You asked for this yourself; someone else must decide it.".into()
        },
    };
    shell.approvals.set_rows(vec![
        apr(
            "apr-0007",
            "mail.send",
            "alice",
            "to=finance@example.com subject=\"March invoice run\" attachments=3",
            true,
        ),
        apr(
            "apr-0008",
            "files.delete",
            "owner",
            "path=/data/reports/2025 recursive=true",
            false,
        ),
        apr(
            "apr-0009",
            "pay.send",
            "carol",
            "payee=ACME-LTD amount=1250000 currency=USD",
            true,
        ),
    ]);
    shell.approvals.on_key(Key::Down);
    shot(&shell, &dir, "approvals")?;

    shell.on_key(Key::F(4));
    let rec = |seq: u64, t: &str, who: &str, act: &str, out: &str| AuditRow {
        seq,
        time: t.into(),
        actor: who.into(),
        action: act.into(),
        target: if act.contains('.') {
            format!("{act}-{seq}")
        } else {
            "-".into()
        },
        outcome: out.into(),
        hash: format!("{:016x}", seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    };
    shell.audit.set(
        vec![
            rec(1, "09:00:02", "owner", "session.open", "ok"),
            rec(2, "09:00:41", "owner", "chat", "ok via llama-3.2-3b"),
            rec(3, "09:02:10", "owner", "security.score", "ok"),
            rec(
                4,
                "09:03:55",
                "owner",
                "shell",
                "denied: role owner lacks permission shell",
            ),
            rec(5, "09:07:18", "alice", "mail.send", "pending approval"),
            rec(6, "09:08:30", "bob", "files.delete", "failed: no such file"),
            rec(7, "09:10:12", "carol", "pay.send", "pending approval"),
            rec(8, "09:12:47", "owner", "chat", "ok via llama-3.2-3b"),
        ],
        Chain::Verified,
    );
    shell.audit.on_key(Key::Down);
    shot(&shell, &dir, "audit")?;

    shell.on_key(Key::F(5));
    let fnd = |id: &str, sev: Severity, title: &str, detail: &str, seen: bool| Finding {
        id: id.into(),
        severity: sev,
        title: title.into(),
        detail: detail.into(),
        acknowledged: seen,
    };
    shell.security.set(
        68,
        vec![
            fnd("case-3", Severity::Low, "New device signed in", "A laptop not seen before signed in from the office network.", true),
            fnd("case-7", Severity::Critical, "Sign-in after repeated failures", "Five failed sign-ins for owner from 203.0.113.9 within one minute, then a success. Change the password and review the audit log.", false),
            fnd("case-5", Severity::High, "Large payment held", "pay.send of 1,250,000 USD to a payee not used before is waiting for approval.", false),
        ],
    );
    shell.security.on_key(Key::Down);
    shot(&shell, &dir, "security")?;

    shell.on_key(Key::F(6));
    let diff = |lines: &[&str]| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    shell.review.set(
        "Round tax per line, not per invoice",
        vec![
            FileChange {
                path: "crates/mouli-ops/src/sales.rs".into(),
                added: 4,
                removed: 2,
                skipped: vec![false],
                diff: diff(&[
                    "--- a/crates/mouli-ops/src/sales.rs",
                    "+++ b/crates/mouli-ops/src/sales.rs",
                    "@@ -41,7 +41,9 @@ fn line_tax(line: &Line) -> i64 {",
                    "     let base = line.qty * line.unit_price;",
                    "-    let tax = base * rate / 100;",
                    "-    tax",
                    "+    // Round half up, per line, in minor units.",
                    "+    let tax = (base * rate + 50) / 100;",
                    "+    debug_assert!(tax >= 0);",
                    "+    tax",
                ]),
            },
            FileChange {
                path: "crates/mouli-ops/tests/tax.rs".into(),
                added: 12,
                removed: 0,
                skipped: vec![false],
                diff: diff(&[
                    "+++ b/crates/mouli-ops/tests/tax.rs",
                    "+#[test]",
                    "+fn rounds_half_up() {}",
                ]),
            },
        ],
        vec![
            Check {
                name: "tests".into(),
                passed: true,
            },
            Check {
                name: "lint".into(),
                passed: true,
            },
            Check {
                name: "no secrets".into(),
                passed: true,
            },
        ],
    );
    shot(&shell, &dir, "review")?;
    Ok(())
}
