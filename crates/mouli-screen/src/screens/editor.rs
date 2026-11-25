//! The code editor: a file list, a text area with line numbers and colouring, undo and find.
//!
//! The screen never touches the disk. Opening a file asks for it (`code.read`), saving asks for
//! the change to be proposed (`code.edit`), and the proposal then waits on the Review screen for a
//! second person's approval. So an edit made here cannot reach the files by itself.
//!
//! Limits, stated plainly: there is no selection or clipboard yet, a character is one cell wide,
//! and colouring works one line at a time (see [`crate::syntax`]).

use crate::grid::{theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};
use crate::syntax::{classify, Lang};
use std::cell::Cell;

/// Largest file the editor opens, in bytes.
pub const MAX_BYTES: usize = 256 * 1024;
/// Most lines in a file.
pub const MAX_LINES: usize = 20_000;
/// Longest line, in characters.
pub const MAX_LINE: usize = 2_000;
/// Undo steps kept.
const MAX_UNDO: usize = 100;
const TAB_WIDTH: usize = 4;
const GUTTER: usize = 6;

/// What the keyboard is typing into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The file list.
    #[default]
    Files,
    /// The text.
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    lines: Vec<Vec<char>>,
    row: usize,
    col: usize,
}

/// What the last edit was, so a run of typing is one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Typing(usize),
    Other,
}

/// The text of one file as lines of characters.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Buffer {
    lines: Vec<Vec<char>>,
    row: usize,
    col: usize,
    crlf: bool,
    final_newline: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            crlf: false,
            final_newline: true,
        }
    }
}

impl Buffer {
    fn from_text(text: &str) -> Result<Self, String> {
        if text.len() > MAX_BYTES {
            return Err(format!("The file is larger than {} KB.", MAX_BYTES / 1024));
        }
        let crlf = text.contains("\r\n");
        let final_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut lines: Vec<Vec<char>> = body
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).chars().collect())
            .collect();
        if lines.len() > MAX_LINES {
            return Err(format!("The file has more than {MAX_LINES} lines."));
        }
        if lines.iter().any(|l| l.len() > MAX_LINE) {
            return Err(format!("A line is longer than {MAX_LINE} characters."));
        }
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        Ok(Self {
            lines,
            row: 0,
            col: 0,
            crlf,
            final_newline,
        })
    }

    fn to_text(&self) -> String {
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self
            .lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join(eol);
        if self.final_newline {
            s.push_str(eol);
        }
        s
    }

    fn line_len(&self) -> usize {
        self.lines[self.row].len()
    }

    fn clamp(&mut self) {
        self.row = self.row.min(self.lines.len() - 1);
        self.col = self.col.min(self.line_len());
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.lines = s.lines;
        self.row = s.row;
        self.col = s.col;
        self.clamp();
    }
}

/// The editor screen.
#[derive(Debug, Clone, Default)]
pub struct EditorScreen {
    /// The files of the project, as paths below its folder.
    pub files: Vec<String>,
    tree: Cursor,
    path: Option<String>,
    buf: Buffer,
    dirty: bool,
    focus: Focus,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Option<Edit>,
    find: Option<String>,
    last_find: String,
    discard: Option<usize>,
    top: Cell<usize>,
    left: Cell<usize>,
    view_h: Cell<usize>,
    /// A message for the person.
    pub message: String,
}

impl EditorScreen {
    /// Replaces the list of files.
    pub fn set_files(&mut self, mut files: Vec<String>) {
        files.sort();
        files.dedup();
        self.files = files;
        self.tree.clamp(self.files.len());
    }

    /// Shows `text` as the content of `path`. Returns the reason when the file cannot be edited
    /// here (too large, too many lines), leaving the open file as it was.
    ///
    /// # Errors
    /// A message for the person.
    pub fn load(&mut self, path: &str, text: &str) -> Result<(), String> {
        let buf = Buffer::from_text(text)?;
        self.buf = buf;
        self.path = Some(path.to_string());
        self.dirty = false;
        self.undo.clear();
        self.redo.clear();
        self.last = None;
        self.find = None;
        self.discard = None;
        self.top.set(0);
        self.left.set(0);
        self.focus = Focus::Text;
        self.message.clear();
        Ok(())
    }

