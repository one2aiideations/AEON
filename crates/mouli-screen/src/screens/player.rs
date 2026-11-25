//! The video and YouTube player: what is playing, how far, the settings and the queue.
//!
//! Like the other application screens this one only shows a copy of the player's state and asks for
//! things to be done as `Action::Call` (`player.play`, `player.add` ...), so every key goes through
//! the same governed gate as a typed command. Nothing here plays anything.

use crate::grid::{theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// Longest address the input line takes, in characters.
const MAX_INPUT: usize = 2048;
/// Seconds moved by the arrow keys.
const SEEK_STEP: i32 = 5;
/// Percent moved by the volume keys.
const VOLUME_STEP: i32 = 5;

/// One queue row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRow {
    /// The name to show.
    pub title: String,
    /// `short`, `video`, `file` or `link`.
    pub kind: String,
    /// True for what is playing.
    pub current: bool,
}

/// A copy of the player's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerView {
    /// `idle`, `loading`, `playing`, `paused`, `ended` or `failed`.
    pub state: String,
    /// Why it failed.
    pub failure: Option<String>,
    /// What is playing.
    pub now: Option<String>,
    /// Seconds in.
    pub position: u32,
    /// Length, when known.
    pub duration: Option<u32>,
    /// Volume, 0 to 100.
    pub volume: u8,
    /// True when muted.
    pub muted: bool,
    /// Speed in percent.
    pub speed: u16,
    /// Next item starts by itself.
    pub autoplay: bool,
    /// Hands-free Shorts.
    pub shorts: bool,
    /// Shuffle.
    pub shuffle: bool,
    /// `off`, `one` or `all`.
    pub repeat: String,
    /// The queue.
    pub queue: Vec<PlayerRow>,
}

impl Default for PlayerView {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            failure: None,
            now: None,
            position: 0,
            duration: None,
            volume: 80,
            muted: false,
            speed: 100,
            autoplay: true,
            shorts: false,
            shuffle: false,
            repeat: "off".into(),
            queue: Vec::new(),
        }
    }
}

/// The player screen.
#[derive(Debug, Clone, Default)]
pub struct PlayerScreen {
    /// The player's state, set by the controller.
    pub view: PlayerView,
    cursor: Cursor,
    adding: Option<Vec<char>>,
    /// A message for the person.
    pub message: String,
}

fn clock(secs: u32) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

fn call(cap: &str, args: &[(&str, String)]) -> Action {
    Action::Call {
        capability: cap.into(),
        args: args
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
    }
}

