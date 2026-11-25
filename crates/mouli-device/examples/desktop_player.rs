//! Fills the player with a Shorts feed and a few files, starts it, and draws the desktop:
//! `cargo run -p mouli-device --example desktop_player -- <dir>`.

use mouli_desk::desktop::Desktop;
use mouli_device::{Backend, Device};
use mouli_interface::routing::{ModelEntry, Route};
use mouli_interface::{Permissions, Policy, Workspace};
use mouli_player::Event;
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

struct Silent;
impl mouli_player::Backend for Silent {
    fn load(&mut self, _: &mouli_player::Source) -> Result<(), String> {
        Ok(())
    }
    fn pause(&mut self, _: bool) {}
    fn seek(&mut self, _: u32) {}
    fn set_volume(&mut self, _: u8) {}
    fn set_speed(&mut self, _: u16) {}
    fn stop(&mut self) {}
}

fn type_keys(d: &mut Device<Echo>, s: &str) {
    for c in s.chars() {
        d.on_key(Key::Char(c));
    }
}

fn main() -> std::io::Result<()> {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&out)?;
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
    let player = mouli_apps::register_player(&mut ws, Box::new(Silent)).unwrap();
    let mut d = Device::new(ws, Echo, "acme", "owner", "owner").unwrap();
    d.attach_player(player);
    d.on_key(Key::F(10));
    for u in [
        "https://www.youtube.com/shorts/aaaaaaaaaaa",
        "https://www.youtube.com/shorts/bbbbbbbbbbb",
        "https://www.youtube.com/shorts/ccccccccccc",
        "https://youtu.be/dQw4w9WgXcQ",
        "/mouli/data/videos/holiday.mp4",
        "https://cdn.example.org/lecture.webm",
    ] {
        d.on_key(Key::Char('o'));
        type_keys(&mut d, u);
        d.on_key(Key::Enter);
    }
    d.on_key(Key::Char('f')); // Shorts feed on
    d.on_key(Key::Char(' '));
    d.player_event(Event::Loaded(Some(34)));
    d.player_event(Event::Position(13));
    d.on_key(Key::Down);
    d.on_key(Key::Down);

    let mut desk = Desktop::new(1024, 768);
    desk.status = "owner".into();
    desk.open(&mut d.shell, Tab::Player);
    desk.pointer = (900, 650);
    std::fs::write(out.join("player.png"), to_png(&desk.render(&d.shell)))?;
    println!("wrote player.png");
    Ok(())
}
