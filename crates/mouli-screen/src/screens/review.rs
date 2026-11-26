//! Reviewing a proposed code change.

use crate::grid::{theme, Color, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// Where the change stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatchState {
    /// Proposed; nothing has changed on disk yet.
    #[default]
    Proposed,
    /// Applied to the files.
    Applied,
    /// Put back as it was.
    RolledBack,
    /// Thrown away.
    Rejected,
}

impl PatchState {
    /// The word shown on screen.
    pub const fn label(self) -> &'static str {
        match self {
            PatchState::Proposed => "proposed",
            PatchState::Applied => "applied",
            PatchState::RolledBack => "rolled back",
            PatchState::Rejected => "rejected",
        }
    }
}

/// One file in the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The path.
    pub path: String,
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// The diff text of this file (unified format, one line per element).
    pub diff: Vec<String>,
}

/// A check that ran on the result (tests, quality rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// Whether it passed.
    pub passed: bool,
}

/// The code-review screen.
#[derive(Debug, Clone, Default)]
pub struct Review {
    /// A one-line description of the change.
    pub summary: String,
    /// Where the change stands.
    pub state: PatchState,
    /// The files, in order.
    pub files: Vec<FileChange>,
    /// The checks that ran.
    pub checks: Vec<Check>,
    /// The selected file.
    pub cursor: Cursor,
    /// How far the diff is scrolled.
    pub scroll: usize,
    /// A message for the person.
    pub message: String,
    /// Whether the diff is shown as old and new side by side (when the screen is wide enough).
    pub side_by_side: bool,
}

/// Narrowest screen (in columns) that can show old and new side by side.
pub const SIDE_BY_SIDE_MIN_WIDTH: usize = 70;

/// What a row of the side-by-side view holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A file or hunk header, shown across both sides.
    Header,
    /// A line that is the same on both sides.
    Same,
    /// A line that was changed: the old text on the left, the new on the right.
    Changed,
}

/// One row of the side-by-side view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideRow {
    /// What kind of row.
    pub kind: RowKind,
    /// The old text (without the leading `-` or space); `None` for an added line.
    pub left: Option<String>,
    /// The new text; `None` for a removed line.
    pub right: Option<String>,
}

