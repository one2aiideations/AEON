//! Renders the desktop with sample data to PNG files:
//! `cargo run -p mouli-desk --example desktop -- <dir>`.

use mouli_desk::desktop::Desktop;
use mouli_desk::input::{Button, MouseEvent};
use mouli_screen::raster::to_png;
use mouli_screen::screen::Tab;
use mouli_screen::screens::{ApprovalRow, AuditRow, Chain, Finding, Role, Severity};
use mouli_screen::shell::Shell;
use std::path::PathBuf;

fn save(d: &Desktop, shell: &Shell, dir: &std::path::Path, name: &str) -> std::io::Result<()> {
    let png = to_png(&d.render(shell));
    std::fs::write(dir.join(format!("{name}.png")), png)?;
    println!("wrote {name}.png");
    Ok(())
}

fn main() -> std::io::Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .map(Into::into)
        .unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir)?;
    let mut shell = Shell::new();
    shell
        .chat
        .push(Role::User, "Summarise the open approvals for me.");
    shell.chat.push(
        Role::Assistant,
        "Two requests are waiting: alice wants to send a mail to bob, and carol wants to pay ACME-LTD 1,250,000. Neither can be approved by the person who asked.",
    );
    shell.chat.push(
        Role::Notice,
        "Held for approval (apr-000002). Someone else must approve it.",
    );
    shell.approvals.set_rows(vec![
        ApprovalRow {
            id: "apr-000001".into(),
            capability: "mail.send".into(),
            requester: "alice".into(),
            detail: "to=bob@example.com subject=Invoice".into(),
            can_decide: true,
            why_not: String::new(),
        },
        ApprovalRow {
            id: "apr-000002".into(),
            capability: "pay.send".into(),
            requester: "owner".into(),
            detail: "payee=ACME-LTD amount=1250000".into(),
            can_decide: false,
            why_not: "You asked for this yourself; someone else must decide.".into(),
        },
    ]);
    shell.security.set(
        68,
        vec![
            Finding {
                id: "case-7".into(),
                severity: Severity::Critical,
                title: "Sign-in after repeated failures".into(),
                detail: "Five failed sign-ins within one minute, then a success.".into(),
                acknowledged: false,
            },
            Finding {
                id: "case-3".into(),
                severity: Severity::Low,
                title: "New device signed in".into(),
                detail: "A laptop not seen before.".into(),
                acknowledged: true,
            },
        ],
    );
    shell.audit.set(
        vec![AuditRow {
            seq: 1,
            time: "09:00:02".into(),
            actor: "owner".into(),
            action: "session.open".into(),
            target: "ses-000001".into(),
            outcome: "ok".into(),
            hash: "9e3779b97f4a7c15".into(),
        }],
        Chain::Verified,
    );

    let mut d = Desktop::new(1024, 768);
    d.status = "owner".into();
    d.open(&mut shell, Tab::Security);
    d.open(&mut shell, Tab::Approvals);
    d.open(&mut shell, Tab::Chat);
    d.pointer = (560, 420);
    save(&d, &shell, &dir, "desktop")?;

    // Open the View menu by pressing on its title, with the pointer over the Audit item.
    let view = d.menu.title_rects()[3];
    d.pointer = (view.x + 10, 8);
    d.on_mouse(&mut shell, MouseEvent::Down(Button::Left), 0);
    let items = d.menu.item_rects(3);
    let (ix, iy) = (items[3].x + 30, items[3].y + 8);
    d.on_mouse(
        &mut shell,
        MouseEvent::Move {
            dx: ix - d.pointer.0,
            dy: iy - d.pointer.1,
        },
        10,
    );
    save(&d, &shell, &dir, "menu")?;
    d.on_mouse(&mut shell, MouseEvent::Up(Button::Left), 20);

    // The About box.
    d.pointer = (300, 300);
    let about = d.menu.title_rects()[0];
    d.pointer = (about.x + 10, 8);
    d.on_mouse(&mut shell, MouseEvent::Down(Button::Left), 30);
    d.on_mouse(&mut shell, MouseEvent::Up(Button::Left), 31);
    let first = d.menu.item_rects(0)[0];
    let (ax, ay) = (first.x + 20, first.y + 8);
    d.on_mouse(
        &mut shell,
        MouseEvent::Move {
            dx: ax - d.pointer.0,
            dy: ay - d.pointer.1,
        },
        40,
    );
    d.on_mouse(&mut shell, MouseEvent::Down(Button::Left), 50);
    d.on_mouse(&mut shell, MouseEvent::Up(Button::Left), 51);
    save(&d, &shell, &dir, "about")
}
