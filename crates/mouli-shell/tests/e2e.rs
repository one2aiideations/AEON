//! Everything together except a real framebuffer: keys typed on a pseudo-terminal go through the
//! raw-mode reader and the event loop, and the picture goes through the framebuffer writer into a
//! file. The file is then compared byte for byte with an independent rendering of the same screen.

#![cfg(target_os = "linux")]

use mouli_screen::grid::Grid;
use mouli_screen::key::Key;
use mouli_screen::raster::{render, RasterCfg};
use mouli_screen::screen::Action;
use mouli_screen::screens::Role;
use mouli_screen::shell::Shell;
use mouli_shell::fb::{Framebuffer, PixelOrder};
use mouli_shell::runner::{run, Control, Keys};
use mouli_shell::tty::{RawTty, TtyKeys};
use nix::pty::openpty;
use std::fs;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::time::Duration;

/// Wraps the real key reader and ends the session after a number of quiet moments, so the loop
/// stops once the typing has been handled and drawn.
struct EndAfterQuiet {
    inner: TtyKeys<OwnedFd>,
    quiet_left: usize,
}

impl Keys for EndAfterQuiet {
    fn next(&mut self, timeout: Duration) -> io::Result<Vec<Key>> {
        let keys = self.inner.next(timeout)?;
        if keys.is_empty() {
            if self.quiet_left == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "session over"));
            }
            self.quiet_left -= 1;
        }
        Ok(keys)
    }
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!("mouli-e2e-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
}

fn fake_fb(root: &std::path::Path, w: usize, h: usize) -> (std::path::PathBuf, std::path::PathBuf) {
    let sys = root.join("fb0");
    fs::create_dir_all(&sys).unwrap();
    fs::write(sys.join("virtual_size"), format!("{w},{h}\n")).unwrap();
    fs::write(sys.join("bits_per_pixel"), "32\n").unwrap();
    fs::write(sys.join("stride"), format!("{}\n", w * 4)).unwrap();
    let dev = root.join("fb-device");
    fs::write(&dev, vec![0u8; w * h * 4]).unwrap();
    (sys, dev)
}

#[test]
fn typing_in_a_terminal_ends_up_on_the_framebuffer() {
    let root = tmp("chat");
    let (sys, dev) = fake_fb(&root, 1280, 800);
    let mut fb = Framebuffer::open(&dev, &sys, PixelOrder::Bgrx).unwrap();

    let pty = openpty(None, None).unwrap();
    let mut master = fs::File::from(pty.master);
    let _raw = RawTty::enter(&pty.slave).unwrap();
    let mut keys = EndAfterQuiet {
        inner: TtyKeys::new(pty.slave),
        quiet_left: 2,
    };

    // The person presses F1 (Linux console form), types "hello world", presses Enter.
    let writer = std::thread::spawn(move || {
        for chunk in [&b"\x1b[[A"[..], b"hello ", b"world", b"\r"] {
            std::thread::sleep(Duration::from_millis(80));
            master.write_all(chunk).unwrap();
        }
        master // keep the terminal open until the loop has finished
    });

    let mut shell = Shell::new();
    shell.chat.model = "tiny".into();
    let mut sent = Vec::new();
    run(
        &mut shell,
        &mut fb,
        &mut keys,
        RasterCfg::default(),
        |action, shell| {
            if let Action::Send(text) = &action {
                shell.chat.push(Role::User, text);
                shell.chat.push(Role::Assistant, "Hello! How can I help?");
            }
            sent.push(action);
            Control::Continue
        },
        |_| {},
    )
    .unwrap();
    let _master = writer.join().unwrap();

    assert_eq!(sent, vec![Action::Send("hello world".into())]);

    // Build the same screen independently and compare it with the bytes on the "screen".
    let mut expected = Shell::new();
    expected.chat.model = "tiny".into();
    expected.on_key(Key::F(1));
    expected.chat.push(Role::User, "hello world");
    expected
        .chat
        .push(Role::Assistant, "Hello! How can I help?");
    let cfg = RasterCfg::default();
    let (cols, rows) = cfg.grid_for(1280, 800);
    let mut grid = Grid::new(cols, rows);
    expected.draw(&mut grid);
    let want = render(&grid, cfg).to_xrgb8888();
    let got = fs::read(&dev).unwrap();
    assert_eq!(got.len(), want.len());
    assert!(
        got == want,
        "the framebuffer differs from the expected rendering"
    );
    let text = grid.to_text();
    assert!(
        text.contains("hello world") && text.contains("Hello! How can I help?"),
        "{text}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_screen_only_rewrites_what_changed_between_frames() {
    // Two frames that differ in one chat line must touch only the rows around it.
    let root = tmp("diff");
    let (sys, dev) = fake_fb(&root, 1280, 800);
    let mut fb = Framebuffer::open(&dev, &sys, PixelOrder::Bgrx).unwrap();
    let cfg = RasterCfg::default();
    let (cols, rows) = cfg.grid_for(1280, 800);
    let mut shell = Shell::new();
    shell.on_key(Key::F(1));
    let draw = |shell: &Shell| {
        let mut g = Grid::new(cols, rows);
        shell.draw(&mut g);
        render(&g, cfg)
    };
    assert_eq!(fb.present(&draw(&shell)).unwrap(), 800);
    shell.chat.push(Role::User, "one line");
    let rewritten = fb.present(&draw(&shell)).unwrap();
    assert!(rewritten > 0 && rewritten < 800, "{rewritten} of 800 rows");
    assert_eq!(fb.present(&draw(&shell)).unwrap(), 0);
    let _ = fs::remove_dir_all(root);
}
