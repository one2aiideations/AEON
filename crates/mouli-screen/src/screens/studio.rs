//! The document editor: the document's blocks, and a line to add the next one.

use crate::grid::{theme, wrap, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Screen};
use std::cell::Cell;

/// Longest line the input accepts, in characters.
const MAX_INPUT: usize = 4000;

/// One block of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudioBlock {
    /// A heading (true) or a paragraph (false).
    pub heading: bool,
    /// The text.
    pub text: String,
}

/// The studio screen.
#[derive(Debug, Clone, Default)]
pub struct StudioScreen {
    /// The document's blocks, in order.
    pub blocks: Vec<StudioBlock>,
    input: Vec<char>,
    scroll: Cell<usize>,
    max_scroll: Cell<usize>,
    /// A message for the person.
    pub message: String,
}

impl StudioScreen {
    /// The text typed so far.
    pub fn input(&self) -> String {
        self.input.iter().collect()
    }

    fn lines(&self, width: usize) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push((String::new(), false));
            }
            for l in wrap(&b.text, width.max(1)) {
                out.push((l, b.heading));
            }
        }
        out
    }
}

impl Screen for StudioScreen {
    fn on_key(&mut self, key: Key) -> Action {
        match key {
            Key::Char(c) if self.input.len() < MAX_INPUT => self.input.push(c),
            Key::Backspace => {
                self.input.pop();
            }
            Key::Ctrl('u') => self.input.clear(),
            Key::Up => self.scroll.set(self.scroll.get().saturating_sub(1)),
            Key::Down => self
                .scroll
                .set((self.scroll.get() + 1).min(self.max_scroll.get())),
            Key::PageUp => self.scroll.set(self.scroll.get().saturating_sub(10)),
            Key::PageDown => self
                .scroll
                .set((self.scroll.get() + 10).min(self.max_scroll.get())),
            Key::Enter => {
                let line = self.input().trim().to_string();
                if line.is_empty() {
                    return Action::None;
                }
                self.input.clear();
                self.message.clear();
                // A line that starts with "# " is a heading; anything else is a paragraph.
                return match line.strip_prefix("# ") {
                    Some(h) => Action::Call {
                        capability: "studio.heading".into(),
                        args: vec![("text".into(), h.trim().to_string())],
                    },
                    None => Action::Call {
                        capability: "studio.paragraph".into(),
                        args: vec![("text".into(), line)],
                    },
                };
            }
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        let title = format!("Studio  {} block(s)", self.blocks.len());
        g.boxed(area, Some(&title), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 16 || inner.h < 6 {
            return;
        }
        let text_w = inner.w - 4;
        let view_h = inner.h - 4;
        let lines = self.lines(text_w);
        self.max_scroll.set(lines.len().saturating_sub(view_h));
        let top = self.scroll.get().min(self.max_scroll.get());
        if lines.is_empty() {
            g.text(
                inner.x + 2,
                inner.y + 1,
                "The document is empty. Type below to add a paragraph;",
                theme::DIM,
                theme::BG,
            );
            g.text(
                inner.x + 2,
                inner.y + 2,
                "start a line with \"# \" to add a heading.",
                theme::DIM,
                theme::BG,
            );
        }
        for (i, (line, heading)) in lines.iter().skip(top).take(view_h).enumerate() {
            let fg = if *heading { theme::ACCENT } else { theme::TEXT };
            g.text(inner.x + 2, inner.y + i, line, fg, theme::BG);
        }
        let by = inner.y + inner.h - 3;
        g.boxed(
            Rect::new(inner.x, by, inner.w, 3),
            None,
            theme::DIM,
            theme::BG,
        );
        let typed = self.input();
        let tail: String = typed
            .chars()
            .rev()
            .take(inner.w.saturating_sub(6))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        g.text(inner.x + 2, by + 1, ">", theme::ACCENT, theme::BG);
        let used = g.text(inner.x + 4, by + 1, &tail, theme::TEXT, theme::BG);
        g.text(inner.x + 4 + used, by + 1, "_", theme::ACCENT, theme::BG);
        if !self.message.is_empty() {
            g.text_fit(
                inner.x + 2,
                area.y + area.h - 1,
                inner.w.saturating_sub(2),
                &format!(" {} ", self.message),
                theme::WARN,
                theme::BG,
            );
        }
    }

    fn hints(&self) -> String {
        "Enter add paragraph  # heading  Up/Down scroll".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(heading: bool, text: &str) -> StudioBlock {
        StudioBlock {
            heading,
            text: text.into(),
        }
    }

    fn drawn(s: &StudioScreen, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let a = g.area();
        s.draw(&mut g, a);
        g.to_text()
    }

    fn type_line(s: &mut StudioScreen, line: &str) -> Action {
        for c in line.chars() {
            s.on_key(Key::Char(c));
        }
        s.on_key(Key::Enter)
    }

    #[test]
    fn an_empty_document_explains_how_to_start() {
        let t = drawn(&StudioScreen::default(), 70, 14);
        assert!(
            t.contains("The document is empty") && t.contains("# "),
            "{t}"
        );
    }

    #[test]
    fn blocks_are_shown_with_headings_apart_and_long_text_wrapped() {
        let s = StudioScreen {
            blocks: vec![block(true, "Plan"), block(false, "A long paragraph that must wrap across more than one line of the window to be readable.")],
            ..StudioScreen::default()
    };
        let t = drawn(&s, 50, 16);
        assert!(t.contains("Plan") && t.contains("A long paragraph"), "{t}");
        assert!(t.contains("2 block(s)"), "{t}");
        assert!(
            t.lines()
                .filter(|l| l.contains("window") || l.contains("readable"))
                .count()
                >= 1,
            "wrapped: {t}"
        );
    }

    #[test]
    fn enter_adds_a_paragraph_or_a_heading_by_its_prefix() {
        let mut s = StudioScreen::default();
        assert_eq!(
            type_line(&mut s, "  First step.  "),
            Action::Call {
                capability: "studio.paragraph".into(),
                args: vec![("text".into(), "First step.".into())]
            }
        );
        assert_eq!(s.input(), "", "the line is cleared");
        assert_eq!(
            type_line(&mut s, "# Plan"),
            Action::Call {
                capability: "studio.heading".into(),
                args: vec![("text".into(), "Plan".into())]
            }
        );
        assert_eq!(
            s.on_key(Key::Enter),
            Action::None,
            "an empty line sends nothing"
        );
        type_line(&mut s, "   ");
        assert_eq!(s.on_key(Key::Enter), Action::None);
    }

    #[test]
    fn scrolling_is_bounded_by_the_document() {
        let mut s = StudioScreen {
            blocks: (0..40)
                .map(|i| block(false, &format!("paragraph number {i}")))
                .collect(),
            ..StudioScreen::default()
        };
        drawn(&s, 60, 14);
        for _ in 0..500 {
            s.on_key(Key::Down);
        }
        let t = drawn(&s, 60, 14);
        assert!(
            t.contains("paragraph number 39"),
            "the end is reachable: {t}"
        );
        for _ in 0..500 {
            s.on_key(Key::Up);
        }
        assert!(drawn(&s, 60, 14).contains("paragraph number 0"));
    }

    #[test]
    fn input_is_length_limited_and_editable() {
        let mut s = StudioScreen::default();
        for _ in 0..MAX_INPUT + 20 {
            s.on_key(Key::Char('x'));
        }
        assert_eq!(s.input().chars().count(), MAX_INPUT);
        s.on_key(Key::Backspace);
        assert_eq!(s.input().chars().count(), MAX_INPUT - 1);
        s.on_key(Key::Ctrl('u'));
        assert_eq!(s.input(), "");
    }

    #[test]
    fn drawing_never_panics_at_any_size() {
        let s = StudioScreen {
            blocks: vec![block(true, "H"), block(false, "text ".repeat(100).trim())],
            ..StudioScreen::default()
        };
        for w in 0..70 {
            for h in 0..20 {
                drawn(&s, w, h);
            }
        }
    }
}
