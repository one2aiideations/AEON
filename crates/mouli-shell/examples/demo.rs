//! Tries the MOULI screens on this machine's real framebuffer.
//!
//! Run it from a text console (Ctrl-Alt-F3, not inside a desktop session), as a user who may open
//! `/dev/fb0` (root, or in the `video` group):
//!
//! ```text
//! sudo cargo run -p mouli-shell --example demo
//! ```
//!
//! F1 to F4 switch screens. On the Models screen press `q` to leave; the text console is put
//! back. If the colours look wrong (red and blue swapped), set `MOULI_FB_ORDER=rgbx`.

use mouli_screen::raster::RasterCfg;
use mouli_screen::screen::Action;
use mouli_screen::screens::{ApprovalRow, AuditRow, Chain, ModelRow, Role};
use mouli_screen::shell::Shell;
use mouli_shell::console::ConsoleTakeover;
use mouli_shell::fb::{Framebuffer, PixelOrder};
use mouli_shell::runner::{run, Control};
use mouli_shell::tty::{RawTty, TtyKeys};
use std::io::IsTerminal;
use std::path::Path;

fn main() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        eprintln!("This demo needs a terminal on standard input.");
        std::process::exit(2);
    }
    let order = if std::env::var("MOULI_FB_ORDER").is_ok_and(|v| v.eq_ignore_ascii_case("rgbx")) {
        PixelOrder::Rgbx
    } else {
        PixelOrder::Bgrx
    };
    let mut fb = match Framebuffer::open(
        Path::new("/dev/fb0"),
        Path::new("/sys/class/graphics/fb0"),
        order,
    ) {
        Ok(fb) => fb,
        Err(e) => {
            eprintln!("Cannot use the framebuffer: {e}");
            std::process::exit(1);
        }
    };

    let mut shell = Shell::new();
    shell.status = "demo".into();
    shell.models.rows = vec![
        ModelRow {
            name: "tiny-demo-model".into(),
            size_mb: 700,
            arch: Some("llama".into()),
            needs_mb: 900,
            fits: true,
        },
        ModelRow {
            name: "huge-demo-model".into(),
            size_mb: 40_000,
            arch: Some("llama".into()),
            needs_mb: 48_000,
            fits: false,
        },
    ];
    shell.models.budget_mb = 4096;
    shell.models.ctx = 2048;
    shell.approvals.set_rows(vec![ApprovalRow {
        id: "apr-1".into(),
        capability: "mail.send".into(),
        requester: "alice".into(),
        detail: "to=bob@example.com".into(),
        can_decide: true,
        why_not: String::new(),
    }]);
    shell.audit.set(
        vec![AuditRow {
            seq: 1,
            time: "00:00:01".into(),
            actor: "owner".into(),
            action: "session.open".into(),
            target: "-".into(),
            outcome: "ok".into(),
            hash: "0123456789abcdef".into(),
        }],
        Chain::Verified,
    );

    let mut console = ConsoleTakeover::new(Path::new("/sys"));
    let notes = console.engage();
    let _raw = RawTty::enter(&stdin)?;
    let mut keys = TtyKeys::new(stdin);

    let result = run(
        &mut shell,
        &mut fb,
        &mut keys,
        RasterCfg::default(),
        |action, shell| match action {
            Action::Quit => Control::Exit,
            Action::Send(text) => {
                shell.chat.push(Role::User, &text);
                shell.chat.push(Role::Assistant, "(demo) I heard you.");
                Control::Continue
            }
            other => {
                shell.chat.push(Role::Notice, &format!("{other:?}"));
                Control::Continue
            }
        },
        |_| {},
    );
    drop(_raw);
    console.release();
    for n in notes {
        eprintln!("note: {n}");
    }
    result
}
