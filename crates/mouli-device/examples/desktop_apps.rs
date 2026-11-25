//! Drives a device through its keys (the Sheet and Studio applications, then a held request) and
//! draws the result on the desktop: `cargo run -p mouli-device --example desktop_apps -- <dir>`.

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

fn type_keys(d: &mut Device<Echo>, s: &str) {
    for c in s.chars() {
        d.on_key(Key::Char(c));
    }
}

fn main() -> std::io::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir)?;
    let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_role(Permissions::new("clerk", &["sheet.*", "studio.*"]));
    ws.add_model(ModelEntry {
        id: "local-small".into(),
        provider: "local".into(),
        local: true,
        caps: BTreeSet::from(["text".to_string()]),
        context: 4096,
        cost_per_1k: 0,
        quality: 50,
    });
    let apps = mouli_apps::register(&mut ws).unwrap();
    // A clerk has asked to erase part of the sheet: that request waits for a second person.
    let alice = ws.open_session("acme", "alice", "clerk").unwrap();
    ws.submit("acme", &alice, "/sheet.set cell=F1 value=scratch")
        .unwrap();
    ws.submit("acme", &alice, "/sheet.clear range=F1:F1")
        .unwrap();

    let mut d = Device::new(ws, Echo, "acme", "owner", "owner").unwrap();
    d.attach_apps(apps);

    d.on_key(Key::F(7));
    // Type a cell, commit it with Enter, move right; at the end of a row go back to column A and
    // down one row.
    let row = |d: &mut Device<Echo>, cells: &[String]| {
        for c in cells {
            // An empty cell is skipped with the arrow key: Enter on it would open an edit.
            if !c.is_empty() {
                type_keys(d, c);
                d.on_key(Key::Enter);
            }
            d.on_key(Key::Right);
        }
        for _ in cells {
            d.on_key(Key::Left);
        }
        d.on_key(Key::Down);
    };
    row(&mut d, &["Item", "Qty", "Price", "Total"].map(String::from));
    for (i, (item, qty, price)) in [("Notebook", 12, 4), ("Pen", 40, 1), ("Folder", 8, 3)]
        .iter()
        .enumerate()
    {
        let r = i + 2;
        row(
            &mut d,
            &[
                item.to_string(),
                qty.to_string(),
                price.to_string(),
                format!("=B{r}*C{r}"),
            ],
        );
    }
    row(
        &mut d,
        &[
            "Total".to_string(),
            String::new(),
            String::new(),
            "=SUM(D2:D4)".to_string(),
        ],
    );
    d.on_key(Key::F(8));
    type_keys(&mut d, "# Supplies order");
    d.on_key(Key::Enter);
    type_keys(
        &mut d,
        "Three items are needed this month. Totals are calculated in the sheet.",
    );
    d.on_key(Key::Enter);

    let mut desk = Desktop::new(1024, 768);
    desk.status = "owner".into();
    desk.open(&mut d.shell, Tab::Approvals);
    desk.open(&mut d.shell, Tab::Studio);
    desk.open(&mut d.shell, Tab::Sheet);
    desk.pointer = (880, 600);
    std::fs::write(dir.join("apps.png"), to_png(&desk.render(&d.shell)))?;
    println!(
        "wrote apps.png; audit records: {}",
        d.workspace().audit().records().len()
    );
    Ok(())
}
