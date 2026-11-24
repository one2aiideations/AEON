//! Renders each screen with sample data to a PNG: `cargo run -p mouli-screen --example screenshots -- <dir>`.

use mouli_screen::grid::Grid;
use mouli_screen::key::Key;
use mouli_screen::raster::{render, to_png, RasterCfg};
use mouli_screen::screen::Screen;
use mouli_screen::screens::{ModelRow, Role};
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
    Ok(())
}
