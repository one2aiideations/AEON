//! Choosing a model.

use crate::grid::{fit, theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// One model found on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// File or display name.
    pub name: String,
    /// File size in megabytes.
    pub size_mb: u64,
    /// Architecture (for example `llama`), when known.
    pub arch: Option<String>,
    /// Memory needed to run it with the planned context, in megabytes.
    pub needs_mb: u64,
    /// Whether it fits in the memory budget.
    pub fits: bool,
}

/// The model list.
#[derive(Debug, Clone, Default)]
pub struct ModelPicker {
    /// Models found.
    pub rows: Vec<ModelRow>,
    /// How much memory a model may use, in megabytes.
    pub budget_mb: u64,
    /// The planned context length in tokens.
    pub ctx: u32,
    /// A message under the list (for example why a load failed).
    pub message: String,
    /// The selection.
    pub cursor: Cursor,
}

impl ModelPicker {
    /// The selected model.
    pub fn selected(&self) -> Option<&ModelRow> {
        self.rows.get(self.cursor.selected)
    }
}

fn mb(n: u64) -> String {
    if n >= 1024 {
        // Rounded to one decimal place (tenths of a gigabyte).
        let tenths = (n * 10 + 512) / 1024;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else {
        format!("{n} MB")
    }
}

impl Screen for ModelPicker {
    fn on_key(&mut self, key: Key) -> Action {
        let len = self.rows.len();
        match key {
            Key::Up | Key::Char('k') => self.cursor.step(-1, len),
            Key::Down | Key::Char('j') => self.cursor.step(1, len),
            Key::PageUp => self.cursor.step(-8, len),
            Key::PageDown => self.cursor.step(8, len),
            Key::Home => self.cursor.jump(false, len),
            Key::End => self.cursor.jump(true, len),
            Key::Enter => {
                return match self.selected() {
                    Some(m) if m.fits => Action::ChooseModel(self.cursor.selected),
                    Some(m) => {
                        self.message = format!(
                            "{} needs {} but only {} may be used",
                            m.name,
                            mb(m.needs_mb),
                            mb(self.budget_mb)
                        );
                        Action::None
                    }
                    None => Action::None,
                };
            }
            Key::Char('r') => return Action::RescanModels,
            Key::Char('w') => return Action::Wifi,
            Key::Char('h') => return Action::Hosted,
            Key::Char('i') => return Action::HardwareInfo,
            Key::Char('q') => return Action::Quit,
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.boxed(area, Some("Choose a model"), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 20 || inner.h < 6 {
            return;
        }
        g.text_fit(
            inner.x + 1,
            inner.y,
            inner.w - 2,
            &format!(
                "Memory for a model: {}   Context: {} tokens",
                mb(self.budget_mb),
                self.ctx
            ),
            theme::DIM,
            theme::BG,
        );
        let list_top = inner.y + 2;
        let list_h = inner.h.saturating_sub(5);
        if self.rows.is_empty() {
            g.text(
                inner.x + 2,
                list_top,
                "No models found.",
                theme::WARN,
                theme::BG,
            );
            g.text(
                inner.x + 2,
                list_top + 1,
                "Copy a .gguf model file onto a USB stick or disk, plug it in,",
                theme::TEXT,
                theme::BG,
            );
            g.text(
                inner.x + 2,
                list_top + 2,
                "then press r to look again.",
                theme::TEXT,
                theme::BG,
            );
        } else {
            let top = self.cursor.window(list_h);
            let name_w = inner.w.saturating_sub(34).clamp(10, 40);
            for (i, row) in self.rows.iter().enumerate().skip(top).take(list_h) {
                let y = list_top + (i - top);
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
                g.text(inner.x + 3, y, &format!("{:>2}", i + 1), theme::DIM, bg);
                g.text_fit(
                    inner.x + 6,
                    y,
                    name_w,
                    &row.name,
                    if row.fits { theme::TEXT } else { theme::DIM },
                    bg,
                );
                let col = inner.x + 7 + name_w;
                g.text(col, y, &format!("{:>9}", mb(row.size_mb)), theme::DIM, bg);
                g.text(
                    col + 11,
                    y,
                    &fit(row.arch.as_deref().unwrap_or("?"), 8),
                    theme::DIM,
                    bg,
                );
                let (tag, colour) = if row.fits {
                    ("fits", theme::OK)
                } else {
                    ("too big", theme::BAD)
                };
                g.text(col + 20, y, tag, colour, bg);
            }
        }
        let foot = inner.y + inner.h - 2;
        g.text_fit(
            inner.x + 1,
            foot,
            inner.w - 2,
            &self.message,
            theme::WARN,
            theme::BG,
        );
    }

    fn hints(&self) -> String {
        "Up/Down choose  Enter load  r rescan  w Wi-Fi  h hosted  i hardware  q off".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, size: u64, needs: u64, fits: bool) -> ModelRow {
        ModelRow {
            name: name.into(),
            size_mb: size,
            arch: Some("llama".into()),
            needs_mb: needs,
            fits,
        }
    }

    fn picker() -> ModelPicker {
        ModelPicker {
            rows: vec![
                row("tiny-q4", 700, 900, true),
                row("big-q8", 9000, 11000, false),
                row("mid-q4", 3000, 3600, true),
            ],
            budget_mb: 4096,
            ctx: 2048,
            ..Default::default()
        }
    }

    fn drawn(p: &ModelPicker, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let area = g.area();
        p.draw(&mut g, area);
        g.to_text()
    }

    #[test]
    fn navigation_and_loading() {
        let mut p = picker();
        assert_eq!(p.on_key(Key::Enter), Action::ChooseModel(0));
        assert_eq!(p.on_key(Key::Down), Action::None);
        assert_eq!(p.on_key(Key::Char('j')), Action::None);
        assert_eq!(p.on_key(Key::Enter), Action::ChooseModel(2));
        p.on_key(Key::Home);
        assert_eq!(p.cursor.selected, 0);
        p.on_key(Key::End);
        assert_eq!(p.cursor.selected, 2);
        p.on_key(Key::Up);
        assert_eq!(p.cursor.selected, 1);
    }

    #[test]
    fn a_model_that_does_not_fit_cannot_be_loaded() {
        let mut p = picker();
        p.cursor.selected = 1;
        assert_eq!(p.on_key(Key::Enter), Action::None);
        assert!(
            p.message
                .contains("big-q8 needs 10.7 GB but only 4.0 GB may be used"),
            "{}",
            p.message
        );
    }

    #[test]
    fn the_hint_lines_fit_an_80_column_footer() {
        assert!(
            ModelPicker::default().hints().chars().count() <= 78,
            "{}",
            ModelPicker::default().hints()
        );
        assert!(crate::screens::Chat::default().hints().chars().count() <= 78);
    }

    #[test]
    fn shortcut_keys_become_actions() {
        let mut p = picker();
        assert_eq!(p.on_key(Key::Char('r')), Action::RescanModels);
        assert_eq!(p.on_key(Key::Char('w')), Action::Wifi);
        assert_eq!(p.on_key(Key::Char('h')), Action::Hosted);
        assert_eq!(p.on_key(Key::Char('i')), Action::HardwareInfo);
        assert_eq!(p.on_key(Key::Char('q')), Action::Quit);
        assert_eq!(p.on_key(Key::Char('z')), Action::None);
    }

    #[test]
    fn an_empty_list_explains_what_to_do() {
        let mut p = ModelPicker {
            budget_mb: 1024,
            ctx: 512,
            ..Default::default()
        };
        let t = drawn(&p, 80, 16);
        assert!(t.contains("No models found."));
        assert!(t.contains("then press r to look again."));
        assert_eq!(p.on_key(Key::Enter), Action::None);
        assert_eq!(p.on_key(Key::Down), Action::None);
    }

    #[test]
    fn the_list_shows_size_fit_and_selection() {
        let t = drawn(&picker(), 90, 16);
        assert!(t.contains("Memory for a model: 4.0 GB   Context: 2048 tokens"));
        let first = t.lines().find(|l| l.contains("tiny-q4")).unwrap();
        assert!(
            first.contains("> ") && first.contains("700 MB") && first.contains("fits"),
            "{first}"
        );
        let second = t.lines().find(|l| l.contains("big-q8")).unwrap();
        assert!(
            second.contains("8.8 GB") && second.contains("too big"),
            "{second}"
        );
    }

    #[test]
    fn a_long_list_scrolls_with_the_selection() {
        let rows: Vec<ModelRow> = (0..30)
            .map(|i| row(&format!("model-{i:02}"), 100, 200, true))
            .collect();
        let mut p = ModelPicker {
            rows,
            budget_mb: 4096,
            ctx: 2048,
            ..Default::default()
        };
        for _ in 0..25 {
            p.on_key(Key::Down);
        }
        let t = drawn(&p, 80, 14);
        assert!(t.contains("model-25"), "{t}");
        assert!(!t.contains("model-00"));
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        for (w, h) in [(0, 0), (3, 2), (10, 5), (19, 20), (80, 5)] {
            let _ = drawn(&picker(), w, h);
        }
    }
}
