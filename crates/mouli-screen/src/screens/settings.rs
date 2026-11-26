//! The settings: how the screen looks and what the machine is set up with.

use crate::grid::{theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// One line of the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// The name the host knows it by, for example `contrast`.
    pub id: String,
    /// What the person reads.
    pub label: String,
    /// The current value, as shown.
    pub value: String,
    /// The values the person can step through (empty: the value is only shown).
    pub choices: Vec<String>,
    /// How to change it, or what it means.
    pub note: String,
}

/// The settings screen.
#[derive(Debug, Clone, Default)]
pub struct SettingsScreen {
    /// The settings, in order.
    pub rows: Vec<SettingRow>,
    /// The selection.
    pub cursor: Cursor,
    /// A message under the list.
    pub message: String,
}

impl SettingsScreen {
    /// Replaces the rows, keeping the selection in range.
    pub fn set_rows(&mut self, rows: Vec<SettingRow>) {
        self.rows = rows;
        self.cursor.clamp(self.rows.len());
    }

    fn change(&mut self, forward: bool) -> Action {
        let Some(row) = self.rows.get(self.cursor.selected) else {
            return Action::None;
        };
        if row.choices.is_empty() {
            self.message = if row.note.is_empty() {
                format!("{} cannot be changed here.", row.label)
            } else {
                format!("{}: {}", row.label, row.note)
            };
            return Action::None;
        }
        let n = row.choices.len();
        let at = row
            .choices
            .iter()
            .position(|c| *c == row.value)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        self.message.clear();
        Action::Setting {
            id: row.id.clone(),
            value: row.choices[next].clone(),
        }
    }
}

impl Screen for SettingsScreen {
    fn on_key(&mut self, key: Key) -> Action {
        let len = self.rows.len();
        match key {
            Key::Up | Key::Char('k') => self.cursor.step(-1, len),
            Key::Down | Key::Char('j') => self.cursor.step(1, len),
            Key::Home => self.cursor.jump(false, len),
            Key::End => self.cursor.jump(true, len),
            Key::Right | Key::Enter | Key::Char(' ') => return self.change(true),
            Key::Left => return self.change(false),
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.boxed(area, Some("Settings"), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 30 || inner.h < 5 {
            return;
        }
        if self.rows.is_empty() {
            g.text(
                inner.x + 2,
                inner.y + 1,
                "No settings.",
                theme::DIM,
                theme::BG,
            );
            return;
        }
        let label_w = self
            .rows
            .iter()
            .map(|r| r.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(inner.w / 2);
        let list_h = inner.h.saturating_sub(4).max(1);
        let top = self.cursor.window(list_h);
        for (i, r) in self.rows.iter().enumerate().skip(top).take(list_h) {
            let y = inner.y + 1 + (i - top);
            let sel = i == self.cursor.selected;
            let bg = if sel { theme::SELECT } else { theme::BG };
            g.fill(Rect::new(inner.x, y, inner.w, 1), bg);
            g.text(
                inner.x + 1,
                y,
                if sel { ">" } else { " " },
                theme::ACCENT,
                bg,
            );
            g.text_fit(inner.x + 3, y, label_w, &r.label, theme::TEXT, bg);
            let vx = inner.x + 4 + label_w;
            let shown = if r.choices.is_empty() {
                r.value.clone()
            } else {
                format!("< {} >", r.value)
            };
            g.text_fit(
                vx,
                y,
                inner.w.saturating_sub(vx - inner.x + 1),
                &shown,
                if r.choices.is_empty() {
                    theme::DIM
                } else {
                    theme::ACCENT
                },
                bg,
            );
        }
        let note_y = inner.y + inner.h - 2;
        if let Some(r) = self.rows.get(self.cursor.selected) {
            g.text_fit(
                inner.x + 2,
                note_y,
                inner.w - 4,
                &r.note,
                theme::DIM,
                theme::BG,
            );
        }
        if !self.message.is_empty() {
            g.text_fit(
                inner.x + 2,
                note_y - 1,
                inner.w - 4,
                &self.message,
                theme::WARN,
                theme::BG,
            );
        }
    }

    fn hints(&self) -> String {
        "Up/Down choose  Left/Right or Enter change".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<SettingRow> {
        vec![
            SettingRow {
                id: "contrast".into(),
                label: "High contrast".into(),
                value: "off".into(),
                choices: vec!["off".into(), "on".into()],
                note: "Stronger colours.".into(),
            },
            SettingRow {
                id: "size".into(),
                label: "Text size".into(),
                value: "2x".into(),
                choices: vec!["1x".into(), "2x".into(), "3x".into()],
                note: "Applies after a restart.".into(),
            },
            SettingRow {
                id: "memory".into(),
                label: "Memory for a model".into(),
                value: "4096 MB".into(),
                choices: vec![],
                note: "Set at start-up.".into(),
            },
        ]
    }

    fn screen() -> SettingsScreen {
        let mut s = SettingsScreen::default();
        s.set_rows(rows());
        s
    }

    fn drawn(s: &SettingsScreen) -> String {
        let mut g = Grid::new(70, 14);
        let a = g.area();
        s.draw(&mut g, a);
        g.to_text()
    }

    #[test]
    fn right_and_left_step_through_the_choices_and_wrap() {
        let mut s = screen();
        assert_eq!(
            s.on_key(Key::Right),
            Action::Setting {
                id: "contrast".into(),
                value: "on".into()
            }
        );
        // Left from the first choice wraps to the last.
        assert_eq!(
            s.on_key(Key::Left),
            Action::Setting {
                id: "contrast".into(),
                value: "on".into()
            }
        );
        s.on_key(Key::Down);
        assert_eq!(
            s.on_key(Key::Enter),
            Action::Setting {
                id: "size".into(),
                value: "3x".into()
            }
        );
        assert_eq!(
            s.on_key(Key::Left),
            Action::Setting {
                id: "size".into(),
                value: "1x".into()
            }
        );
    }

    #[test]
    fn a_read_only_row_explains_itself_and_changes_nothing() {
        let mut s = screen();
        s.on_key(Key::End);
        assert_eq!(s.on_key(Key::Enter), Action::None);
        assert!(s.message.contains("Set at start-up"), "{}", s.message);
        assert!(drawn(&s).contains("Set at start-up"));
    }

    #[test]
    fn it_draws_values_and_survives_tiny_and_empty_areas() {
        let s = screen();
        let t = drawn(&s);
        assert!(t.contains("High contrast") && t.contains("< off >"), "{t}");
        assert!(t.contains("4096 MB") && !t.contains("< 4096 MB >"), "{t}");
        let mut g = Grid::new(10, 3);
        let a = g.area();
        s.draw(&mut g, a);
        let mut e = SettingsScreen::default();
        assert_eq!(e.on_key(Key::Enter), Action::None);
        assert!(drawn(&e).contains("No settings"));
        e.set_rows(rows());
        e.cursor.selected = 99;
        e.set_rows(rows());
        assert!(e.cursor.selected < 3);
    }
}
