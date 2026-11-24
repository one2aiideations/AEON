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
        self.selected()
            .map_or(0, |f| f.diff.len().saturating_sub(1))
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
        "Up/Down file  PgUp/PgDn scroll  a apply  u undo  x reject".into()
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
}
