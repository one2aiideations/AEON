//! The spreadsheet: a grid of cells with a formula bar. Editing a cell asks the controller to run
//! `sheet.set`; the screen only holds a copy of what the workbook shows.

use crate::grid::{theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Screen};
use std::cell::Cell;
use std::collections::HashMap;

/// Rows the grid has.
pub const MAX_ROWS: u32 = 1000;
/// Columns the grid has (A to Z).
pub const MAX_COLS: u32 = 26;
/// Width of a column in characters.
const COL_W: usize = 11;
/// Width of the row-number gutter.
const GUTTER: usize = 6;
/// Longest text a cell edit accepts, in characters.
const MAX_EDIT: usize = 500;

/// One cell as the workbook shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SheetCell {
    /// What is displayed (the computed value).
    pub shown: String,
    /// What was typed (a number, text, or a formula starting with `=`).
    pub raw: String,
}

/// The sheet screen.
#[derive(Debug, Clone, Default)]
pub struct SheetScreen {
    /// The sheet's name.
    pub name: String,
    /// The cells that have content, by (row, column), both from zero.
    pub cells: HashMap<(u32, u32), SheetCell>,
    /// The selected cell as (row, column).
    pub cursor: (u32, u32),
    editing: Option<Vec<char>>,
    top: Cell<u32>,
    left: Cell<u32>,
    /// A message for the person (for example why an edit was refused).
    pub message: String,
}

/// The name of a cell, such as `B2`.
pub fn cell_label(row: u32, col: u32) -> String {
    format!(
        "{}{}",
        char::from(b'A' + (col.min(MAX_COLS - 1)) as u8),
        row + 1
    )
}

impl SheetScreen {
    /// Replaces what the screen shows.
    pub fn set_cells(&mut self, name: &str, cells: HashMap<(u32, u32), SheetCell>) {
        self.name = name.to_string();
        self.cells = cells;
    }

    /// The text being typed into the selected cell, if an edit is open.
    pub fn editing(&self) -> Option<String> {
        self.editing.as_ref().map(|e| e.iter().collect())
    }

    fn raw_here(&self) -> String {
        self.cells
            .get(&self.cursor)
            .map(|c| c.raw.clone())
            .unwrap_or_default()
    }

    fn set_action(&self, value: String) -> Action {
        Action::Call {
            capability: "sheet.set".into(),
            args: vec![
                ("cell".into(), cell_label(self.cursor.0, self.cursor.1)),
                ("value".into(), value),
            ],
        }
    }

    fn step(&mut self, dr: i64, dc: i64) {
        let row = (i64::from(self.cursor.0) + dr).clamp(0, i64::from(MAX_ROWS) - 1);
        let col = (i64::from(self.cursor.1) + dc).clamp(0, i64::from(MAX_COLS) - 1);
        self.cursor = (row as u32, col as u32);
    }
}

