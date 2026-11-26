//! The conversation.

use crate::grid::{theme, wrap, Color, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Screen};
use std::cell::Cell;

/// Who a message is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person using the machine.
    User,
    /// The model.
    Assistant,
    /// A capability's result (a tool the assistant or person ran).
    Tool,
    /// A notice from MOULI itself (a denial, a warning).
    Notice,
}

/// One message in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Who sent it.
    pub role: Role,
    /// The text.
    pub text: String,
}

/// Longest message the input line accepts, in characters.
pub const MAX_INPUT: usize = 4000;

/// The chat screen.
#[derive(Debug, Clone, Default)]
pub struct Chat {
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// The text being typed.
    input: Vec<char>,
    /// Cursor position in `input`.
    cursor: usize,
    /// How many lines the transcript is scrolled up from the bottom.
    pub scroll: usize,
    /// The furthest the transcript can scroll, as of the last time it was drawn. Keeps `scroll`
    /// from running past the top, which would take as many key presses to undo.
    max_scroll: Cell<usize>,
    /// True while the model is answering (the input is held back).
    pub busy: bool,
    /// True while the last message is an answer that is still arriving piece by piece.
    pub streaming: bool,
    /// The loaded model's name, shown above the transcript.
    pub model: String,
    /// A status line (for example "context nearly full").
    pub status: String,
}

fn label(role: Role) -> (&'static str, Color) {
    match role {
        Role::User => ("you", theme::ACCENT),
        Role::Assistant => ("mouli", theme::OK),
        Role::Tool => ("tool", theme::WARN),
        Role::Notice => ("note", theme::DIM),
    }
}

/// Width of the label column ("mouli" plus a space).
const LABEL_W: usize = 7;

impl Chat {
    /// Adds a message and jumps to the bottom.
    pub fn push(&mut self, role: Role, text: &str) {
        self.messages.push(Message {
            role,
            text: text.to_string(),
        });
        self.scroll = 0;
    }

    /// Adds a piece of an answer that is still arriving: it extends the answer being shown, or
    /// starts one.
    pub fn stream_piece(&mut self, piece: &str) {
        if self.streaming {
            if let Some(last) = self.messages.last_mut() {
                last.text.push_str(piece);
                self.scroll = 0;
                return;
            }
        }
        self.push(Role::Assistant, piece);
        self.streaming = true;
    }

    /// Ends an answer with its final text: replaces what was streamed, or adds it when nothing
    /// was streamed.
    pub fn finish_answer(&mut self, text: &str) {
        if self.streaming {
            if let Some(last) = self.messages.last_mut() {
                last.text = text.to_string();
            }
            self.streaming = false;
            self.scroll = 0;
        } else {
            self.push(Role::Assistant, text);
        }
    }

    /// Forgets the conversation (the input line is kept).
    pub fn clear(&mut self) {
        self.messages.clear();
        self.streaming = false;
        self.scroll = 0;
    }

    /// The text being typed.
    pub fn input(&self) -> String {
        self.input.iter().collect()
    }

    /// Replaces the input line (for example to restore a draft).
    pub fn set_input(&mut self, text: &str) {
        self.input = text
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_INPUT)
            .collect();
        self.cursor = self.input.len();
    }

    /// The transcript as display lines: (label, colour, text), where only the first line of a
    /// message carries its label.
    fn lines(&self, width: usize) -> Vec<(String, Color, String)> {
        let text_w = width.saturating_sub(LABEL_W).max(1);
        let mut out = Vec::new();
        for m in &self.messages {
            let (name, colour) = label(m.role);
            for (i, line) in wrap(&m.text, text_w).into_iter().enumerate() {
                out.push((
                    if i == 0 {
                        name.to_string()
                    } else {
                        String::new()
                    },
                    colour,
                    line,
                ));
            }
            out.push((String::new(), colour, String::new()));
        }
        if self.busy && !self.streaming {
            out.push(("mouli".to_string(), theme::OK, "thinking...".to_string()));
        }
        out
    }
}

