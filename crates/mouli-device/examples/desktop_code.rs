//! Opens a small project in the code editor, makes an edit, sends it for review and draws the
//! desktop: `cargo run -p mouli-device --example desktop_code -- <dir>`.

use mouli_desk::desktop::Desktop;
use mouli_device::{Backend, Device};
use mouli_interface::routing::{ModelEntry, Route};
use mouli_interface::{Permissions, Policy, Workspace};
use mouli_screen::key::Key;
use mouli_screen::raster::to_png;
use mouli_screen::screen::Tab;
use std::collections::BTreeSet;

struct Echo;
impl Backend for Echo {
    fn complete(&mut self, _r: &Route, prompt: &str) -> Result<String, String> {
        Ok(format!("You said: {prompt}"))
    }
}

const LIB: &str = "//! Tax helpers.\n\nuse std::fmt;\n\n/// An amount in cents.\n#[derive(Debug, Clone, Copy)]\npub struct Money(pub i64);\n\nimpl fmt::Display for Money {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        write!(f, \"{}.{:02}\", self.0 / 100, self.0 % 100)\n    }\n}\n\n/// The tax on `base` at `rate` percent, rounded to the nearest cent.\npub fn tax(base: Money, rate: i64) -> Money {\n    let raw = base.0 * rate;\n    Money((raw + 50) / 100) // round half up\n}\n";

fn main() -> std::io::Result<()> {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&out)?;
    let proj = tempfile::tempdir()?;
    std::fs::create_dir_all(proj.path().join("src"))?;
    std::fs::write(proj.path().join("src/lib.rs"), LIB)?;
    std::fs::write(proj.path().join("src/main.rs"), "fn main() {}\n")?;
    std::fs::write(
        proj.path().join("Cargo.toml"),
        "[package]\nname = \"tax\"\nversion = \"0.1.0\"\n\n[dependencies]\n",
    )?;
    std::fs::write(proj.path().join("README.md"), "# Tax\n\nUse `tax()`.\n")?;

    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_model(ModelEntry {
        id: "local-small".into(),
        provider: "local".into(),
        local: true,
        caps: BTreeSet::from(["text".to_string()]),
        context: 4096,
        cost_per_1k: 0,
        quality: 50,
    });
    let code = mouli_apps::register_code(&mut ws).unwrap();
    let mut d = Device::new(ws, Echo, "acme", "owner", "owner").unwrap();
    d.attach_code(code);
    d.open_project(proj.path()).unwrap();

    d.on_key(Key::F(9));
    // The list is sorted: Cargo.toml, README.md, src/lib.rs, src/main.rs.
    d.on_key(Key::Down);
    d.on_key(Key::Down);
    d.on_key(Key::Enter);
    // Go to the line `Money((raw + 50) / 100)` and change the rate type comment.
    for _ in 0..17 {
        d.on_key(Key::Down);
    }
    d.on_key(Key::End);
    for c in " and keep the cents".chars() {
        d.on_key(Key::Char(c));
    }
    d.on_key(Key::Ctrl('s'));

    let mut desk = Desktop::new(1024, 768);
    desk.status = "owner".into();
    desk.open(&mut d.shell, Tab::Review);
    desk.open(&mut d.shell, Tab::Editor);
    desk.pointer = (900, 650);
    std::fs::write(out.join("code.png"), to_png(&desk.render(&d.shell)))?;
    println!("wrote code.png; held change: {:?}", d.shell.review.summary);
    Ok(())
}
