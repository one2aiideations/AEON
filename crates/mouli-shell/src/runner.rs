//! The loop that draws the interface, waits for keys and reports what the person chose.

use mouli_screen::grid::Grid;
use mouli_screen::key::Key;
use mouli_screen::raster::{render, Canvas, RasterCfg};
use mouli_screen::screen::Action;
use mouli_screen::shell::Shell;
use std::io;
use std::time::Duration;

/// Somewhere to show pictures.
pub trait Display {
    /// The screen size in pixels.
    fn size(&self) -> (usize, usize);
    /// Shows a picture.
    ///
    /// # Errors
    /// When the picture cannot be shown.
    fn present(&mut self, canvas: &Canvas) -> io::Result<()>;
}

/// Somewhere keys come from.
pub trait Keys {
    /// Waits up to `timeout` and returns the keys pressed (possibly none).
    ///
    /// # Errors
    /// When the input is gone (for example the terminal closed).
    fn next(&mut self, timeout: Duration) -> io::Result<Vec<Key>>;
}

impl Display for crate::fb::Framebuffer {
    fn size(&self) -> (usize, usize) {
        let i = self.info();
        (i.width, i.height)
    }

    fn present(&mut self, canvas: &Canvas) -> io::Result<()> {
        crate::fb::Framebuffer::present(self, canvas)
            .map(|_| ())
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

#[cfg(target_os = "linux")]
impl<F: std::os::fd::AsFd> Keys for crate::tty::TtyKeys<F> {
    fn next(&mut self, timeout: Duration) -> io::Result<Vec<Key>> {
        crate::tty::TtyKeys::next(self, timeout)
    }
}

/// What to do after an action was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Keep running.
    Continue,
    /// Stop the loop.
    Exit,
}

/// How long to wait for a key before running the idle hook and redrawing.
pub const IDLE: Duration = Duration::from_millis(500);

/// Runs the interface until `on_action` asks to exit or the input closes.
///
/// Each time round it draws `shell` for the display's size, shows it, and waits for keys. Every key
/// goes to the shell; any action the shell reports (other than [`Action::None`], and other than
/// switching screens, which the shell handles itself) is passed to `on_action`, which may change
/// the shell, for example to add the model's reply to the chat. When no key arrives within
/// [`IDLE`], `on_idle` runs (for the clock, network status, a finished model reply) and the screen
/// is redrawn.
///
/// # Errors
/// Display or input errors. A closed input ends the loop with `Ok`.
pub fn run<D, K, A, I>(
    shell: &mut Shell,
    display: &mut D,
    keys: &mut K,
    cfg: RasterCfg,
    mut on_action: A,
    mut on_idle: I,
) -> io::Result<()>
where
    D: Display,
    K: Keys,
    A: FnMut(Action, &mut Shell) -> Control,
    I: FnMut(&mut Shell),
{
    loop {
        let (w, h) = display.size();
        let (cols, rows) = cfg.grid_for(w, h);
        let mut grid = Grid::new(cols, rows);
        shell.draw(&mut grid);
        display.present(&render(&grid, cfg))?;
        let pressed = match keys.next(IDLE) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if pressed.is_empty() {
            on_idle(shell);
            continue;
        }
        for key in pressed {
            match shell.on_key(key) {
                Action::None | Action::Show(_) => {}
                action => {
                    if on_action(action, shell) == Control::Exit {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouli_screen::screen::Tab;
    use mouli_screen::screens::Role;

    struct Mem {
        size: (usize, usize),
        frames: Vec<Canvas>,
    }

    impl Display for Mem {
        fn size(&self) -> (usize, usize) {
            self.size
        }
        fn present(&mut self, c: &Canvas) -> io::Result<()> {
            self.frames.push(c.clone());
            Ok(())
        }
    }

    struct Script(Vec<io::Result<Vec<Key>>>);

    impl Keys for Script {
        fn next(&mut self, _t: Duration) -> io::Result<Vec<Key>> {
            if self.0.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "end of script",
                ));
            }
            self.0.remove(0)
        }
    }

    fn typed(s: &str) -> io::Result<Vec<Key>> {
        Ok(s.chars().map(Key::Char).collect())
    }

    fn cfg() -> RasterCfg {
        RasterCfg {
            scale: 1,
            line_gap: 0,
        }
    }

    #[test]
    fn typing_a_message_runs_the_handler_and_shows_the_reply() {
        let mut shell = Shell::new();
        let mut display = Mem {
            size: (640, 400),
            frames: Vec::new(),
        };
        let mut keys = Script(vec![
            Ok(vec![Key::F(1)]),
            typed("hello"),
            Ok(vec![Key::Enter]),
        ]);
        let mut seen = Vec::new();
        run(
            &mut shell,
            &mut display,
            &mut keys,
            cfg(),
            |a, shell| {
                if let Action::Send(text) = &a {
                    shell.chat.push(Role::User, text);
                    shell.chat.push(Role::Assistant, "hi there");
                }
                seen.push(a);
                Control::Continue
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(seen, vec![Action::Send("hello".into())]);
        assert_eq!(shell.chat.messages.len(), 2);
        // Draw once per wait: before F1, after F1, after typing, after Enter (then the script ends).
        assert_eq!(display.frames.len(), 4);
        assert_eq!(
            (display.frames[0].width, display.frames[0].height),
            (640, 400)
        );
    }

    #[test]
    fn the_grid_follows_the_display_size() {
        let mut shell = Shell::new();
        let mut a = Mem {
            size: (640, 400),
            frames: Vec::new(),
        };
        run(
            &mut shell,
            &mut a,
            &mut Script(vec![]),
            cfg(),
            |_, _| Control::Exit,
            |_| {},
        )
        .unwrap();
        let mut b = Mem {
            size: (1024, 768),
            frames: Vec::new(),
        };
        run(
            &mut shell,
            &mut b,
            &mut Script(vec![]),
            cfg(),
            |_, _| Control::Exit,
            |_| {},
        )
        .unwrap();
        assert_eq!((a.frames[0].width, a.frames[0].height), (640, 400));
        assert_eq!((b.frames[0].width, b.frames[0].height), (1024, 768));
    }

    #[test]
    fn the_handler_can_stop_the_loop() {
        let mut shell = Shell::new();
        let mut display = Mem {
            size: (320, 200),
            frames: Vec::new(),
        };
        let mut keys = Script(vec![Ok(vec![Key::Char('q')]), Ok(vec![Key::Char('x')])]);
        let mut calls = 0;
        run(
            &mut shell,
            &mut display,
            &mut keys,
            cfg(),
            |a, _| {
                calls += 1;
                assert_eq!(a, Action::Quit);
                Control::Exit
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(
            keys.0.len(),
            1,
            "the loop stopped before reading more input"
        );
    }

    #[test]
    fn idle_time_runs_the_hook_and_redraws() {
        let mut shell = Shell::new();
        let mut display = Mem {
            size: (1024, 400),
            frames: Vec::new(),
        };
        let mut keys = Script(vec![Ok(vec![]), Ok(vec![]), Ok(vec![Key::Char('q')])]);
        let mut idles = 0;
        run(
            &mut shell,
            &mut display,
            &mut keys,
            cfg(),
            |_, _| Control::Exit,
            |s| {
                idles += 1;
                s.status = format!("tick {idles}");
            },
        )
        .unwrap();
        assert_eq!(idles, 2);
        assert_eq!(shell.status, "tick 2");
        assert_eq!(display.frames.len(), 3);
        assert_ne!(
            display.frames[0], display.frames[2],
            "the status change was drawn"
        );
    }

    #[test]
    fn screen_switches_are_handled_by_the_shell_not_the_handler() {
        let mut shell = Shell::new();
        let mut display = Mem {
            size: (640, 400),
            frames: Vec::new(),
        };
        let mut keys = Script(vec![Ok(vec![Key::F(1), Key::F(4)])]);
        let mut calls = 0;
        run(
            &mut shell,
            &mut display,
            &mut keys,
            cfg(),
            |_, _| {
                calls += 1;
                Control::Continue
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(calls, 0);
        assert_eq!(shell.active, Tab::Audit);
    }

    #[test]
    fn a_closed_input_ends_the_loop_cleanly_and_other_errors_are_returned() {
        let mut shell = Shell::new();
        let mut display = Mem {
            size: (320, 200),
            frames: Vec::new(),
        };
        assert!(run(
            &mut shell,
            &mut display,
            &mut Script(vec![]),
            cfg(),
            |_, _| Control::Continue,
            |_| {}
        )
        .is_ok());
        let mut broken = Script(vec![Err(io::Error::other("keyboard unplugged"))]);
        let err = run(
            &mut shell,
            &mut display,
            &mut broken,
            cfg(),
            |_, _| Control::Continue,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "keyboard unplugged");
    }

    #[test]
    fn a_display_error_is_returned() {
        struct Dead;
        impl Display for Dead {
            fn size(&self) -> (usize, usize) {
                (320, 200)
            }
            fn present(&mut self, _: &Canvas) -> io::Result<()> {
                Err(io::Error::other("no screen"))
            }
        }
        let mut shell = Shell::new();
        let err = run(
            &mut shell,
            &mut Dead,
            &mut Script(vec![]),
            cfg(),
            |_, _| Control::Continue,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "no screen");
    }

    #[test]
    fn a_screen_too_small_for_any_cells_still_works() {
        let mut shell = Shell::new();
        let mut display = Mem {
            size: (5, 5),
            frames: Vec::new(),
        };
        run(
            &mut shell,
            &mut display,
            &mut Script(vec![]),
            cfg(),
            |_, _| Control::Continue,
            |_| {},
        )
        .unwrap();
        assert_eq!((display.frames[0].width, display.frames[0].height), (0, 0));
    }
}
