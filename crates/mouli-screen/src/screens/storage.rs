//! Storage: what is on the data disk, how much room is left, and removing a model.
//!
//! The list is filled by the host (it owns the disk). Removing needs two presses of `d` on the
//! same row, so a stray key never deletes a model; the screen only *asks* (`Action::DeleteStorage`)
//! and the host decides whether the row may be removed.

use crate::grid::{fit, theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// What kind of thing a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    /// A model file (can be removed here).
    Model,
    /// A Code project folder.
    Project,
    /// The activity record.
    Record,
    /// Anything else on the disk that MOULI keeps (settings and so on).
    Other,
}

impl StorageKind {
    fn label(self) -> &'static str {
        match self {
            StorageKind::Model => "model",
            StorageKind::Project => "project",
            StorageKind::Record => "record",
            StorageKind::Other => "other",
        }
    }
}

/// One thing on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRow {
    /// File or folder name.
    pub name: String,
    /// Size in bytes (folders: everything inside, up to a limit).
    pub bytes: u64,
    /// What it is.
    pub kind: StorageKind,
}

/// The Storage screen.
#[derive(Debug, Clone, Default)]
pub struct StorageScreen {
    /// What is on the disk.
    pub rows: Vec<StorageRow>,
    /// Free bytes on the disk, if known.
    pub free: Option<u64>,
    /// Total bytes on the disk, if known.
    pub total: Option<u64>,
    /// Where the disk is, for display (for example `MOULI-DATA`), empty when there is none.
    pub location: String,
    /// A message under the list.
    pub message: String,
    /// The selection.
    pub cursor: Cursor,
    /// The row waiting for a second `d`.
    confirm: Option<usize>,
}

/// Bytes as a short readable size: `512 B`, `3.4 MB`, `2.1 GB`.
#[must_use]
pub fn size_text(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    // One decimal place, rounded, using integer arithmetic.
    let tenths = |n: u64, unit: u64| n.saturating_mul(10).saturating_add(unit / 2) / unit;
    if bytes >= GB {
        let t = tenths(bytes, GB);
        format!("{}.{} GB", t / 10, t % 10)
    } else if bytes >= MB {
        let t = tenths(bytes, MB);
        format!("{}.{} MB", t / 10, t % 10)
    } else if bytes >= KB {
        let t = tenths(bytes, KB);
        format!("{}.{} KB", t / 10, t % 10)
    } else {
        format!("{bytes} B")
    }
}

impl StorageScreen {
    /// The selected row.
    #[must_use]
    pub fn selected(&self) -> Option<&StorageRow> {
        self.rows.get(self.cursor.selected)
    }

    /// Replaces the list (after a scan or a removal) and cancels any pending removal.
    pub fn set(
        &mut self,
        rows: Vec<StorageRow>,
        free: Option<u64>,
        total: Option<u64>,
        location: &str,
    ) {
        self.rows = rows;
        self.free = free;
        self.total = total;
        self.location = location.to_string();
        self.confirm = None;
        self.cursor.clamp(self.rows.len());
    }

    /// Bytes used by what is listed.
    #[must_use]
    pub fn listed_bytes(&self) -> u64 {
        self.rows
            .iter()
            .fold(0u64, |a, r| a.saturating_add(r.bytes))
    }
}