    /// Records that the open file's content was sent for review.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// The open file's path.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// The text as it is now.
    pub fn text(&self) -> String {
        self.buf.to_text()
    }

    /// True when the text differs from what was loaded or last sent.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// What the keyboard is typing into.
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The cursor as (line, column), counted from 1.
    pub fn position(&self) -> (usize, usize) {
        (self.buf.row + 1, self.buf.col + 1)
    }

    fn begin_edit(&mut self, kind: Edit) {
        let coalesce =
            matches!((self.last, kind), (Some(Edit::Typing(a)), Edit::Typing(b)) if a == b);
        if !coalesce {
            if self.undo.len() == MAX_UNDO {
                self.undo.remove(0);
            }
            self.undo.push(self.buf.snapshot());
        }
        self.redo.clear();
        self.last = Some(kind);
        self.dirty = true;
        self.message.clear();
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(self.buf.snapshot());
            self.buf.restore(prev);
            self.dirty = true;
            self.last = None;
        } else {
            self.message = "Nothing to undo.".into();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(self.buf.snapshot());
            self.buf.restore(next);
            self.dirty = true;
            self.last = None;
        } else {
            self.message = "Nothing to redo.".into();
        }
    }

    fn insert(&mut self, c: char) {
        if self.buf.line_len() >= MAX_LINE {
            self.message = format!("A line cannot be longer than {MAX_LINE} characters.");
            return;
        }
        self.begin_edit(Edit::Typing(self.buf.row));
        let (r, c0) = (self.buf.row, self.buf.col);
        self.buf.lines[r].insert(c0, c);
        self.buf.col += 1;
    }

    fn insert_tab(&mut self) {
        for _ in 0..TAB_WIDTH {
            self.insert(' ');
        }
    }

    fn newline(&mut self) {
        if self.buf.lines.len() >= MAX_LINES {
            self.message = format!("A file cannot have more than {MAX_LINES} lines.");
            return;
        }
        self.begin_edit(Edit::Other);
        let (r, c) = (self.buf.row, self.buf.col);
        let indent: Vec<char> = self.buf.lines[r]
            .iter()
            .take_while(|ch| **ch == ' ' || **ch == '\t')
            .copied()
            .take(c)
            .collect();
        let rest = self.buf.lines[r].split_off(c);
        let mut next = indent.clone();
        next.extend(rest);
        self.buf.lines.insert(r + 1, next);
        self.buf.row += 1;
        self.buf.col = indent.len();
    }

    fn backspace(&mut self) {
        let (r, c) = (self.buf.row, self.buf.col);
        if c > 0 {
            self.begin_edit(Edit::Other);
            self.buf.lines[r].remove(c - 1);
            self.buf.col -= 1;
        } else if r > 0 {
            let len = self.buf.lines[r - 1].len() + self.buf.lines[r].len();
            if len > MAX_LINE {
                self.message = format!("Joined, the line would pass {MAX_LINE} characters.");
                return;
            }
            self.begin_edit(Edit::Other);
            let cur = self.buf.lines.remove(r);
            self.buf.row -= 1;
            self.buf.col = self.buf.lines[r - 1].len();
            self.buf.lines[r - 1].extend(cur);
        }
    }

    fn delete(&mut self) {
        let (r, c) = (self.buf.row, self.buf.col);
        if c < self.buf.lines[r].len() {
            self.begin_edit(Edit::Other);
            self.buf.lines[r].remove(c);
        } else if r + 1 < self.buf.lines.len() {
            if self.buf.lines[r].len() + self.buf.lines[r + 1].len() > MAX_LINE {
                self.message = format!("Joined, the line would pass {MAX_LINE} characters.");
                return;
            }
            self.begin_edit(Edit::Other);
            let next = self.buf.lines.remove(r + 1);
            self.buf.lines[r].extend(next);
        }
    }

    fn move_cursor(&mut self, key: Key) {
        let b = &mut self.buf;
        match key {
            Key::Left => {
                if b.col > 0 {
                    b.col -= 1;
                } else if b.row > 0 {
                    b.row -= 1;
                    b.col = b.line_len();
                }
            }
            Key::Right => {
                if b.col < b.line_len() {
                    b.col += 1;
                } else if b.row + 1 < b.lines.len() {
                    b.row += 1;
                    b.col = 0;
                }
            }
            Key::Up => b.row = b.row.saturating_sub(1),
            Key::Down => b.row = (b.row + 1).min(b.lines.len() - 1),
            Key::Home => {
                // First press goes to the first non-blank, a second to the very start.
                let first = b.lines[b.row]
                    .iter()
                    .position(|c| !c.is_whitespace())
                    .unwrap_or(0);
                b.col = if b.col == first { 0 } else { first };
            }
            Key::End => b.col = b.line_len(),
            Key::PageUp => b.row = b.row.saturating_sub(self.view_h.get().max(1)),
            Key::PageDown => {
                b.row = (b.row + self.view_h.get().max(1)).min(b.lines.len() - 1);
            }
            _ => {}
        }
        b.clamp();
        self.last = None;
    }

    /// Moves to the next match of `needle` after the cursor, wrapping round. True when found.
    fn find_next(&mut self, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        let n = self.buf.lines.len();
        for step in 0..=n {
            let row = (self.buf.row + step) % n;
            let line: String = self.buf.lines[row].iter().collect();
            // On the first visit start after the cursor; on the last (the same line again) stop
            // at the cursor.
            let start_char = if step == 0 { self.buf.col + 1 } else { 0 };
            let start_byte = line
                .char_indices()
                .nth(start_char)
                .map_or(line.len(), |(i, _)| i);
            if start_byte > line.len() {
                continue;
            }
            let hay = if step == n {
                let end_byte = line
                    .char_indices()
                    .nth(self.buf.col + needle.chars().count())
                    .map_or(line.len(), |(i, _)| i);
                &line[..end_byte]
            } else {
                &line[start_byte..]
            };
            let base = if step == n { 0 } else { start_byte };
            if let Some(i) = hay.find(needle) {
                self.buf.row = row;
                self.buf.col = line[..base + i].chars().count();
                self.last = None;
                return true;
            }
        }
        false
    }

    fn key_in_find(&mut self, key: Key) -> Action {
        let Some(q) = &mut self.find else {
            return Action::None;
        };
        match key {
            Key::Esc => {
                self.find = None;
            }
            Key::Char(c) => q.push(c),
            Key::Backspace => {
                q.pop();
            }
            Key::Enter => {
                let needle = q.clone();
                self.last_find = needle.clone();
                self.message = if self.find_next(&needle) {
                    String::new()
                } else {
                    format!("\"{needle}\" was not found.")
                };
            }
            _ => {}
        }
        Action::None
    }

    fn key_in_files(&mut self, key: Key) -> Action {
        if !matches!(key, Key::Enter) {
            self.discard = None;
        }
        let len = self.files.len();
        match key {
            Key::Up => self.tree.step(-1, len),
            Key::Down => self.tree.step(1, len),
            Key::PageUp => self.tree.step(-10, len),
            Key::PageDown => self.tree.step(10, len),
            Key::Home => self.tree.jump(false, len),
            Key::End => self.tree.jump(true, len),
            Key::Tab | Key::Right if self.path.is_some() => self.focus = Focus::Text,
            Key::Ctrl('r') => {
                return Action::Call {
                    capability: "code.files".into(),
                    args: vec![],
                }
            }
            Key::Enter => {
                let Some(file) = self.files.get(self.tree.selected).cloned() else {
                    return Action::None;
                };
                let other = self.path.as_deref() != Some(file.as_str());
                if self.dirty && other && self.discard != Some(self.tree.selected) {
                    self.discard = Some(self.tree.selected);
                    self.message = format!(
                        "{} has changes that were not sent. Press Enter again to drop them, or Tab and Ctrl-S to send them first.",
                        self.path.as_deref().unwrap_or("The file")
                    );
                    return Action::None;
                }
                self.discard = None;
                return Action::Call {
                    capability: "code.read".into(),
                    args: vec![("path".into(), file)],
                };
            }
            _ => {}
        }
        Action::None
    }

    fn key_in_text(&mut self, key: Key) -> Action {
        match key {
            Key::Char(c) => self.insert(c),
            Key::Tab => self.insert_tab(),
            Key::Enter => self.newline(),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown => self.move_cursor(key),
            Key::Esc => self.focus = Focus::Files,
            Key::Ctrl('z') => self.undo(),
            Key::Ctrl('y') => self.redo(),
            Key::Ctrl('f') => {
                self.find = Some(self.last_find.clone());
            }
            Key::Ctrl('s') => {
                let Some(path) = self.path.clone() else {
                    return Action::None;
                };
                if !self.dirty {
                    self.message = "Nothing has changed.".into();
                    return Action::None;
                }
                self.message = "Sent for review. Approve it under Review to write the file.".into();
                return Action::Call {
                    capability: "code.edit".into(),
                    args: vec![("path".into(), path), ("text".into(), self.text())],
                };
            }
            _ => {}
        }
        Action::None
    }

    fn draw_files(&self, g: &mut Grid, r: Rect) {
        let focused = self.focus == Focus::Files;
        let edge = if focused { theme::ACCENT } else { theme::DIM };
        g.boxed(r, Some("Files"), edge, theme::BG);
        let inner = r.inset(1);
        if inner.h == 0 || inner.w == 0 {
            return;
        }
        if self.files.is_empty() {
            g.text_fit(
                inner.x,
                inner.y,
                inner.w,
                "No files. Ctrl-R lists them.",
                theme::DIM,
                theme::BG,
            );
            return;
        }
        let top = self.tree.window(inner.h);
        for (i, f) in self.files.iter().enumerate().skip(top).take(inner.h) {
            let selected = i == self.tree.selected;
            let open = self.path.as_deref() == Some(f.as_str());
            let bg = if selected && focused {
                theme::SELECT
            } else {
                theme::BG
            };
            let fg = if open { theme::ACCENT } else { theme::TEXT };
            let mark = if open {
                if self.dirty {
                    '*'
                } else {
                    '>'
                }
            } else {
                ' '
            };
            let line = format!("{mark}{f}");
            g.text_fit(inner.x, inner.y + i - top, inner.w, &line, fg, bg);
        }
    }

    fn draw_text(&self, g: &mut Grid, r: Rect) {
        let focused = self.focus == Focus::Text;
        let edge = if focused { theme::ACCENT } else { theme::DIM };
        let title = match &self.path {
            Some(p) if self.dirty => format!("{p} *"),
            Some(p) => p.clone(),
            None => "Editor".to_string(),
        };
        g.boxed(r, Some(&title), edge, theme::BG);
        let inner = r.inset(1);
        if inner.w <= GUTTER + 2 || inner.h < 2 {
            return;
        }
        let Some(path) = &self.path else {
            g.text_fit(
                inner.x + 1,
                inner.y,
                inner.w - 1,
                "Choose a file on the left and press Enter.",
                theme::DIM,
                theme::BG,
            );
            return;
        };
        let lang = Lang::of(path);
        let text_w = inner.w - GUTTER;
        let view_h = inner.h;
        self.view_h.set(view_h);
        // Keep the cursor in view.
        let (mut top, mut left) = (self.top.get(), self.left.get());
        if self.buf.row < top {
            top = self.buf.row;
        } else if self.buf.row >= top + view_h {
            top = self.buf.row + 1 - view_h;
        }
        if self.buf.col < left {
            left = self.buf.col;
        } else if self.buf.col >= left + text_w {
            left = self.buf.col + 1 - text_w;
        }
        self.top.set(top);
        self.left.set(left);
        for i in 0..view_h {
            let row = top + i;
            let y = inner.y + i;
            let Some(line) = self.buf.lines.get(row) else {
                break;
            };
            let current = row == self.buf.row;
            let num_fg = if current { theme::ACCENT } else { theme::DIM };
            g.text(inner.x, y, &format!("{:>4} ", row + 1), num_fg, theme::BG);
            let kinds = classify(lang, line);
            for (k, (ch, kind)) in line.iter().zip(&kinds).enumerate().skip(left).take(text_w) {
                g.put(
                    inner.x + GUTTER + k - left,
                    y,
                    *ch,
                    kind.colour(),
                    theme::BG,
                );
            }
            if focused && current {
                let cx = inner.x + GUTTER + self.buf.col - left;
                let under = line.get(self.buf.col).copied().unwrap_or(' ');
                let under = if under.is_control() { ' ' } else { under };
                g.put(cx, y, under, theme::BG, theme::ACCENT);
            }
        }
    }
}