impl Screen for Chat {
    fn on_key(&mut self, key: Key) -> Action {
        match key {
            Key::Char(c) if !c.is_control() => {
                if self.input.len() < MAX_INPUT {
                    self.input.insert(self.cursor, c);
                    self.cursor += 1;
                }
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                }
            }
            Key::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.input.len()),
            Key::Home | Key::Ctrl('a') => self.cursor = 0,
            Key::End | Key::Ctrl('e') => self.cursor = self.input.len(),
            Key::Ctrl('u') => {
                self.input.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::Ctrl('k') => self.input.truncate(self.cursor),
            Key::Up => self.scroll = (self.scroll + 1).min(self.max_scroll.get()),
            Key::Down => self.scroll = self.scroll.saturating_sub(1),
            Key::PageUp => self.scroll = (self.scroll + 10).min(self.max_scroll.get()),
            Key::PageDown => self.scroll = self.scroll.saturating_sub(10),
            Key::Ctrl('l') => return Action::NewConversation,
            Key::Enter => {
                let text: String = self.input.iter().collect::<String>().trim().to_string();
                if text.is_empty() || self.busy {
                    return Action::None;
                }
                self.input.clear();
                self.cursor = 0;
                return Action::Send(text);
            }
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        let title = if self.model.is_empty() {
            "Chat".to_string()
        } else {
            format!("Chat  {}", self.model)
        };
        g.boxed(area, Some(&title), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 12 || inner.h < 5 {
            return;
        }
        // The input box takes the bottom three rows; the transcript gets the rest.
        let view_h = inner.h - 3;
        let lines = self.lines(inner.w - 2);
        let max_scroll = lines.len().saturating_sub(view_h);
        self.max_scroll.set(max_scroll);
        let scroll = self.scroll.min(max_scroll);
        let end = lines.len() - scroll;
        let start = end.saturating_sub(view_h);
        if lines.is_empty() {
            g.text(
                inner.x + 2,
                inner.y + 1,
                "Ready. Type a message and press Enter.",
                theme::DIM,
                theme::BG,
            );
        }
        for (row, (name, colour, text)) in lines[start..end].iter().enumerate() {
            let y = inner.y + row;
            g.text(inner.x + 1, y, name, *colour, theme::BG);
            g.text(inner.x + 1 + LABEL_W, y, text, theme::TEXT, theme::BG);
        }
        if scroll > 0 {
            g.text(
                inner.x + inner.w - 14,
                inner.y,
                &format!(" {scroll} lines up "),
                theme::WARN,
                theme::BG,
            );
        }
        // Input box.
        let box_rect = Rect::new(inner.x, inner.y + view_h, inner.w, 3);
        g.boxed(
            box_rect,
            if self.busy {
                Some("waiting for the model - Ctrl-C stops it")
            } else {
                None
            },
            if self.busy { theme::DIM } else { theme::ACCENT },
            theme::BG,
        );
        let field_w = inner.w.saturating_sub(5);
        let (first, cursor_col) = window(self.cursor, self.input.len(), field_w);
        g.text(inner.x + 1, box_rect.y + 1, ">", theme::ACCENT, theme::BG);
        let shown: String = self.input.iter().skip(first).take(field_w).collect();
        g.text(inner.x + 3, box_rect.y + 1, &shown, theme::TEXT, theme::BG);
        if !self.busy {
            let c = self.input.get(self.cursor).copied().unwrap_or(' ');
            g.put(
                inner.x + 3 + cursor_col,
                box_rect.y + 1,
                c,
                theme::BG,
                theme::TEXT,
            );
        }
        if !self.status.is_empty() {
            let s = crate::grid::fit(&self.status, inner.w.saturating_sub(4));
            g.text(
                inner.x + 2,
                area.y + area.h - 1,
                &format!(" {s} "),
                theme::WARN,
                theme::BG,
            );
        }
    }

    fn hints(&self) -> String {
        if self.busy {
            return "Ctrl-C stop the answer  Up/PgUp scroll  F2 models".into();
        }
        "Enter send  Up/PgUp scroll  Ctrl-L new chat  F2 models".into()
    }
}

/// For an input of `len` characters with the cursor at `cursor`, shown in `width` cells:
/// the first visible character and the cursor's column. Scrolls just enough to keep the cursor
/// visible, leaving the cursor cell itself free at the end.
fn window(cursor: usize, len: usize, width: usize) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }
    let _ = len;
    let first = (cursor + 1).saturating_sub(width);
    (first, cursor - first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_text(c: &mut Chat, s: &str) {
        for ch in s.chars() {
            c.on_key(Key::Char(ch));
        }
    }

    fn drawn(c: &Chat, w: usize, h: usize) -> Grid {
        let mut g = Grid::new(w, h);
        let area = g.area();
        c.draw(&mut g, area);
        g
    }

    #[test]
    fn typing_editing_and_sending() {
        let mut c = Chat::default();
        type_text(&mut c, "helo");
        c.on_key(Key::Left);
        c.on_key(Key::Char('l'));
        assert_eq!(c.input(), "hello");
        c.on_key(Key::Home);
        c.on_key(Key::Delete);
        assert_eq!(c.input(), "ello");
        c.on_key(Key::End);
        c.on_key(Key::Backspace);
        assert_eq!(c.input(), "ell");
        assert_eq!(c.on_key(Key::Enter), Action::Send("ell".into()));
        assert_eq!(c.input(), "", "the line is cleared after sending");
        assert_eq!(
            c.on_key(Key::Enter),
            Action::None,
            "an empty line sends nothing"
        );
        type_text(&mut c, "   ");
        assert_eq!(
            c.on_key(Key::Enter),
            Action::None,
            "spaces alone send nothing"
        );
    }

    #[test]
    fn emacs_style_shortcuts() {
        let mut c = Chat::default();
        type_text(&mut c, "hello world");
        c.on_key(Key::Ctrl('a'));
        for _ in 0..6 {
            c.on_key(Key::Right);
        }
        c.on_key(Key::Ctrl('k'));
        assert_eq!(c.input(), "hello ");
        c.on_key(Key::Ctrl('u'));
        assert_eq!(c.input(), "");
        assert_eq!(c.on_key(Key::Ctrl('l')), Action::NewConversation);
    }

    #[test]
    fn editing_at_the_edges_is_safe() {
        let mut c = Chat::default();
        c.on_key(Key::Backspace);
        c.on_key(Key::Delete);
        c.on_key(Key::Left);
        c.on_key(Key::Right);
        type_text(&mut c, "a\u{7}b");
        assert_eq!(c.input(), "ab", "control characters are not inserted");
        c.set_input("tab\there");
        assert_eq!(c.input(), "tabhere");
        let long: String = std::iter::repeat_n('x', MAX_INPUT + 50).collect();
        c.set_input(&long);
        assert_eq!(c.input().len(), MAX_INPUT);
        c.on_key(Key::Char('y'));
        assert_eq!(
            c.input().len(),
            MAX_INPUT,
            "the limit holds while typing too"
        );
    }

    #[test]
    fn pieces_extend_one_answer_and_the_final_text_replaces_them() {
        let mut c = Chat::default();
        c.push(Role::User, "hi");
        c.busy = true;
        c.stream_piece("Hel");
        c.stream_piece("lo");
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[1].text, "Hello");
        assert!(c.streaming);
        assert!(c.hints().contains("Ctrl-C"));
        c.finish_answer("Hello there");
        assert!(!c.streaming);
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[1].text, "Hello there");
        // Without streaming the answer is simply added.
        c.finish_answer("Another");
        assert_eq!(c.messages.len(), 3);
    }

    #[test]
    fn nothing_is_sent_while_the_model_is_busy() {
        let mut c = Chat::default();
        type_text(&mut c, "hi");
        c.busy = true;
        assert_eq!(c.on_key(Key::Enter), Action::None);
        assert_eq!(c.input(), "hi", "the draft survives");
        c.busy = false;
        assert_eq!(c.on_key(Key::Enter), Action::Send("hi".into()));
    }

    #[test]
    fn the_transcript_shows_labels_and_wraps() {
        let mut c = Chat {
            model: "tiny-q4".into(),
            ..Default::default()
        };
        c.push(Role::User, "What is the capital of France?");
        c.push(Role::Assistant, "The capital of France is Paris, a city on the river Seine in the north of the country.");
        let t = drawn(&c, 50, 16).to_text();
        assert!(t.contains("Chat  tiny-q4"));
        assert!(t.contains("you    What is the capital of France?"), "{t}");
        assert!(
            t.contains("mouli  The capital of France is Paris, a city"),
            "{t}"
        );
        // The second line of the reply is indented under the text, not labelled again.
        let wrapped = t
            .lines()
            .find(|l| l.contains("river Seine") || l.contains("Seine"))
            .unwrap();
        assert!(!wrapped.contains("mouli"), "{wrapped}");
    }

    #[test]
    fn scrolling_shows_older_lines_and_says_so() {
        let mut c = Chat::default();
        for i in 0..30 {
            c.push(Role::User, &format!("message number {i}"));
        }
        let bottom = drawn(&c, 50, 12).to_text();
        assert!(bottom.contains("message number 29") && !bottom.contains("message number 0 "));
        for _ in 0..3 {
            c.on_key(Key::PageUp);
        }
        let up = drawn(&c, 50, 12).to_text();
        assert!(
            up.contains("lines up") && !up.contains("message number 29"),
            "{up}"
        );
        // Scrolling past the top is clamped, so one press of PageDown moves back at once.
        for _ in 0..100 {
            c.on_key(Key::PageUp);
        }
        let at_top = drawn(&c, 50, 12).to_text();
        assert!(at_top.contains("message number 0"));
        c.on_key(Key::PageDown);
        assert_ne!(
            drawn(&c, 50, 12).to_text(),
            at_top,
            "PageDown must have an immediate effect"
        );
        for _ in 0..100 {
            c.on_key(Key::PageDown);
        }
        assert!(drawn(&c, 50, 12).to_text().contains("message number 29"));
        c.on_key(Key::Up);
        c.push(Role::User, "new");
        assert_eq!(c.scroll, 0, "a new message jumps to the bottom");
    }

    #[test]
    fn thinking_indicator_and_busy_input_box() {
        let mut c = Chat::default();
        c.push(Role::User, "hello");
        c.busy = true;
        let t = drawn(&c, 50, 12).to_text();
        assert!(t.contains("mouli  thinking..."));
        assert!(t.contains("waiting for the model"));
    }

    #[test]
    fn the_cursor_is_drawn_inverted_and_long_input_scrolls() {
        let mut c = Chat::default();
        type_text(&mut c, "abc");
        let g = drawn(&c, 40, 10);
        let row = 10 - 2 - 1; // the input line inside the bottom box
        let cell = (0..40)
            .filter_map(|x| g.get(x, row))
            .find(|cell| cell.bg == theme::TEXT)
            .expect("cursor cell");
        assert_eq!(cell.ch, ' ', "the cursor sits after the last character");
        c.set_input(&"0123456789".repeat(10));
        let g = drawn(&c, 40, 10);
        let line = g.row_text(row);
        assert!(
            line.contains("789"),
            "the end of a long input stays visible: {line}"
        );
        assert!(
            !line.contains("0123456789012345678901234567890123456789"),
            "{line}"
        );
        c.on_key(Key::Home);
        assert!(drawn(&c, 40, 10).row_text(row).contains("0123456789"));
    }

    #[test]
    fn empty_chat_invites_typing_and_clear_empties_it() {
        let mut c = Chat::default();
        assert!(drawn(&c, 60, 12)
            .to_text()
            .contains("Ready. Type a message and press Enter."));
        c.push(Role::Notice, "welcome");
        c.clear();
        assert!(c.messages.is_empty());
        assert_eq!(c.scroll, 0);
    }

    #[test]
    fn a_status_line_appears_on_the_frame() {
        let c = Chat {
            status: "context nearly full".into(),
            ..Default::default()
        };
        assert!(drawn(&c, 60, 12)
            .to_text()
            .contains(" context nearly full "));
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let mut c = Chat::default();
        c.push(Role::User, "x".repeat(500).as_str());
        c.busy = true;
        type_text(&mut c, "abc");
        for (w, h) in [
            (0, 0),
            (2, 2),
            (11, 4),
            (12, 5),
            (13, 6),
            (80, 3),
            (200, 100),
        ] {
            let _ = drawn(&c, w, h);
        }
    }

    #[test]
    fn window_keeps_the_cursor_visible() {
        assert_eq!(window(0, 0, 10), (0, 0));
        assert_eq!(window(9, 9, 10), (0, 9));
        assert_eq!(window(10, 10, 10), (1, 9));
        assert_eq!(window(100, 100, 10), (91, 9));
        assert_eq!(window(5, 100, 0), (0, 0));
    }
}
