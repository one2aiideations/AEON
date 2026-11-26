//! The tamper-evident record of what happened.

use crate::grid::{fit, theme, Color, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// One audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    /// Position in the log, from 1.
    pub seq: u64,
    /// When it happened, already formatted for display.
    pub time: String,
    /// Who did it.
    pub actor: String,
    /// What they did, for example `mail.send`.
    pub action: String,
    /// What it concerned (an id, a file, a model).
    pub target: String,
    /// How it ended: `ok`, `pending approval`, `denied: ...`, `failed: ...`.
    pub outcome: String,
    /// This record's hash (first characters are enough for display).
    pub hash: String,
}

/// Whether the chain of hashes is intact.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Chain {
    /// Not checked yet.
    #[default]
    Unchecked,
    /// Every record links to the one before it.
    Verified,
    /// A record was changed, removed or reordered, starting at this record number.
    Broken(u64),
}

/// Which records to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Everything.
    #[default]
    All,
    /// Only records that were denied, failed or are waiting.
    Problems,
}

/// The audit screen.
#[derive(Debug, Clone, Default)]
pub struct Audit {
    /// All records, oldest first.
    pub rows: Vec<AuditRow>,
    /// The result of checking the hash chain.
    pub chain: Chain,
    /// Which records are shown.
    pub filter: Filter,
    /// The selection (an index into the filtered view).
    pub cursor: Cursor,
    /// Only records whose actor, action, target or outcome contain this text (any case).
    pub search: String,
    /// True while the search text is being typed.
    pub typing: bool,
}

/// True for outcomes that deserve attention.
pub fn is_problem(outcome: &str) -> bool {
    outcome.starts_with("denied") || outcome.starts_with("failed") || outcome.starts_with("pending")
}

fn outcome_colour(outcome: &str) -> Color {
    if outcome.starts_with("denied") || outcome.starts_with("failed") {
        theme::BAD
    } else if outcome.starts_with("pending") {
        theme::WARN
    } else {
        theme::OK
    }
}

impl Audit {
    /// The records the current filter lets through, newest first.
    pub fn view(&self) -> Vec<&AuditRow> {
        let mut v: Vec<&AuditRow> = self
            .rows
            .iter()
            .filter(|r| self.filter == Filter::All || is_problem(&r.outcome))
            .filter(|r| self.matches(r))
            .collect();
        v.reverse();
        v
    }

    fn matches(&self, r: &AuditRow) -> bool {
        let q = self.search.trim().to_lowercase();
        q.is_empty()
            || [&r.actor, &r.action, &r.target, &r.outcome]
                .iter()
                .any(|f| f.to_lowercase().contains(&q))
    }

    /// The selected record.
    pub fn selected(&self) -> Option<&AuditRow> {
        self.view().get(self.cursor.selected).copied()
    }

    /// Replaces the records and the chain result.
    pub fn set(&mut self, rows: Vec<AuditRow>, chain: Chain) {
        self.rows = rows;
        self.chain = chain;
        self.cursor.clamp(self.view().len());
    }
}