impl Screen for EditorScreen {
    fn on_key(&mut self, key: Key) -> Action {
        if self.find.is_some() {
            return self.key_in_find(key);
        }
        match self.focus {
            Focus::Files => self.key_in_files(key),
            Focus::Text => self.key_in_text(key),
        }
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.fill(area, theme::BG);
        if area.h < 5 || area.w < 24 {
            g.text(area.x, area.y, "Editor", theme::DIM, theme::BG);
            return;
        }
        let body = Rect::new(area.x, area.y, area.w, area.h - 1);
        let status_y = area.y + area.h - 1;
        let tree_w = (body.w / 4).clamp(16, 34);
        if body.w >= 64 {
            self.draw_files(g, Rect::new(body.x, body.y, tree_w, body.h));
            self.draw_text(
                g,
                Rect::new(body.x + tree_w, body.y, body.w - tree_w, body.h),
            );
        } else if self.focus == Focus::Files {
            self.draw_files(g, body);
        } else {
            self.draw_text(g, body);
        }
        if let Some(q) = &self.find {
            let line = format!("Find: {q}_   Enter next  Esc close");
            g.text_fit(area.x, status_y, area.w, &line, theme::ACCENT, theme::BG);
        } else if !self.message.is_empty() {
            g.text_fit(
                area.x,
                status_y,
                area.w,
                &format!(" {}", self.message),
                theme::WARN,
                theme::BG,
            );
        } else if let Some(p) = &self.path {
            let (l, c) = self.position();
            let line = format!(
                " {}   Ln {l}, Col {c}   {} line(s)",
                Lang::of(p).name(),
                self.buf.lines.len()
            );
            g.text_fit(area.x, status_y, area.w, &line, theme::DIM, theme::BG);
        }
    }