/// Turns unified-diff lines into side-by-side rows. Within a hunk a run of removed lines is
/// paired, in order, with the run of added lines that follows it; the longer run's extra lines
/// have nothing opposite. A file header (`---`/`+++`) is only a header before the first hunk, so
/// a removed line whose text begins with `--` is not mistaken for one.
#[must_use]
pub fn side_rows(diff: &[String]) -> Vec<SideRow> {
    let mut rows = Vec::new();
    let mut in_hunk = false;
    let mut removed: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();
    let flush = |rows: &mut Vec<SideRow>, removed: &mut Vec<String>, added: &mut Vec<String>| {
        for i in 0..removed.len().max(added.len()) {
            rows.push(SideRow {
                kind: RowKind::Changed,
                left: removed.get(i).cloned(),
                right: added.get(i).cloned(),
            });
        }
        removed.clear();
        added.clear();
    };
    for line in diff {
        if line.starts_with("@@") {
            flush(&mut rows, &mut removed, &mut added);
            in_hunk = true;
            rows.push(SideRow {
                kind: RowKind::Header,
                left: Some(line.clone()),
                right: None,
            });
        } else if !in_hunk {
            rows.push(SideRow {
                kind: RowKind::Header,
                left: Some(line.clone()),
                right: None,
            });
        } else if let Some(t) = line.strip_prefix('-') {
            if !added.is_empty() {
                flush(&mut rows, &mut removed, &mut added);
            }
            removed.push(t.to_string());
        } else if let Some(t) = line.strip_prefix('+') {
            added.push(t.to_string());
        } else if line.starts_with('\\') {
            // "\ No newline at end of file" belongs to the line before it; show it as a note.
            flush(&mut rows, &mut removed, &mut added);
            rows.push(SideRow {
                kind: RowKind::Header,
                left: Some(line.clone()),
                right: None,
            });
        } else {
            flush(&mut rows, &mut removed, &mut added);
            let t = line.strip_prefix(' ').unwrap_or(line).to_string();
            rows.push(SideRow {
                kind: RowKind::Same,
                left: Some(t.clone()),
                right: Some(t),
            });
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

impl Review {
    /// Loads a proposed change.
    pub fn set(&mut self, summary: &str, files: Vec<FileChange>, checks: Vec<Check>) {
        self.summary = summary.to_string();
        self.files = files;
        self.checks = checks;
        self.state = PatchState::Proposed;
        self.cursor = Cursor::default();
        self.scroll = 0;
        self.message.clear();
    }

    /// Records what the controller did.
    pub fn set_state(&mut self, state: PatchState) {
        self.state = state;
        self.message.clear();
    }

    /// True when every check passed (and there is at least one).
    pub fn all_checks_pass(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|c| c.passed)
    }

    fn selected(&self) -> Option<&FileChange> {
        self.files.get(self.cursor.selected)
    }

    fn max_scroll(&self) -> usize {
        self.selected().map_or(0, |f| {
            if self.side_by_side {
                side_rows(&f.diff).len().saturating_sub(1)
            } else {
                f.diff.len().saturating_sub(1)
            }
        })
    }

    fn apply(&mut self) -> Action {
        if self.state != PatchState::Proposed {
            self.message = format!("This change is already {}.", self.state.label());
            return Action::None;
        }
        if self.files.is_empty() {
            self.message = "There is nothing to apply.".into();
            return Action::None;
        }
        if !self.all_checks_pass() {
            self.message = "Checks have not all passed, so it cannot be applied.".into();
            return Action::None;
        }
        self.message.clear();
        Action::ApplyPatch
    }

    fn rollback(&mut self) -> Action {
        if self.state != PatchState::Applied {
            self.message = "Only an applied change can be rolled back.".into();
            return Action::None;
        }
        self.message.clear();
        Action::RollbackPatch
    }

    fn reject(&mut self) -> Action {
        if self.state != PatchState::Proposed {
            self.message = "Only a proposed change can be rejected.".into();
            return Action::None;
        }
        self.message.clear();
        Action::RejectPatch
    }
}

impl Review {
    /// Draws old on the left and new on the right, with a divider between.
    fn draw_side(&self, g: &mut Grid, inner: Rect, dy: usize, rows: usize, f: &FileChange) {
        let half = (inner.w.saturating_sub(5)) / 2;
        let lx = inner.x + 2;
        let rx = lx + half + 1;
        for (i, row) in side_rows(&f.diff)
            .iter()
            .skip(self.scroll)
            .take(rows)
            .enumerate()
        {
            let y = dy + 1 + i;
            match row.kind {
                RowKind::Header => {
                    let text = row.left.as_deref().unwrap_or("");
                    g.text_fit(
                        lx,
                        y,
                        inner.w.saturating_sub(4),
                        text,
                        diff_colour(text),
                        theme::BG,
                    );
                }
                RowKind::Same | RowKind::Changed => {
                    let changed = row.kind == RowKind::Changed;
                    if let Some(t) = &row.left {
                        let col = if changed { theme::BAD } else { theme::TEXT };
                        g.text_fit(lx, y, half, t, col, theme::BG);
                    }
                    g.text(rx - 1, y, "|", theme::DIM, theme::BG);
                    if let Some(t) = &row.right {
                        let col = if changed { theme::OK } else { theme::TEXT };
                        g.text_fit(rx, y, half, t, col, theme::BG);
                    }
                }
            }
        }
    }
}

fn diff_colour(line: &str) -> Color {
    if line.starts_with("+++") || line.starts_with("---") {
        theme::DIM
    } else if line.starts_with('+') {
        theme::OK
    } else if line.starts_with('-') {
        theme::BAD
    } else if line.starts_with("@@") {
        theme::ACCENT
    } else {
        theme::TEXT
    }
}

impl Screen for Review {
    fn on_key(&mut self, key: Key) -> Action {
        let len = self.files.len();
        match key {
            Key::Up | Key::Char('k') => {
                self.cursor.step(-1, len);
                self.scroll = 0;
            }
            Key::Down | Key::Char('j') => {
                self.cursor.step(1, len);
                self.scroll = 0;
            }
            Key::PageDown => self.scroll = (self.scroll + 8).min(self.max_scroll()),
            Key::PageUp => self.scroll = self.scroll.saturating_sub(8),
            Key::Char('s') => {
                self.side_by_side = !self.side_by_side;
                self.scroll = 0;
            }
            Key::Char('a') => return self.apply(),
            Key::Char('u') => return self.rollback(),
            Key::Char('x') => return self.reject(),
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        let title = format!("Code review  {}", self.state.label());
        g.boxed(area, Some(&title), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 30 || inner.h < 6 {
            return;
        }
        if self.files.is_empty() {
            g.text(
                inner.x + 2,
                inner.y + 1,
                "No change is waiting for review.",
                theme::DIM,
                theme::BG,
            );
            return;
        }
        g.text_fit(
            inner.x + 2,
            inner.y,
            inner.w - 3,
            &self.summary,
            theme::TEXT,
            theme::BG,
        );
        let list_h = (inner.h / 4).clamp(2, 6).min(self.files.len());
        let top = self.cursor.window(list_h);
        for (i, f) in self.files.iter().enumerate().skip(top).take(list_h) {
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
            g.text(inner.x + 3, y, &format!("+{}", f.added), theme::OK, bg);
            g.text(inner.x + 9, y, &format!("-{}", f.removed), theme::BAD, bg);
            g.text_fit(
                inner.x + 15,
                y,
                inner.w.saturating_sub(17),
                &f.path,
                theme::TEXT,
                bg,
            );
        }
        // Checks sit on one line above the diff.
        let cy = inner.y + 2 + list_h;
        let mut x = inner.x + 2;
        for c in &self.checks {
            if x + 4 >= inner.x + inner.w {
                break;
            }
            let (mark, col) = if c.passed {
                ("ok ", theme::OK)
            } else {
                ("FAIL ", theme::BAD)
            };
            x += g.text(x, cy, mark, col, theme::BG);
            x += g.text(x, cy, &format!("{}  ", c.name), theme::DIM, theme::BG);
        }
        let dy = cy + 1;
        let end = inner.y + inner.h;
        if dy + 2 > end {
            return;
        }
        g.boxed(
            Rect::new(inner.x, dy, inner.w, end - dy),
            self.selected().map(|f| f.path.as_str()),
            theme::DIM,
            theme::BG,
        );
        if let Some(f) = self.selected() {
            let rows = (end - dy).saturating_sub(2);
            if self.side_by_side && inner.w >= SIDE_BY_SIDE_MIN_WIDTH {
                self.draw_side(g, inner, dy, rows, f);
            } else {
                for (i, line) in f.diff.iter().skip(self.scroll).take(rows).enumerate() {
                    g.text_fit(
                        inner.x + 2,
                        dy + 1 + i,
                        inner.w.saturating_sub(4),
                        line,
                        diff_colour(line),
                        theme::BG,
                    );
                }
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
        "Up/Down file  PgUp/PgDn scroll  s side by side  a apply  u undo  x reject".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileChange {
        FileChange {
            path: path.into(),
            added: 2,
            removed: 1,
            diff: [
                "--- a/x",
                "+++ b/x",
                "@@ -1,2 +1,3 @@",
                " keep",
                "-old line",
                "+new line",
                "+another",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }

    fn ok_checks() -> Vec<Check> {
        vec![
            Check {
                name: "tests".into(),
                passed: true,
            },
            Check {
                name: "lint".into(),
                passed: true,
            },
        ]
    }

    fn loaded(checks: Vec<Check>) -> Review {
        let mut r = Review::default();
        r.set(
            "Fix the rounding bug",
            vec![file("src/a.rs"), file("src/b.rs")],
            checks,
        );
        r
    }

    fn drawn(r: &Review, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let a = g.area();
        r.draw(&mut g, a);
        g.to_text()
    }

    #[test]
    fn it_draws_summary_files_checks_and_diff() {
        let t = drawn(&loaded(ok_checks()), 80, 24);
        assert!(t.contains("Code review  proposed"), "{t}");
        assert!(t.contains("Fix the rounding bug"), "{t}");
        assert!(t.contains("src/b.rs"), "{t}");
        assert!(t.contains("ok tests"), "{t}");
        assert!(t.contains("+new line"), "{t}");
        assert!(t.contains("-old line"), "{t}");
    }

    #[test]
    fn apply_needs_passing_checks_and_a_proposed_change() {
        let mut r = loaded(vec![Check {
            name: "tests".into(),
            passed: false,
        }]);
        assert_eq!(r.on_key(Key::Char('a')), Action::None);
        assert!(r.message.contains("Checks have not all passed"));
        let mut none = loaded(vec![]);
        assert_eq!(none.on_key(Key::Char('a')), Action::None);
        let mut r = loaded(ok_checks());
        assert_eq!(r.on_key(Key::Char('a')), Action::ApplyPatch);
        r.set_state(PatchState::Applied);
        assert_eq!(r.on_key(Key::Char('a')), Action::None);
        assert!(r.message.contains("already applied"));
    }

    #[test]
    fn undo_only_after_apply_and_reject_only_before() {
        let mut r = loaded(ok_checks());
        assert_eq!(r.on_key(Key::Char('u')), Action::None);
        assert_eq!(r.on_key(Key::Char('x')), Action::RejectPatch);
        r.set_state(PatchState::Applied);
        assert_eq!(r.on_key(Key::Char('x')), Action::None);
        assert_eq!(r.on_key(Key::Char('u')), Action::RollbackPatch);
        r.set_state(PatchState::RolledBack);
        assert_eq!(r.on_key(Key::Char('u')), Action::None);
    }

    #[test]
    fn changing_file_resets_the_scroll_and_scroll_is_bounded() {
        let mut r = loaded(ok_checks());
        r.on_key(Key::PageDown);
        r.on_key(Key::PageDown);
        assert_eq!(r.scroll, 6);
        r.on_key(Key::PageUp);
        assert_eq!(r.scroll, 0);
        r.on_key(Key::PageDown);
        r.on_key(Key::Down);
        assert_eq!(r.scroll, 0);
        assert_eq!(r.cursor.selected, 1);
    }

    #[test]
    fn an_empty_review_says_so_and_ignores_actions() {
        let mut r = Review::default();
        assert!(drawn(&r, 80, 14).contains("No change is waiting"));
        assert_eq!(r.on_key(Key::Char('a')), Action::None);
        assert_eq!(r.on_key(Key::PageDown), Action::None);
    }

    #[test]
    fn drawing_never_panics_at_any_size() {
        let r = loaded(ok_checks());
        for w in 0..70 {
            for h in 0..26 {
                drawn(&r, w, h);
            }
        }
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn removed_and_added_lines_are_paired_within_a_hunk() {
        let rows = side_rows(&lines(&[
            "--- a/x",
            "+++ b/x",
            "@@ -1,4 +1,5 @@",
            " keep",
            "-old one",
            "-old two",
            "+new one",
            "+new two",
            "+new three",
            " tail",
        ]));
        let shown: Vec<_> = rows
            .iter()
            .map(|r| (r.kind, r.left.as_deref(), r.right.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                (RowKind::Header, Some("--- a/x"), None),
                (RowKind::Header, Some("+++ b/x"), None),
                (RowKind::Header, Some("@@ -1,4 +1,5 @@"), None),
                (RowKind::Same, Some("keep"), Some("keep")),
                (RowKind::Changed, Some("old one"), Some("new one")),
                (RowKind::Changed, Some("old two"), Some("new two")),
                (RowKind::Changed, None, Some("new three")),
                (RowKind::Same, Some("tail"), Some("tail")),
            ]
        );
        // A pure deletion and a pure addition have nothing opposite.
        let del = side_rows(&lines(&["@@ -1 +0,0 @@", "-gone"]));
        assert_eq!(
            (del[1].left.as_deref(), del[1].right.as_deref()),
            (Some("gone"), None)
        );
        let add = side_rows(&lines(&["@@ -0,0 +1 @@", "+fresh"]));
        assert_eq!(
            (add[1].left.as_deref(), add[1].right.as_deref()),
            (None, Some("fresh"))
        );
    }

    #[test]
    fn a_removed_line_that_looks_like_a_header_is_still_a_removed_line() {
        // The text "-- comment" removed from a file shows in the diff as "--- comment".
        let rows = side_rows(&lines(&[
            "--- a/x",
            "+++ b/x",
            "@@ -1 +1 @@",
            "--- comment",
            "+++ added",
        ]));
        assert_eq!(rows[3].kind, RowKind::Changed);
        assert_eq!(rows[3].left.as_deref(), Some("-- comment"));
        assert_eq!(rows[3].right.as_deref(), Some("++ added"));
        // Two hunks do not run into each other.
        let two = side_rows(&lines(&["@@ -1 +1 @@", "-a", "@@ -9 +9 @@", "+b"]));
        assert_eq!(two.len(), 4);
        assert_eq!(
            (two[1].left.as_deref(), two[1].right.as_deref()),
            (Some("a"), None)
        );
        assert_eq!(
            (two[3].left.as_deref(), two[3].right.as_deref()),
            (None, Some("b"))
        );
        assert!(side_rows(&[]).is_empty());
        // The "no newline" note is a note across both sides, not a changed line.
        let note = side_rows(&lines(&[
            "@@ -1 +1 @@",
            "-a",
            "\\ No newline at end of file",
            "+b",
        ]));
        assert_eq!(note.len(), 4, "{note:?}");
        assert_eq!(note[2].kind, RowKind::Header);
        assert_eq!(
            note[2].left.as_deref(),
            Some("\\ No newline at end of file")
        );
    }

    #[test]
    fn s_toggles_side_by_side_and_it_draws_old_left_new_right_on_wide_screens_only() {
        let mut r = loaded(ok_checks());
        assert_eq!(r.on_key(Key::Char('s')), Action::None);
        assert!(r.side_by_side);
        let wide = drawn(&r, 100, 24);
        let row = wide
            .lines()
            .find(|l| l.contains("old line"))
            .expect("a changed row");
        let (l, rt) = row.split_once('|').expect("a divider");
        assert!(l.contains("old line") && !l.contains("new line"), "{row}");
        assert!(rt.contains("new line") && !rt.contains("old line"), "{row}");
        assert!(
            !wide.contains("-old line") && !wide.contains("+new line"),
            "markers are gone: {wide}"
        );
        // On a narrow screen it falls back to the unified view instead of squeezing.
        let narrow = drawn(&r, 60, 24);
        assert!(
            narrow.contains("-old line") && narrow.contains("+new line"),
            "{narrow}"
        );
        // Toggling back, and scrolling is reset and bounded by the rows of the current view.
        r.on_key(Key::PageDown);
        r.on_key(Key::Char('s'));
        assert!(!r.side_by_side);
        assert_eq!(r.scroll, 0);
    }

    #[test]
    fn side_by_side_never_panics_at_any_size() {
        let mut r = loaded(ok_checks());
        r.side_by_side = true;
        for w in 0..130 {
            for h in 0..26 {
                drawn(&r, w, h);
            }
        }
    }
}