impl Screen for StorageScreen {
    fn on_key(&mut self, key: Key) -> Action {
        let len = self.rows.len();
        match key {
            Key::Up | Key::Char('k') => {
                self.cursor.step(-1, len);
                self.confirm = None;
            }
            Key::Down | Key::Char('j') => {
                self.cursor.step(1, len);
                self.confirm = None;
            }
            Key::PageUp => {
                self.cursor.step(-8, len);
                self.confirm = None;
            }
            Key::PageDown => {
                self.cursor.step(8, len);
                self.confirm = None;
            }
            Key::Home => {
                self.cursor.jump(false, len);
                self.confirm = None;
            }
            Key::End => {
                self.cursor.jump(true, len);
                self.confirm = None;
            }
            Key::Char('r') => {
                self.confirm = None;
                self.message.clear();
                return Action::RescanStorage;
            }
            Key::Esc => {
                if self.confirm.take().is_some() {
                    self.message = "Not removed.".into();
                }
            }
            Key::Char('d') | Key::Delete => {
                let i = self.cursor.selected;
                match self.rows.get(i) {
                    Some(r) if r.kind == StorageKind::Model => {
                        if self.confirm == Some(i) {
                            self.confirm = None;
                            return Action::DeleteStorage(i);
                        }
                        self.confirm = Some(i);
                        self.message = format!(
                            "Press d again to remove {} ({}). Esc cancels.",
                            r.name,
                            size_text(r.bytes)
                        );
                    }
                    Some(r) => {
                        self.message =
                            format!("{} cannot be removed here; only models can.", r.name);
                    }
                    None => {}
                }
            }
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.boxed(area, Some("Storage"), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 24 || inner.h < 6 {
            return;
        }
        let place = if self.location.is_empty() {
            "No data disk: nothing is kept after power-off".to_string()
        } else {
            format!("Disk: {}", self.location)
        };
        g.text_fit(
            inner.x + 1,
            inner.y,
            inner.w - 2,
            &place,
            theme::DIM,
            theme::BG,
        );
        let space = match (self.free, self.total) {
            (Some(f), Some(t)) if t > 0 => {
                // Whole percent used, from integers.
                let used = t.saturating_sub(f);
                let pct = used.saturating_mul(100).saturating_add(t / 2) / t;
                format!("{} free of {} ({}% used)", size_text(f), size_text(t), pct)
            }
            (Some(f), _) => format!("{} free", size_text(f)),
            _ => "Free space unknown".to_string(),
        };
        g.text_fit(
            inner.x + 1,
            inner.y + 1,
            inner.w - 2,
            &space,
            theme::TEXT,
            theme::BG,
        );
        let list_top = inner.y + 3;
        let list_h = inner.h.saturating_sub(6);
        if self.rows.is_empty() {
            g.text(
                inner.x + 2,
                list_top,
                "Nothing stored yet.",
                theme::DIM,
                theme::BG,
            );
        } else {
            let top = self.cursor.window(list_h);
            let name_w = inner.w.saturating_sub(24).clamp(8, 48);
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
                g.text_fit(
                    inner.x + 3,
                    y,
                    name_w,
                    &fit(&row.name, name_w),
                    theme::TEXT,
                    bg,
                );
                let col = inner.x + 4 + name_w;
                g.text(
                    col,
                    y,
                    &format!("{:>9}", size_text(row.bytes)),
                    theme::DIM,
                    bg,
                );
                g.text(col + 10, y, row.kind.label(), theme::DIM, bg);
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
        g.text_fit(
            inner.x + 1,
            foot + 1,
            inner.w - 2,
            &format!("Listed: {}", size_text(self.listed_bytes())),
            theme::DIM,
            theme::BG,
        );
    }

    fn hints(&self) -> String {
        "Up/Down choose  d remove a model (twice)  r rescan".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, bytes: u64, kind: StorageKind) -> StorageRow {
        StorageRow {
            name: name.into(),
            bytes,
            kind,
        }
    }

    fn screen() -> StorageScreen {
        let mut s = StorageScreen::default();
        s.set(
            vec![
                row("tiny-q4.gguf", 700 << 20, StorageKind::Model),
                row("big-q8.gguf", 9 << 30, StorageKind::Model),
                row("projects", 3 << 20, StorageKind::Project),
                row("graph.log", 40 << 10, StorageKind::Record),
            ],
            Some(50 << 30),
            Some(64 << 30),
            "MOULI-DATA",
        );
        s
    }

    fn drawn(s: &StorageScreen, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let area = g.area();
        s.draw(&mut g, area);
        g.to_text()
    }

    #[test]
    fn sizes_read_naturally() {
        for (b, t) in [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1.0 KB"),
            (1536, "1.5 KB"),
            (1 << 20, "1.0 MB"),
            (700 << 20, "700.0 MB"),
            (1 << 30, "1.0 GB"),
            ((5 << 30) + (1 << 29), "5.5 GB"),
        ] {
            assert_eq!(size_text(b), t, "{b}");
        }
    }

    #[test]
    fn the_largest_size_does_not_overflow() {
        assert!(size_text(u64::MAX).ends_with(" GB"));
    }

    #[test]
    fn removing_a_model_needs_two_presses_on_the_same_row() {
        let mut s = screen();
        assert_eq!(s.on_key(Key::Char('d')), Action::None);
        assert!(
            s.message.contains("Press d again") && s.message.contains("tiny-q4.gguf"),
            "{}",
            s.message
        );
        assert_eq!(s.on_key(Key::Char('d')), Action::DeleteStorage(0));
        // The confirmation is used up.
        assert_eq!(s.on_key(Key::Char('d')), Action::None);
        // Moving away cancels it.
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::Char('d')), Action::None);
        s.on_key(Key::Up);
        assert_eq!(
            s.on_key(Key::Char('d')),
            Action::None,
            "a different row needs its own first press"
        );
        // Escape cancels.
        s.on_key(Key::Esc);
        assert!(s.message.contains("Not removed"));
        assert_eq!(s.on_key(Key::Char('d')), Action::None);
        assert_eq!(s.on_key(Key::Delete), Action::DeleteStorage(0));
    }

    #[test]
    fn only_models_can_be_removed() {
        let mut s = screen();
        s.on_key(Key::End);
        assert_eq!(s.selected().unwrap().kind, StorageKind::Record);
        assert_eq!(s.on_key(Key::Char('d')), Action::None);
        assert_eq!(s.on_key(Key::Char('d')), Action::None);
        assert!(s.message.contains("only models can"), "{}", s.message);
        let mut empty = StorageScreen::default();
        assert_eq!(empty.on_key(Key::Char('d')), Action::None);
        assert_eq!(empty.on_key(Key::Char('d')), Action::None);
    }

    #[test]
    fn rescan_and_replacing_the_list_cancel_a_pending_removal() {
        let mut s = screen();
        s.on_key(Key::Char('d'));
        assert_eq!(s.on_key(Key::Char('r')), Action::RescanStorage);
        assert_eq!(
            s.on_key(Key::Char('d')),
            Action::None,
            "needs a fresh first press"
        );
        s.on_key(Key::Char('d'));
        s.set(vec![row("x.gguf", 1, StorageKind::Model)], None, None, "");
        assert_eq!(s.on_key(Key::Char('d')), Action::None);
        // The selection stays inside a shorter list.
        let mut s = screen();
        s.on_key(Key::End);
        s.set(
            vec![row("only.gguf", 1, StorageKind::Model)],
            None,
            None,
            "",
        );
        assert_eq!(s.selected().unwrap().name, "only.gguf");
    }

    #[test]
    fn it_shows_the_disk_the_space_and_the_rows() {
        let t = drawn(&screen(), 80, 16);
        assert!(
            t.contains("Storage") && t.contains("Disk: MOULI-DATA"),
            "{t}"
        );
        assert!(t.contains("50.0 GB free of 64.0 GB (22% used)"), "{t}");
        assert!(
            t.contains("tiny-q4.gguf") && t.contains("700.0 MB") && t.contains("model"),
            "{t}"
        );
        assert!(t.contains("graph.log") && t.contains("record"), "{t}");
        assert!(t.contains("Listed:"), "{t}");
        let none = drawn(&StorageScreen::default(), 80, 14);
        assert!(
            none.contains("No data disk")
                && none.contains("Free space unknown")
                && none.contains("Nothing stored yet"),
            "{none}"
        );
        // Tiny areas do not panic.
        for (w, h) in [(1, 1), (10, 3), (24, 6), (30, 8)] {
            let _ = drawn(&screen(), w, h);
        }
    }

    #[test]
    fn the_pending_removal_is_shown_in_the_footer() {
        let mut s = screen();
        s.on_key(Key::Char('d'));
        assert!(drawn(&s, 90, 16).contains("Press d again to remove tiny-q4.gguf"));
    }
}