    fn hints(&self) -> String {
        if self.find.is_some() {
            return "Enter next  Esc close".into();
        }
        match self.focus {
            Focus::Files => "Enter open  Up/Down choose  Tab to text  Ctrl-R refresh".into(),
            Focus::Text => {
                "Ctrl-S send for review  Ctrl-Z undo  Ctrl-Y redo  Ctrl-F find  Esc files".into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> EditorScreen {
        let mut e = EditorScreen::default();
        e.set_files(vec![
            "src/lib.rs".into(),
            "Cargo.toml".into(),
            "README.md".into(),
        ]);
        e.load("src/lib.rs", text).unwrap();
        e
    }

    fn typ(e: &mut EditorScreen, s: &str) {
        for c in s.chars() {
            e.on_key(if c == '\n' { Key::Enter } else { Key::Char(c) });
        }
    }

    fn drawn(e: &EditorScreen, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let a = g.area();
        e.draw(&mut g, a);
        g.to_text()
    }

    #[test]
    fn text_round_trips_with_its_line_endings_and_final_newline() {
        for t in [
            "",
            "a\n",
            "a",
            "a\nb\n",
            "a\r\nb\r\n",
            "a\n\n\nb\n",
            "\n",
            "x\ny",
        ] {
            let e = editor(t);
            assert_eq!(e.text(), t, "{t:?}");
        }
    }

    #[test]
    fn typing_inserts_at_the_cursor_and_marks_the_file_changed() {
        let mut e = editor("fn main() {}\n");
        assert!(!e.is_dirty());
        e.on_key(Key::End);
        e.on_key(Key::Left);
        typ(&mut e, "\n    x();\n");
        assert_eq!(
            e.text(),
            "fn main() {\n    x();\n    }\n",
            "the new line keeps the indent"
        );
        assert!(e.is_dirty());
    }

    #[test]
    fn enter_keeps_the_indent_of_the_line() {
        let mut e = editor("    a\n");
        e.on_key(Key::End);
        e.on_key(Key::Enter);
        typ(&mut e, "b");
        assert_eq!(e.text(), "    a\n    b\n");
    }

    #[test]
    fn backspace_and_delete_join_lines_and_stop_at_the_ends() {
        let mut e = editor("ab\ncd\n");
        e.on_key(Key::Backspace); // at the very start: nothing
        assert_eq!(e.text(), "ab\ncd\n");
        e.on_key(Key::Down);
        e.on_key(Key::Home);
        e.on_key(Key::Backspace);
        assert_eq!(e.text(), "abcd\n");
        assert_eq!(e.position(), (1, 3));
        e.on_key(Key::Delete);
        assert_eq!(e.text(), "abd\n");
        e.on_key(Key::End);
        e.on_key(Key::Delete); // at the very end: nothing
        assert_eq!(e.text(), "abd\n");
    }

    #[test]
    fn tab_inserts_spaces_and_cursor_keys_stay_inside_the_text() {
        let mut e = editor("a\nb\n");
        e.on_key(Key::Tab);
        assert_eq!(e.text(), "    a\nb\n");
        for _ in 0..20 {
            e.on_key(Key::Down);
        }
        assert_eq!(e.position().0, 2);
        for _ in 0..20 {
            e.on_key(Key::Right);
        }
        assert_eq!(e.position(), (2, 2));
        for _ in 0..20 {
            e.on_key(Key::Left);
            e.on_key(Key::Up);
        }
        assert_eq!(e.position(), (1, 1));
    }

    #[test]
    fn a_run_of_typing_is_one_undo_step_and_redo_brings_it_back() {
        let mut e = editor("x\n");
        e.on_key(Key::End);
        typ(&mut e, "abc");
        e.on_key(Key::Enter);
        typ(&mut e, "def");
        assert_eq!(e.text(), "xabc\ndef\n");
        e.on_key(Key::Ctrl('z'));
        assert_eq!(e.text(), "xabc\n\n");
        e.on_key(Key::Ctrl('z'));
        assert_eq!(e.text(), "xabc\n");
        e.on_key(Key::Ctrl('z'));
        assert_eq!(e.text(), "x\n");
        e.on_key(Key::Ctrl('z'));
        assert!(e.message.contains("Nothing to undo"));
        e.on_key(Key::Ctrl('y'));
        e.on_key(Key::Ctrl('y'));
        e.on_key(Key::Ctrl('y'));
        assert_eq!(e.text(), "xabc\ndef\n");
        e.on_key(Key::Ctrl('y'));
        assert!(e.message.contains("Nothing to redo"));
    }

    #[test]
    fn a_new_edit_after_undo_clears_redo() {
        let mut e = editor("a\n");
        typ(&mut e, "b");
        e.on_key(Key::Ctrl('z'));
        typ(&mut e, "c");
        e.on_key(Key::Ctrl('y'));
        assert!(e.message.contains("Nothing to redo"));
        assert_eq!(e.text(), "ca\n");
    }

    #[test]
    fn undo_depth_is_bounded() {
        let mut e = editor("");
        for _ in 0..(MAX_UNDO + 50) {
            e.on_key(Key::Enter);
        }
        assert!(e.undo.len() <= MAX_UNDO);
        for _ in 0..(MAX_UNDO + 10) {
            e.on_key(Key::Ctrl('z'));
        }
        assert!(e.text().lines().count() >= 50, "older steps are gone");
    }

    #[test]
    fn find_moves_to_the_next_match_wraps_and_reports_misses() {
        let mut e = editor("one two\nthree two\nfour\n");
        e.on_key(Key::Ctrl('f'));
        typ(&mut e, "two");
        e.on_key(Key::Enter);
        assert_eq!(e.position(), (1, 5));
        e.on_key(Key::Enter);
        assert_eq!(e.position(), (2, 7));
        e.on_key(Key::Enter);
        assert_eq!(e.position(), (1, 5), "wrapped round to the first");
        e.on_key(Key::Esc);
        e.on_key(Key::Ctrl('f'));
        assert!(
            drawn(&e, 80, 12).contains("Find: two"),
            "the last search is offered again"
        );
        for _ in 0..3 {
            e.on_key(Key::Backspace);
        }
        typ(&mut e, "zzz");
        e.on_key(Key::Enter);
        assert!(e.message.contains("not found"), "{}", e.message);
        assert_eq!(e.position(), (1, 5), "the cursor stays where it was");
    }

    #[test]
    fn find_on_a_single_match_comes_back_to_it_and_handles_multibyte_text() {
        let mut e = editor("héllo wörld\n");
        e.on_key(Key::Ctrl('f'));
        typ(&mut e, "wörld");
        e.on_key(Key::Enter);
        assert_eq!(e.position(), (1, 7));
        e.on_key(Key::Enter);
        assert_eq!(
            e.position(),
            (1, 7),
            "the only match is found again after wrapping"
        );
        assert!(e.message.is_empty());
    }

    #[test]
    fn typing_in_find_does_not_edit_the_text() {
        let mut e = editor("abc\n");
        e.on_key(Key::Ctrl('f'));
        typ(&mut e, "b");
        assert_eq!(e.text(), "abc\n");
        assert!(!e.is_dirty());
    }

    #[test]
    fn saving_asks_for_a_proposal_with_the_whole_text_and_only_when_changed() {
        let mut e = editor("a\n");
        assert_eq!(e.on_key(Key::Ctrl('s')), Action::None, "nothing changed");
        typ(&mut e, "b");
        assert_eq!(
            e.on_key(Key::Ctrl('s')),
            Action::Call {
                capability: "code.edit".into(),
                args: vec![
                    ("path".into(), "src/lib.rs".into()),
                    ("text".into(), "ba\n".into())
                ],
            }
        );
        assert!(e.is_dirty(), "stays marked until the controller confirms");
        e.mark_saved();
        assert!(!e.is_dirty());
        assert_eq!(EditorScreen::default().on_key(Key::Ctrl('s')), Action::None);
    }

    #[test]
    fn opening_a_file_asks_for_it_and_refuses_to_lose_unsent_changes_without_a_second_enter() {
        let mut e = editor("a\n");
        e.on_key(Key::Esc);
        assert_eq!(e.focus(), Focus::Files);
        e.on_key(Key::Down); // Cargo.toml is first after sorting; lib.rs second
        let first = e.on_key(Key::Enter);
        assert!(matches!(&first, Action::Call { capability, .. } if capability == "code.read"));
        // Now with unsent changes.
        e.on_key(Key::Tab);
        typ(&mut e, "z");
        e.on_key(Key::Esc);
        e.on_key(Key::Up);
        let held = e.on_key(Key::Enter);
        assert_eq!(held, Action::None);
        assert!(e.message.contains("not sent"), "{}", e.message);
        let second = e.on_key(Key::Enter);
        assert!(matches!(second, Action::Call { .. }), "{second:?}");
        // Moving elsewhere cancels the pending drop.
        e.on_key(Key::Down);
        let again = e.on_key(Key::Enter);
        assert_eq!(again, Action::None, "asks again for a different file");
    }

    #[test]
    fn reopening_the_open_file_does_not_need_confirmation() {
        let mut e = editor("a\n");
        typ(&mut e, "z");
        e.on_key(Key::Esc);
        e.on_key(Key::End); // lib.rs, the open file, is last
        let a = e.on_key(Key::Enter);
        assert!(matches!(a, Action::Call { .. }), "{a:?}");
    }

    #[test]
    fn refresh_asks_for_the_file_list() {
        let mut e = EditorScreen::default();
        assert_eq!(
            e.on_key(Key::Ctrl('r')),
            Action::Call {
                capability: "code.files".into(),
                args: vec![]
            }
        );
    }

    #[test]
    fn files_are_sorted_and_deduplicated_and_the_cursor_follows_the_list() {
        let mut e = EditorScreen::default();
        e.set_files(vec!["b".into(), "a".into(), "b".into()]);
        assert_eq!(e.files, vec!["a", "b"]);
        e.on_key(Key::End);
        e.set_files(vec!["a".into()]);
        assert_eq!(e.tree.selected, 0);
    }

    #[test]
    fn files_that_are_too_large_or_odd_are_refused_and_the_open_file_stays() {
        let mut e = editor("keep\n");
        assert!(e.load("big.txt", &"x".repeat(MAX_BYTES + 1)).is_err());
        assert!(e.load("long.txt", &"y".repeat(MAX_LINE + 1)).is_err());
        assert!(e.load("many.txt", &"\n".repeat(MAX_LINES + 1)).is_err());
        assert_eq!(e.path(), Some("src/lib.rs"));
        assert_eq!(e.text(), "keep\n");
    }

    #[test]
    fn a_line_cannot_grow_past_the_limit() {
        let mut e = editor(&"a".repeat(MAX_LINE));
        e.on_key(Key::End);
        e.on_key(Key::Char('b'));
        assert_eq!(e.text().trim_end().len(), MAX_LINE);
        assert!(e.message.contains("longer"));
        assert!(!e.is_dirty());
    }

    #[test]
    fn the_screen_shows_numbers_colours_the_cursor_and_the_position() {
        let mut e = editor("pub fn f() {}\n// note\n");
        let t = drawn(&e, 90, 14);
        assert!(t.contains("src/lib.rs") && t.contains("Files"), "{t}");
        assert!(
            t.contains("   1  pub fn f()") && t.contains("   2  // note"),
            "{t}"
        );
        assert!(t.contains("Rust") && t.contains("Ln 1, Col 1"), "{t}");
        let mut g = Grid::new(90, 14);
        let a = g.area();
        e.draw(&mut g, a);
        let cell = |x, y| g.get(x, y).unwrap();
        // Find the 'p' of "pub" and check it is a keyword colour with the cursor on it.
        let (x, y) = (0..90)
            .flat_map(|x| (0..14).map(move |y| (x, y)))
            .find(|&(x, y)| cell(x, y).ch == 'p' && cell(x + 1, y).ch == 'u')
            .unwrap();
        assert_eq!(cell(x, y).bg, theme::ACCENT, "the cursor");
        assert_eq!(cell(x + 4, y).fg, theme::ACCENT, "keyword fn");
        e.on_key(Key::Down);
        let mut g2 = Grid::new(90, 14);
        e.draw(&mut g2, a);
        assert_eq!(
            g2.get(x + 1, y + 1).unwrap().fg,
            theme::DIM,
            "comment colour"
        );
    }

    #[test]
    fn long_files_and_lines_scroll_to_keep_the_cursor_visible() {
        let text: String = (0..200).map(|i| format!("line{i}\n")).collect();
        let mut e = editor(&text);
        for _ in 0..150 {
            e.on_key(Key::Down);
        }
        let t = drawn(&e, 90, 14);
        assert!(t.contains("line150"), "{t}");
        assert!(!t.contains("line0\n") && !t.contains("   1 line0"), "{t}");
        let wide = "w".repeat(300);
        let mut e = editor(&format!("{wide}end\n"));
        e.on_key(Key::End);
        assert!(drawn(&e, 90, 14).contains("end"));
        e.on_key(Key::PageDown);
        e.on_key(Key::PageUp);
        assert_eq!(e.position().0, 1);
    }

    #[test]
    fn narrow_screens_show_one_pane_at_a_time_and_tiny_ones_do_not_panic() {
        let mut e = editor("abc\n");
        e.on_key(Key::Esc);
        assert!(drawn(&e, 40, 12).contains("Files"));
        e.on_key(Key::Tab);
        let t = drawn(&e, 40, 12);
        assert!(t.contains("abc") && !t.contains("Files"), "{t}");
        for (w, h) in [(0, 0), (1, 1), (10, 3), (24, 5), (200, 60)] {
            let _ = drawn(&e, w, h);
            let _ = drawn(&EditorScreen::default(), w, h);
        }
    }

    #[test]
    fn with_no_file_open_the_text_pane_says_what_to_do() {
        let t = drawn(&EditorScreen::default(), 90, 12);
        assert!(t.contains("Choose a file") && t.contains("No files"), "{t}");
    }

    #[test]
    fn hints_follow_the_focus() {
        let mut e = editor("a\n");
        assert!(e.hints().contains("Ctrl-S"));
        e.on_key(Key::Esc);
        assert!(e.hints().contains("Enter open"));
    }
}