impl Screen for Audit {
    fn on_key(&mut self, key: Key) -> Action {
        if self.typing {
            match key {
                Key::Char(c) => self.search.push(c),
                Key::Backspace => {
                    self.search.pop();
                }
                Key::Enter => self.typing = false,
                Key::Esc => {
                    self.search.clear();
                    self.typing = false;
                }
                _ => return Action::None,
            }
            self.cursor = Cursor::default();
            return Action::None;
        }
        let len = self.view().len();
        match key {
            Key::Char('/') => self.typing = true,
            Key::Esc => {
                self.search.clear();
                self.cursor = Cursor::default();
            }
            Key::Up | Key::Char('k') => self.cursor.step(-1, len),
            Key::Down | Key::Char('j') => self.cursor.step(1, len),
            Key::PageUp => self.cursor.step(-10, len),
            Key::PageDown => self.cursor.step(10, len),
            Key::Home => self.cursor.jump(false, len),
            Key::End => self.cursor.jump(true, len),
            Key::Char('f') => {
                self.filter = if self.filter == Filter::All {
                    Filter::Problems
                } else {
                    Filter::All
                };
                self.cursor = Cursor::default();
            }
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        let view = self.view();
        let title = match self.filter {
            Filter::All if !self.search.is_empty() => format!(
                "Audit  {} of {} records matching \"{}\"",
                view.len(),
                self.rows.len(),
                self.search
            ),
            Filter::All => format!("Audit  {} records", self.rows.len()),
            Filter::Problems => format!(
                "Audit  {} of {} records (problems only)",
                view.len(),
                self.rows.len()
            ),
        };
        g.boxed(area, Some(&title), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 40 || inner.h < 6 {
            return;
        }
        let (text, colour) = match &self.chain {
            Chain::Unchecked => ("Chain not checked yet".to_string(), theme::DIM),
            Chain::Verified => (
                format!(
                    "Chain verified: all {} records link to the one before",
                    self.rows.len()
                ),
                theme::OK,
            ),
            Chain::Broken(seq) => (
                format!("CHAIN BROKEN at record {seq}: the log was changed after it was written"),
                theme::BAD,
            ),
        };
        g.text_fit(inner.x + 1, inner.y, inner.w - 2, &text, colour, theme::BG);
        let head = inner.y + 2;
        g.text(
            inner.x + 1,
            head,
            "  seq  time        who         action                outcome",
            theme::DIM,
            theme::BG,
        );
        let list_h = inner.h.saturating_sub(7);
        if view.is_empty() {
            g.text(
                inner.x + 2,
                head + 2,
                if self.rows.is_empty() {
                    "Nothing has happened yet."
                } else if !self.search.is_empty() {
                    "No record matches."
                } else {
                    "No problems recorded."
                },
                theme::DIM,
                theme::BG,
            );
        }
        let top = self.cursor.window(list_h);
        for (i, r) in view.iter().enumerate().skip(top).take(list_h) {
            let y = head + 1 + (i - top);
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
            g.text(inner.x + 2, y, &format!("{:>4}", r.seq), theme::DIM, bg);
            g.text_fit(inner.x + 8, y, 10, &r.time, theme::DIM, bg);
            g.text_fit(inner.x + 20, y, 10, &r.actor, theme::TEXT, bg);
            g.text_fit(inner.x + 32, y, 20, &r.action, theme::TEXT, bg);
            g.text_fit(
                inner.x + 54,
                y,
                inner.w.saturating_sub(55),
                &r.outcome,
                outcome_colour(&r.outcome),
                bg,
            );
        }
        if let Some(r) = view.get(self.cursor.selected) {
            let y = inner.y + inner.h - 4;
            g.boxed(
                Rect::new(inner.x, y, inner.w, 4),
                None,
                theme::DIM,
                theme::BG,
            );
            // A hash is shown as its first 16 characters, not shortened with dots, so it can be
            // compared by eye with another listing.
            let hash: String = r.hash.chars().take(16).collect();
            g.text_fit(
                inner.x + 2,
                y + 1,
                inner.w - 4,
                &format!("target {}   hash {hash}", fit(&r.target, 24)),
                theme::DIM,
                theme::BG,
            );
            // The list column is narrow, so the full outcome is spelled out here.
            g.text_fit(
                inner.x + 2,
                y + 2,
                inner.w - 4,
                &format!("{}: {}", r.action, r.outcome),
                outcome_colour(&r.outcome),
                theme::BG,
            );
        }
    }

    fn hints(&self) -> String {
        if self.typing {
            return format!("Search: {}_   Enter keep  Esc clear", self.search);
        }
        match self.filter {
            Filter::All => "Up/Down PgUp/PgDn move  / search  f show problems only".into(),
            Filter::Problems => "Up/Down PgUp/PgDn move  / search  f show everything".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: u64, actor: &str, action: &str, outcome: &str) -> AuditRow {
        AuditRow {
            seq,
            time: format!("12:0{seq}:00"),
            actor: actor.into(),
            action: action.into(),
            target: "-".into(),
            outcome: outcome.into(),
            hash: format!("{seq:016x}abcdef"),
        }
    }

    fn log() -> Audit {
        let mut a = Audit::default();
        a.set(
            vec![
                row(1, "owner", "session.open", "ok"),
                row(2, "owner", "chat", "ok via llama"),
                row(
                    3,
                    "owner",
                    "shell",
                    "denied: role owner lacks permission shell",
                ),
                row(4, "alice", "mail.send", "pending approval"),
                row(5, "bob", "files.delete", "failed: no such file"),
                row(6, "owner", "security.score", "ok"),
            ],
            Chain::Verified,
        );
        a
    }

    fn drawn(a: &Audit, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let area = g.area();
        a.draw(&mut g, area);
        g.to_text()
    }

    #[test]
    fn search_matches_actor_action_and_outcome_in_any_case_and_clears_with_esc() {
        let mut a = log();
        a.on_key(Key::Char('/'));
        assert!(a.typing);
        for c in "ALICE".chars() {
            a.on_key(Key::Char(c));
        }
        assert_eq!(a.view().iter().map(|r| r.seq).collect::<Vec<_>>(), vec![4]);
        a.on_key(Key::Enter);
        assert!(!a.typing);
        assert!(drawn(&a, 100, 20).contains("1 of 6 records matching \"ALICE\""));
        a.on_key(Key::Esc);
        assert_eq!(a.view().len(), 6);

        a.on_key(Key::Char('/'));
        for c in "denied".chars() {
            a.on_key(Key::Char(c));
        }
        assert_eq!(a.view().iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3]);
        a.on_key(Key::Backspace);
        a.on_key(Key::Backspace);
        a.on_key(Key::Backspace);
        a.on_key(Key::Backspace);
        a.on_key(Key::Backspace);
        a.on_key(Key::Backspace);
        assert_eq!(a.view().len(), 6);
        for c in "zzz".chars() {
            a.on_key(Key::Char(c));
        }
        assert!(a.view().is_empty());
        assert!(drawn(&a, 100, 20).contains("No record matches."));
        assert!(a.hints().contains("Search: zzz_"));
        a.on_key(Key::Esc);
        assert!(!a.typing && a.search.is_empty());
    }

    #[test]
    fn search_and_the_problems_filter_combine() {
        let mut a = log();
        a.on_key(Key::Char('f'));
        a.on_key(Key::Char('/'));
        for c in "owner".chars() {
            a.on_key(Key::Char(c));
        }
        assert_eq!(a.view().iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn newest_records_come_first() {
        let a = log();
        let seqs: Vec<u64> = a.view().iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![6, 5, 4, 3, 2, 1]);
        assert_eq!(a.selected().map(|r| r.seq), Some(6));
    }

    #[test]
    fn the_problems_filter_keeps_denied_failed_and_pending() {
        let mut a = log();
        a.on_key(Key::Char('f'));
        assert_eq!(a.filter, Filter::Problems);
        let seqs: Vec<u64> = a.view().iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![5, 4, 3]);
        assert!(drawn(&a, 100, 20).contains("3 of 6 records (problems only)"));
        a.on_key(Key::Char('f'));
        assert_eq!(a.view().len(), 6);
        assert_eq!(
            a.cursor,
            Cursor::default(),
            "the selection resets when the filter changes"
        );
    }

    #[test]
    fn a_verified_chain_is_reported_in_green_words() {
        let t = drawn(&log(), 100, 20);
        assert!(
            t.contains("Chain verified: all 6 records link to the one before"),
            "{t}"
        );
    }

    #[test]
    fn a_broken_chain_is_impossible_to_miss() {
        let mut a = log();
        a.chain = Chain::Broken(3);
        let t = drawn(&a, 100, 20);
        assert!(t.contains("CHAIN BROKEN at record 3"), "{t}");
        let mut g = Grid::new(100, 20);
        let area = g.area();
        a.draw(&mut g, area);
        let cell = g.get(3, 1).unwrap();
        assert_eq!(
            cell.fg,
            theme::BAD,
            "the warning is drawn in the warning colour"
        );
        a.chain = Chain::Unchecked;
        assert!(drawn(&a, 100, 20).contains("Chain not checked yet"));
    }

    #[test]
    fn outcomes_are_coloured_by_meaning() {
        let a = log();
        let mut g = Grid::new(100, 20);
        let area = g.area();
        a.draw(&mut g, area);
        let colour_of = |needle: &str| {
            let (y, line) = g
                .to_text()
                .lines()
                .enumerate()
                .map(|(i, l)| (i, l.to_string()))
                .find(|(_, l)| l.contains(needle))
                .unwrap();
            let x = line
                .chars()
                .position(|c| c == needle.chars().next().unwrap())
                .unwrap();
            let _ = x;
            let col = line
                .find(needle)
                .map(|b| line[..b].chars().count())
                .unwrap();
            g.get(col, y).unwrap().fg
        };
        assert_eq!(colour_of("denied: role"), theme::BAD);
        assert_eq!(colour_of("pending approval"), theme::WARN);
        assert_eq!(colour_of("failed: no such"), theme::BAD);
        assert_eq!(colour_of("ok via llama"), theme::OK);
    }

    #[test]
    fn the_selected_record_shows_its_target_and_hash() {
        let mut a = log();
        a.on_key(Key::Down);
        let t = drawn(&a, 100, 20);
        assert!(t.contains("target -   hash 0000000000000005"), "{t}");
    }

    #[test]
    fn a_long_outcome_is_spelled_out_for_the_selected_record() {
        let mut a = log();
        for _ in 0..3 {
            a.on_key(Key::Down);
        }
        // Record 3 is the denied one; the list column truncates it but the detail box does not.
        let t = drawn(&a, 100, 24);
        assert!(
            t.contains("shell: denied: role owner lacks permission shell"),
            "{t}"
        );
    }

    #[test]
    fn navigation_is_clamped_and_empty_logs_are_handled() {
        let mut a = log();
        for _ in 0..50 {
            a.on_key(Key::Down);
        }
        assert_eq!(a.cursor.selected, 5);
        a.on_key(Key::Home);
        assert_eq!(a.cursor.selected, 0);
        a.on_key(Key::PageDown);
        assert_eq!(a.cursor.selected, 5);
        let mut empty = Audit::default();
        assert!(drawn(&empty, 100, 20).contains("Nothing has happened yet."));
        empty.on_key(Key::Down);
        empty.on_key(Key::Char('f'));
        a.on_key(Key::Char('f'));
        a.set(vec![row(1, "o", "x", "ok")], Chain::Verified);
        assert!(drawn(&a, 100, 20).contains("No problems recorded."));
    }

    #[test]
    fn a_long_log_scrolls_and_tiny_areas_do_not_panic() {
        let mut a = Audit::default();
        a.set(
            (1..=200).map(|i| row(i, "owner", "chat", "ok")).collect(),
            Chain::Verified,
        );
        for _ in 0..60 {
            a.on_key(Key::Down);
        }
        let t = drawn(&a, 100, 20);
        assert!(t.contains(" 140 "), "{t}");
        for (w, h) in [(0, 0), (10, 10), (39, 20), (40, 6), (120, 3)] {
            let _ = drawn(&a, w, h);
        }
    }

    #[test]
    fn problem_detection() {
        assert!(
            is_problem("denied: x") && is_problem("failed: y") && is_problem("pending approval")
        );
        assert!(!is_problem("ok") && !is_problem("ok via llama"));
    }
}