impl Screen for SheetScreen {
    fn on_key(&mut self, key: Key) -> Action {
        if let Some(buf) = &mut self.editing {
            match key {
                Key::Char(c) if buf.len() < MAX_EDIT => buf.push(c),
                Key::Backspace => {
                    buf.pop();
                }
                Key::Esc => self.editing = None,
                Key::Enter => {
                    let value: String = buf.iter().collect();
                    self.editing = None;
                    self.message.clear();
                    return self.set_action(value);
                }
                _ => {}
            }
            return Action::None;
        }
        match key {
            Key::Up => self.step(-1, 0),
            Key::Down => self.step(1, 0),
            Key::Left | Key::BackTab => self.step(0, -1),
            Key::Right | Key::Tab => self.step(0, 1),
            Key::PageUp => self.step(-10, 0),
            Key::PageDown => self.step(10, 0),
            Key::Home => self.cursor.1 = 0,
            Key::Enter | Key::F(2) => self.editing = Some(self.raw_here().chars().collect()),
            Key::Delete | Key::Backspace => {
                if self.cells.contains_key(&self.cursor) {
                    return self.set_action(String::new());
                }
            }
            Key::Char(c) => self.editing = Some(vec![c]),
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        let title = if self.name.is_empty() {
            "Sheet".to_string()
        } else {
            format!("Sheet  {}", self.name)
        };
        g.boxed(area, Some(&title), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < GUTTER + COL_W + 2 || inner.h < 5 {
            return;
        }
        // The formula bar: the selected cell's name and its contents (or the edit in progress).
        let label = cell_label(self.cursor.0, self.cursor.1);
        let (content, colour) = match self.editing() {
            Some(e) => (format!("{e}_"), theme::ACCENT),
            None => (self.raw_here(), theme::TEXT),
        };
        g.fill(Rect::new(inner.x, inner.y, inner.w, 1), theme::PANEL);
        g.text(inner.x + 1, inner.y, &label, theme::ACCENT, theme::PANEL);
        g.text_fit(
            inner.x + 6,
            inner.y,
            inner.w.saturating_sub(7),
            &content,
            colour,
            theme::PANEL,
        );

        let cols = ((inner.w - GUTTER) / COL_W).clamp(1, MAX_COLS as usize) as u32;
        let rows = (inner.h - 2).clamp(1, MAX_ROWS as usize) as u32;
        // Scroll just enough to keep the cursor in view; the window is remembered between draws.
        let (mut top, mut left) = (self.top.get(), self.left.get());
        if self.cursor.0 < top {
            top = self.cursor.0;
        } else if self.cursor.0 >= top + rows {
            top = self.cursor.0 + 1 - rows;
        }
        if self.cursor.1 < left {
            left = self.cursor.1;
        } else if self.cursor.1 >= left + cols {
            left = self.cursor.1 + 1 - cols;
        }
        self.top.set(top);
        self.left.set(left);

        let head_y = inner.y + 1;
        g.fill(Rect::new(inner.x, head_y, inner.w, 1), theme::PANEL);
        for c in 0..cols {
            let col = left + c;
            let name = char::from(b'A' + col as u8).to_string();
            let x = inner.x + GUTTER + c as usize * COL_W;
            let on = col == self.cursor.1;
            let fg = if on { theme::ACCENT } else { theme::DIM };
            g.text(x + COL_W / 2, head_y, &name, fg, theme::PANEL);
        }
        for r in 0..rows {
            let row = top + r;
            if row >= MAX_ROWS {
                break;
            }
            let y = head_y + 1 + r as usize;
            let fg = if row == self.cursor.0 {
                theme::ACCENT
            } else {
                theme::DIM
            };
            g.text(inner.x + 1, y, &format!("{:>4}", row + 1), fg, theme::BG);
            for c in 0..cols {
                let col = left + c;
                let x = inner.x + GUTTER + c as usize * COL_W;
                let selected = (row, col) == self.cursor;
                let bg = if selected { theme::SELECT } else { theme::BG };
                g.fill(Rect::new(x, y, COL_W, 1), bg);
                let shown = match (selected, self.editing()) {
                    (true, Some(e)) => format!("{e}_"),
                    _ => self
                        .cells
                        .get(&(row, col))
                        .map(|c| c.shown.clone())
                        .unwrap_or_default(),
                };
                let numeric = shown.parse::<f64>().is_ok();
                let text = crate::grid::fit(&shown, COL_W - 1);
                let pad = if numeric {
                    (COL_W - 1).saturating_sub(text.chars().count())
                } else {
                    0
                };
                let fg = if shown.starts_with('#') {
                    theme::BAD
                } else {
                    theme::TEXT
                };
                g.text(x + pad, y, &text, fg, bg);
            }
        }
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
        if self.editing.is_some() {
            "Enter save  Esc cancel".into()
        } else {
            "Arrows move  type to edit  Enter edit  Del clear".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(shown: &str, raw: &str) -> SheetCell {
        SheetCell {
            shown: shown.into(),
            raw: raw.into(),
        }
    }

    fn screen() -> SheetScreen {
        let mut s = SheetScreen::default();
        let mut m = HashMap::new();
        m.insert((0, 0), cell("10", "10"));
        m.insert((1, 0), cell("32", "32"));
        m.insert((2, 0), cell("42", "=A1+A2"));
        m.insert((0, 1), cell("#DIV/0!", "=1/0"));
        s.set_cells("Sheet1", m);
        s
    }

    fn drawn(s: &SheetScreen, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let a = g.area();
        s.draw(&mut g, a);
        g.to_text()
    }

    fn set(cell: &str, value: &str) -> Action {
        Action::Call {
            capability: "sheet.set".into(),
            args: vec![("cell".into(), cell.into()), ("value".into(), value.into())],
        }
    }

    #[test]
    fn the_grid_shows_headers_values_and_the_formula_of_the_selected_cell() {
        let mut s = screen();
        s.cursor = (2, 0);
        let t = drawn(&s, 80, 14);
        assert!(t.contains("Sheet  Sheet1"), "{t}");
        assert!(t.contains("A") && t.contains("B"), "{t}");
        assert!(t.contains("42") && t.contains("#DIV/0!"), "{t}");
        assert!(
            t.contains("A3") && t.contains("=A1+A2"),
            "the formula bar: {t}"
        );
    }

    #[test]
    fn arrows_move_within_the_grid_and_it_scrolls_to_follow() {
        let mut s = screen();
        s.on_key(Key::Up);
        s.on_key(Key::Left);
        assert_eq!(s.cursor, (0, 0), "stays at the corner");
        s.on_key(Key::Right);
        s.on_key(Key::Down);
        assert_eq!(s.cursor, (1, 1));
        for _ in 0..30 {
            s.on_key(Key::PageDown);
        }
        assert_eq!(s.cursor.0, MAX_ROWS.min(1 + 300), "page down is ten rows");
        let t = drawn(&s, 80, 14);
        assert!(
            t.contains(&format!("{}", s.cursor.0 + 1)),
            "the cursor's row is on screen: {t}"
        );
        for _ in 0..40 {
            s.on_key(Key::Right);
        }
        assert_eq!(s.cursor.1, MAX_COLS - 1);
        assert!(drawn(&s, 80, 14).contains('Z'));
    }

    #[test]
    fn typing_edits_a_cell_and_enter_asks_for_it_to_be_set() {
        let mut s = screen();
        s.cursor = (4, 2); // C5
        assert_eq!(s.on_key(Key::Char('h')), Action::None);
        assert_eq!(s.editing().as_deref(), Some("h"));
        for c in "ello \"x\"".chars() {
            s.on_key(Key::Char(c));
        }
        s.on_key(Key::Backspace);
        assert_eq!(s.on_key(Key::Enter), set("C5", "hello \"x"));
        assert_eq!(s.editing(), None);
        assert!(
            !drawn(&s, 80, 14).contains("hello"),
            "the screen waits for the workbook to confirm"
        );
    }

    #[test]
    fn enter_edits_the_existing_contents_and_escape_abandons_the_edit() {
        let mut s = screen();
        s.cursor = (2, 0);
        s.on_key(Key::Enter);
        assert_eq!(s.editing().as_deref(), Some("=A1+A2"));
        assert!(drawn(&s, 80, 14).contains("=A1+A2_"));
        s.on_key(Key::Esc);
        assert_eq!(s.editing(), None);
        assert_eq!(s.on_key(Key::Down), Action::None);
        assert_eq!(s.cursor, (3, 0));
    }

    #[test]
    fn delete_clears_only_a_cell_that_has_content() {
        let mut s = screen();
        s.cursor = (0, 0);
        assert_eq!(s.on_key(Key::Delete), set("A1", ""));
        s.cursor = (5, 5);
        assert_eq!(
            s.on_key(Key::Delete),
            Action::None,
            "nothing there to clear"
        );
    }

    #[test]
    fn edits_are_length_limited() {
        let mut s = screen();
        for _ in 0..MAX_EDIT + 50 {
            s.on_key(Key::Char('x'));
        }
        assert_eq!(s.editing().unwrap().chars().count(), MAX_EDIT);
    }

    #[test]
    fn drawing_never_panics_at_any_size_or_cursor() {
        let mut s = screen();
        for (r, c) in [(0, 0), (999, 25), (500, 12)] {
            s.cursor = (r, c);
            for w in 0..70 {
                for h in 0..20 {
                    drawn(&s, w, h);
                }
            }
        }
        assert_eq!(cell_label(0, 0), "A1");
        assert_eq!(cell_label(998, 25), "Z999");
    }
}