fn flag(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

impl PlayerScreen {
    /// Replaces the copy of the player's state.
    pub fn set_view(&mut self, v: PlayerView) {
        self.view = v;
        self.cursor.clamp(self.view.queue.len());
    }

    /// The text typed on the input line, when it is open.
    pub fn input(&self) -> Option<String> {
        self.adding.as_ref().map(|c| c.iter().collect())
    }

    fn key_when_adding(&mut self, key: Key) -> Action {
        let Some(buf) = self.adding.as_mut() else {
            return Action::None;
        };
        match key {
            Key::Esc => self.adding = None,
            Key::Char(c) if buf.len() < MAX_INPUT => buf.push(c),
            Key::Backspace => {
                buf.pop();
            }
            Key::Ctrl('u') => buf.clear(),
            Key::Enter => {
                let url: String = buf.iter().collect::<String>().trim().to_string();
                self.adding = None;
                if !url.is_empty() {
                    self.message.clear();
                    return call("player.add", &[("url", url)]);
                }
            }
            _ => {}
        }
        Action::None
    }
}

impl Screen for PlayerScreen {
    fn on_key(&mut self, key: Key) -> Action {
        if self.adding.is_some() {
            return self.key_when_adding(key);
        }
        let v = &self.view;
        let len = v.queue.len();
        match key {
            Key::Char(' ') => {
                if matches!(v.state.as_str(), "idle" | "ended" | "failed") {
                    call("player.play", &[])
                } else {
                    call("player.pause", &[])
                }
            }
            Key::Char('n') => call("player.next", &[]),
            Key::Char('p') => call("player.prev", &[]),
            Key::Right => call("player.seek", &[("by", SEEK_STEP.to_string())]),
            Key::Left => call("player.seek", &[("by", (-SEEK_STEP).to_string())]),
            Key::Char('+' | '=') => call("player.volume", &[("by", VOLUME_STEP.to_string())]),
            Key::Char('-' | '_') => call("player.volume", &[("by", (-VOLUME_STEP).to_string())]),
            Key::Char('m') => call("player.mute", &[]),
            Key::Char(']') => call("player.speed", &[("by", "1".into())]),
            Key::Char('[') => call("player.speed", &[("by", "-1".into())]),
            Key::Char('s') => call("player.shuffle", &[("on", (!v.shuffle).to_string())]),
            Key::Char('a') => call("player.autoplay", &[("on", (!v.autoplay).to_string())]),
            Key::Char('f') => call("player.shorts", &[("on", (!v.shorts).to_string())]),
            Key::Char('r') => {
                let next = match v.repeat.as_str() {
                    "off" => "all",
                    "all" => "one",
                    _ => "off",
                };
                call("player.repeat", &[("mode", next.into())])
            }
            Key::Char('u') => call("player.rate", &[("verdict", "up".into())]),
            Key::Char('d') => call("player.rate", &[("verdict", "down".into())]),
            Key::Char('x') => call("player.rate", &[("verdict", "report".into())]),
            Key::Char('c') => call("player.clear", &[]),
            Key::Char('o') => {
                self.adding = Some(Vec::new());
                Action::None
            }
            Key::Up => {
                self.cursor.step(-1, len);
                Action::None
            }
            Key::Down => {
                self.cursor.step(1, len);
                Action::None
            }
            Key::PageUp => {
                self.cursor.step(-10, len);
                Action::None
            }
            Key::PageDown => {
                self.cursor.step(10, len);
                Action::None
            }
            Key::Home => {
                self.cursor.jump(false, len);
                Action::None
            }
            Key::End => {
                self.cursor.jump(true, len);
                Action::None
            }
            Key::Enter if len > 0 => call(
                "player.jump",
                &[("index", self.cursor.selected.to_string())],
            ),
            Key::Delete if len > 0 => call(
                "player.remove",
                &[("index", self.cursor.selected.to_string())],
            ),
            _ => Action::None,
        }
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        let v = &self.view;
        g.fill(area, theme::BG);
        let state_colour = match v.state.as_str() {
            "playing" => theme::OK,
            "failed" => theme::BAD,
            "paused" | "loading" => theme::WARN,
            _ => theme::DIM,
        };
        g.boxed(
            area,
            Some(&format!("Player  {}", v.state)),
            theme::ACCENT,
            theme::BG,
        );
        let inner = area.inset(1);
        if inner.w < 24 || inner.h < 7 {
            return;
        }
        let x = inner.x + 1;
        let w = inner.w - 2;
        // Now playing.
        let now = v.now.clone().unwrap_or_else(|| match &v.failure {
            Some(f) => format!("Stopped: {f}"),
            None => {
                "Nothing is playing. Press o to add a link or file, Space to start.".to_string()
            }
        });
        let fg = if v.now.is_some() {
            theme::TEXT
        } else {
            state_colour
        };
        g.text_fit(x, inner.y, w, &now, fg, theme::BG);
        // Progress.
        let times = match v.duration {
            Some(d) => format!("{} / {}", clock(v.position), clock(d)),
            None => clock(v.position),
        };
        let bar_w = w.saturating_sub(times.chars().count() + 2);
        if bar_w >= 8 {
            match v.duration {
                Some(d) if d > 0 => g.bar(
                    (x, inner.y + 1),
                    bar_w,
                    u64::from(v.position.min(d)),
                    u64::from(d),
                    state_colour,
                    theme::BG,
                ),
                _ => g.bar((x, inner.y + 1), bar_w, 0, 1, state_colour, theme::BG),
            }
            g.text(x + bar_w + 2, inner.y + 1, &times, theme::DIM, theme::BG);
        } else {
            g.text_fit(x, inner.y + 1, w, &times, theme::DIM, theme::BG);
        }
        // Settings.
        // Short labels when the window is narrow, so nothing is cut off.
        let full = w >= 78;
        let (vol, auto) = if full {
            ("Volume", "Autoplay")
        } else {
            ("Vol", "Auto")
        };
        let sound = if v.muted {
            "muted".to_string()
        } else {
            format!("{}%", v.volume)
        };
        let settings = format!(
            "{vol} {sound}  Speed {}.{:02}x  Shorts {}  {auto} {}  Shuffle {}  Repeat {}",
            v.speed / 100,
            v.speed % 100,
            flag(v.shorts),
            flag(v.autoplay),
            flag(v.shuffle),
            v.repeat
        );
        g.text_fit(x, inner.y + 2, w, &settings, theme::DIM, theme::BG);
        if v.shorts {
            g.text(
                x,
                inner.y + 3,
                "Shorts feed: the next Short starts by itself.",
                theme::ACCENT,
                theme::BG,
            );
        }
        // Queue.
        let top = inner.y + 4;
        g.text(
            x,
            top,
            &format!("Queue ({})", v.queue.len()),
            theme::ACCENT,
            theme::BG,
        );
        let input_rows = usize::from(self.adding.is_some()) * 2;
        let list_h = (inner.y + inner.h).saturating_sub(top + 1 + input_rows);
        if v.queue.is_empty() && list_h > 0 {
            g.text(x + 2, top + 1, "empty", theme::DIM, theme::BG);
        }
        let first = self.cursor.window(list_h);
        for (row, (i, r)) in v
            .queue
            .iter()
            .enumerate()
            .skip(first)
            .take(list_h)
            .enumerate()
        {
            let selected = i == self.cursor.selected;
            let bg = if selected { theme::SELECT } else { theme::BG };
            let mark = if r.current { '>' } else { ' ' };
            let line = format!("{mark} {i:>3}  [{:<5}] {}", r.kind, r.title);
            g.text_fit(
                x,
                top + 1 + row,
                w,
                &line,
                if r.current {
                    theme::ACCENT
                } else {
                    theme::TEXT
                },
                bg,
            );
        }
        // Input line.
        if let Some(buf) = &self.adding {
            let by = inner.y + inner.h - 2;
            g.text(
                x,
                by,
                "Add a YouTube link, a file path or a media address:",
                theme::DIM,
                theme::BG,
            );
            let typed: String = buf.iter().collect();
            let tail: String = typed
                .chars()
                .rev()
                .take(w.saturating_sub(4))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            g.text(x, by + 1, ">", theme::ACCENT, theme::BG);
            let used = g.text(x + 2, by + 1, &tail, theme::TEXT, theme::BG);
            g.text(x + 2 + used, by + 1, "_", theme::ACCENT, theme::BG);
        }
        if !self.message.is_empty() {
            g.text_fit(
                area.x + 2,
                area.y + area.h - 1,
                area.w.saturating_sub(4),
                &format!(" {} ", self.message),
                theme::WARN,
                theme::BG,
            );
        }
    }

    fn hints(&self) -> String {
        if self.adding.is_some() {
            "Enter add  Esc cancel".into()
        } else {
            "Space play/pause  n/p next/prev  Left/Right seek  +/- volume  o add  f Shorts feed  Enter play  Del remove".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(t: &str, k: &str, cur: bool) -> PlayerRow {
        PlayerRow {
            title: t.into(),
            kind: k.into(),
            current: cur,
        }
    }

    fn view(state: &str) -> PlayerView {
        PlayerView {
            state: state.into(),
            now: (state == "playing").then(|| "Cat Short".to_string()),
            position: 12,
            duration: Some(31),
            queue: vec![
                row("Cat Short", "short", true),
                row("film.mkv", "file", false),
                row("Talk", "video", false),
            ],
            ..PlayerView::default()
        }
    }

    fn screen(state: &str) -> PlayerScreen {
        let mut s = PlayerScreen::default();
        s.set_view(view(state));
        s
    }

    fn drawn(s: &PlayerScreen, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let a = g.area();
        s.draw(&mut g, a);
        g.to_text()
    }

    fn cap(a: &Action) -> (String, Vec<(String, String)>) {
        match a {
            Action::Call { capability, args } => (capability.clone(), args.clone()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn it_shows_what_is_playing_the_progress_the_settings_and_the_queue() {
        let mut s = screen("playing");
        s.view.shorts = true;
        let t = drawn(&s, 100, 20);
        assert!(
            t.contains("Player  playing") && t.contains("Cat Short"),
            "{t}"
        );
        assert!(
            t.contains("0:12 / 0:31") && t.contains('\u{2588}') && t.contains('\u{2591}'),
            "{t}"
        );
        assert!(
            t.contains("Volume 80%")
                && t.contains("Speed 1.00x")
                && t.contains("Shorts on")
                && t.contains("Autoplay on")
                && t.contains("Repeat off"),
            "{t}"
        );
        assert!(t.contains("the next Short starts by itself"));
        assert!(
            t.contains("Queue (3)")
                && t.contains(">   0  [short] Cat Short")
                && t.contains("[file ] film.mkv")
                && t.contains("[video] Talk"),
            "{t}"
        );
    }

    #[test]
    fn a_narrow_window_uses_short_labels_so_the_settings_are_not_cut_off() {
        let mut s = screen("playing");
        s.view.shuffle = true;
        s.view.repeat = "all".into();
        // 72 columns is the default window width on the desktop (70 inside the frame).
        let t = drawn(&s, 72, 16);
        assert!(
            t.contains("Vol 80%")
                && t.contains("Auto on")
                && t.contains("Shuffle on")
                && t.contains("Repeat all"),
            "{t}"
        );
        assert!(!t.contains(".."), "nothing is cut off: {t}");
        let wide = drawn(&s, 100, 16);
        assert!(
            wide.contains("Volume 80%") && wide.contains("Autoplay on"),
            "{wide}"
        );
    }

    #[test]
    fn it_says_what_to_do_when_idle_and_why_when_it_failed() {
        let mut s = PlayerScreen::default();
        let t = drawn(&s, 90, 14);
        assert!(
            t.contains("Nothing is playing") && t.contains("Queue (0)") && t.contains("empty"),
            "{t}"
        );
        s.set_view(PlayerView {
            state: "failed".into(),
            failure: Some("The video was removed or is private".into()),
            ..PlayerView::default()
        });
        let t = drawn(&s, 90, 14);
        assert!(
            t.contains("Stopped: The video was removed or is private")
                && t.contains("Player  failed"),
            "{t}"
        );
    }

    #[test]
    fn muted_unknown_length_and_hours_are_shown_sensibly() {
        let mut s = screen("playing");
        s.view.muted = true;
        s.view.duration = None;
        s.view.position = 3725;
        let t = drawn(&s, 100, 14);
        assert!(
            t.contains("Volume muted") && t.contains("1:02:05") && !t.contains(" / "),
            "{t}"
        );
    }

    #[test]
    fn space_starts_when_stopped_and_pauses_when_playing() {
        for st in ["idle", "ended", "failed"] {
            assert_eq!(
                cap(&screen(st).on_key(Key::Char(' '))).0,
                "player.play",
                "{st}"
            );
        }
        for st in ["playing", "paused", "loading"] {
            assert_eq!(
                cap(&screen(st).on_key(Key::Char(' '))).0,
                "player.pause",
                "{st}"
            );
        }
    }

    #[test]
    fn each_key_asks_for_the_right_capability_with_the_right_arguments() {
        let mut s = screen("playing");
        for (key, want_cap, want_args) in [
            (Key::Char('n'), "player.next", vec![]),
            (Key::Char('p'), "player.prev", vec![]),
            (Key::Right, "player.seek", vec![("by", "5")]),
            (Key::Left, "player.seek", vec![("by", "-5")]),
            (Key::Char('+'), "player.volume", vec![("by", "5")]),
            (Key::Char('='), "player.volume", vec![("by", "5")]),
            (Key::Char('-'), "player.volume", vec![("by", "-5")]),
            (Key::Char('m'), "player.mute", vec![]),
            (Key::Char(']'), "player.speed", vec![("by", "1")]),
            (Key::Char('['), "player.speed", vec![("by", "-1")]),
            (Key::Char('s'), "player.shuffle", vec![("on", "true")]),
            (Key::Char('a'), "player.autoplay", vec![("on", "false")]),
            (Key::Char('f'), "player.shorts", vec![("on", "true")]),
            (Key::Char('r'), "player.repeat", vec![("mode", "all")]),
            (Key::Char('u'), "player.rate", vec![("verdict", "up")]),
            (Key::Char('d'), "player.rate", vec![("verdict", "down")]),
            (Key::Char('x'), "player.rate", vec![("verdict", "report")]),
            (Key::Char('c'), "player.clear", vec![]),
        ] {
            let (c, a) = cap(&s.on_key(key));
            assert_eq!(c, want_cap, "{key:?}");
            let want: Vec<(String, String)> = want_args
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            assert_eq!(a, want, "{key:?}");
        }
    }

    #[test]
    fn toggles_ask_for_the_opposite_of_what_is_shown_and_repeat_cycles() {
        let mut s = screen("playing");
        s.view.shuffle = true;
        s.view.autoplay = false;
        s.view.shorts = true;
        assert_eq!(cap(&s.on_key(Key::Char('s'))).1[0].1, "false");
        assert_eq!(cap(&s.on_key(Key::Char('a'))).1[0].1, "true");
        assert_eq!(cap(&s.on_key(Key::Char('f'))).1[0].1, "false");
        for (now, next) in [("off", "all"), ("all", "one"), ("one", "off")] {
            s.view.repeat = now.into();
            assert_eq!(cap(&s.on_key(Key::Char('r'))).1[0].1, next);
        }
    }

    #[test]
    fn the_cursor_picks_queue_items_for_jump_and_remove_and_stays_inside_the_queue() {
        let mut s = screen("playing");
        assert_eq!(s.on_key(Key::Up), Action::None);
        s.on_key(Key::Down);
        s.on_key(Key::Down);
        s.on_key(Key::Down);
        assert_eq!(
            cap(&s.on_key(Key::Enter)),
            ("player.jump".into(), vec![("index".into(), "2".into())])
        );
        assert_eq!(
            cap(&s.on_key(Key::Delete)),
            ("player.remove".into(), vec![("index".into(), "2".into())])
        );
        s.on_key(Key::Home);
        assert_eq!(cap(&s.on_key(Key::Enter)).1[0].1, "0");
        s.on_key(Key::End);
        assert_eq!(cap(&s.on_key(Key::Enter)).1[0].1, "2");
        // A shorter queue pulls the cursor back in.
        s.set_view(PlayerView {
            queue: vec![row("only", "file", false)],
            ..PlayerView::default()
        });
        assert_eq!(cap(&s.on_key(Key::Enter)).1[0].1, "0");
        let mut empty = PlayerScreen::default();
        assert_eq!(empty.on_key(Key::Enter), Action::None);
        assert_eq!(empty.on_key(Key::Delete), Action::None);
    }

    #[test]
    fn o_opens_the_input_line_and_enter_adds_what_was_typed() {
        let mut s = screen("idle");
        assert_eq!(s.on_key(Key::Char('o')), Action::None);
        assert_eq!(s.input().as_deref(), Some(""));
        for c in "  https://youtu.be/dQw4w9WgXcQ ".chars() {
            // while typing, command keys are just letters
            assert_eq!(s.on_key(Key::Char(c)), Action::None);
        }
        let t = drawn(&s, 100, 20);
        assert!(
            t.contains("Add a YouTube link") && t.contains("https://youtu.be/dQw4w9WgXcQ"),
            "{t}"
        );
        assert_eq!(
            cap(&s.on_key(Key::Enter)),
            (
                "player.add".into(),
                vec![("url".into(), "https://youtu.be/dQw4w9WgXcQ".into())]
            )
        );
        assert!(s.input().is_none(), "the line closes");
        assert_eq!(
            s.on_key(Key::Char('n')),
            Action::Call {
                capability: "player.next".into(),
                args: vec![]
            },
            "and the keys work again"
        );
    }

    #[test]
    fn the_input_line_can_be_edited_cancelled_and_left_empty() {
        let mut s = screen("idle");
        s.on_key(Key::Char('o'));
        for c in "abc".chars() {
            s.on_key(Key::Char(c));
        }
        s.on_key(Key::Backspace);
        assert_eq!(s.input().as_deref(), Some("ab"));
        s.on_key(Key::Ctrl('u'));
        assert_eq!(s.input().as_deref(), Some(""));
        assert_eq!(
            s.on_key(Key::Enter),
            Action::None,
            "an empty line adds nothing"
        );
        assert!(s.input().is_none());
        s.on_key(Key::Char('o'));
        s.on_key(Key::Char('x'));
        assert_eq!(s.on_key(Key::Esc), Action::None);
        assert!(s.input().is_none());
        s.on_key(Key::Char('o'));
        for _ in 0..(MAX_INPUT + 50) {
            s.on_key(Key::Char('a'));
        }
        assert_eq!(s.input().unwrap().len(), MAX_INPUT, "bounded");
    }

    #[test]
    fn a_long_queue_scrolls_to_keep_the_cursor_in_view_and_small_areas_do_not_panic() {
        let mut s = PlayerScreen::default();
        s.set_view(PlayerView {
            queue: (0..80)
                .map(|i| row(&format!("item number {i}"), "file", false))
                .collect(),
            ..PlayerView::default()
        });
        for _ in 0..70 {
            s.on_key(Key::Down);
        }
        let mut g = Grid::new(90, 16);
        let a = g.area();
        s.draw(&mut g, a);
        assert!(g.to_text().contains("item number 70"), "{}", g.to_text());
        for (w, h) in [(0, 0), (1, 1), (10, 3), (24, 7), (30, 8), (200, 60)] {
            let _ = drawn(&s, w, h);
            let _ = drawn(&screen("playing"), w, h);
        }
        s.on_key(Key::Char('o'));
        for (w, h) in [(24, 7), (40, 9)] {
            let _ = drawn(&s, w, h);
        }
    }

    #[test]
    fn the_message_and_hints_follow_the_mode() {
        let mut s = screen("idle");
        s.message = "Added a thing".into();
        assert!(drawn(&s, 80, 14).contains("Added a thing"));
        assert!(s.hints().contains("Space play/pause"));
        s.on_key(Key::Char('o'));
        assert!(s.hints().contains("Esc cancel"));
    }
}
