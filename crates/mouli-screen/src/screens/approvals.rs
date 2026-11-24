//! Requests held for a decision.

use crate::grid::{fit, theme, wrap, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// One held request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRow {
    /// The approval id.
    pub id: String,
    /// The capability that wants to run, for example `mail.send`.
    pub capability: String,
    /// Who asked.
    pub requester: String,
    /// What it would do, as `key=value` text.
    pub detail: String,
    /// Whether the person using this screen may decide it (the requester may not approve their
    /// own request).
    pub can_decide: bool,
    /// Why not, when `can_decide` is false.
    pub why_not: String,
}

/// The approvals screen.
#[derive(Debug, Clone, Default)]
pub struct Approvals {
    /// Requests waiting for a decision.
    pub rows: Vec<ApprovalRow>,
    /// The selection.
    pub cursor: Cursor,
    /// A message for the person (for example why a decision was refused).
    pub message: String,
}

impl Approvals {
    /// The selected request.
    pub fn selected(&self) -> Option<&ApprovalRow> {
        self.rows.get(self.cursor.selected)
    }

    /// Replaces the list, keeping the selection in range.
    pub fn set_rows(&mut self, rows: Vec<ApprovalRow>) {
        self.rows = rows;
        self.cursor.clamp(self.rows.len());
    }

    fn decide(&mut self, approve: bool) -> Action {
        let Some(row) = self.rows.get(self.cursor.selected).cloned() else {
            return Action::None;
        };
        if !row.can_decide {
            self.message = if row.why_not.is_empty() {
                "You cannot decide this request.".to_string()
            } else {
                row.why_not
            };
            return Action::None;
        }
        self.message.clear();
        Action::Decide {
            id: row.id,
            approve,
        }
    }
}

impl Screen for Approvals {
    fn on_key(&mut self, key: Key) -> Action {
        let len = self.rows.len();
        match key {
            Key::Up | Key::Char('k') => self.cursor.step(-1, len),
            Key::Down | Key::Char('j') => self.cursor.step(1, len),
            Key::Home => self.cursor.jump(false, len),
            Key::End => self.cursor.jump(true, len),
            Key::Char('a') => return self.decide(true),
            Key::Char('r') | Key::Char('d') => return self.decide(false),
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        let title = if self.rows.is_empty() {
            "Approvals".to_string()
        } else {
            format!("Approvals  {} waiting", self.rows.len())
        };
        g.boxed(area, Some(&title), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 30 || inner.h < 6 {
            return;
        }
        if self.rows.is_empty() {
            g.text(
                inner.x + 2,
                inner.y + 1,
                "Nothing is waiting for approval.",
                theme::DIM,
                theme::BG,
            );
            g.text(
                inner.x + 2,
                inner.y + 3,
                "Risky actions (sending, deleting, paying) stop here until a second",
                theme::DIM,
                theme::BG,
            );
            g.text(
                inner.x + 2,
                inner.y + 4,
                "person approves them. Nobody can approve their own request.",
                theme::DIM,
                theme::BG,
            );
            return;
        }
        // List on top, details below.
        let list_h = (inner.h / 2).clamp(3, 10).min(self.rows.len().max(1));
        let top = self.cursor.window(list_h);
        for (i, row) in self.rows.iter().enumerate().skip(top).take(list_h) {
            let y = inner.y + (i - top);
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
            g.text_fit(inner.x + 3, y, 12, &row.id, theme::DIM, bg);
            g.text_fit(inner.x + 16, y, 22, &row.capability, theme::TEXT, bg);
            g.text_fit(
                inner.x + 40,
                y,
                inner.w.saturating_sub(42),
                &format!("asked by {}", row.requester),
                theme::DIM,
                bg,
            );
        }
        if let Some(row) = self.selected() {
            let dy = inner.y + list_h + 1;
            g.boxed(
                Rect::new(inner.x, dy, inner.w, inner.h - list_h - 1),
                Some("This request"),
                theme::DIM,
                theme::BG,
            );
            let text_w = inner.w.saturating_sub(4);
            let mut y = dy + 1;
            let mut line = |g: &mut Grid, label: &str, value: &str, colour| {
                if y >= inner.y + inner.h - 1 {
                    return;
                }
                g.text(inner.x + 2, y, label, theme::DIM, theme::BG);
                for (i, part) in wrap(value, text_w.saturating_sub(12))
                    .into_iter()
                    .enumerate()
                {
                    if y + i >= inner.y + inner.h - 1 {
                        break;
                    }
                    g.text(inner.x + 14, y + i, &part, colour, theme::BG);
                }
                y += wrap(value, text_w.saturating_sub(12)).len().max(1);
            };
            line(g, "action", &row.capability, theme::TEXT);
            line(g, "asked by", &row.requester, theme::TEXT);
            line(
                g,
                "details",
                if row.detail.is_empty() {
                    "(none)"
                } else {
                    &row.detail
                },
                theme::WARN,
            );
            if row.can_decide {
                line(g, "you can", "approve (a) or refuse (r)", theme::OK);
            } else {
                line(g, "you can", &fit(&row.why_not, text_w), theme::BAD);
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
        "Up/Down choose  a approve  r refuse".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, cap: &str, who: &str, can: bool) -> ApprovalRow {
        ApprovalRow {
            id: id.into(),
            capability: cap.into(),
            requester: who.into(),
            detail: "to=bob@example.com subject=Invoice".into(),
            can_decide: can,
            why_not: if can {
                String::new()
            } else {
                "You asked for this yourself; someone else must decide.".into()
            },
        }
    }

    fn screen() -> Approvals {
        let mut a = Approvals::default();
        a.set_rows(vec![
            row("apr-1", "mail.send", "alice", true),
            row("apr-2", "files.delete", "me", false),
            row("apr-3", "pay.send", "carol", true),
        ]);
        a
    }

    fn drawn(a: &Approvals, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let area = g.area();
        a.draw(&mut g, area);
        g.to_text()
    }

    #[test]
    fn approving_and_refusing_report_the_decision() {
        let mut a = screen();
        assert_eq!(
            a.on_key(Key::Char('a')),
            Action::Decide {
                id: "apr-1".into(),
                approve: true
            }
        );
        assert_eq!(
            a.on_key(Key::Char('r')),
            Action::Decide {
                id: "apr-1".into(),
                approve: false
            }
        );
        a.on_key(Key::Down);
        a.on_key(Key::Down);
        assert_eq!(
            a.on_key(Key::Char('d')),
            Action::Decide {
                id: "apr-3".into(),
                approve: false
            }
        );
    }

    #[test]
    fn nobody_decides_their_own_request() {
        let mut a = screen();
        a.on_key(Key::Down);
        assert_eq!(a.on_key(Key::Char('a')), Action::None);
        assert!(
            a.message.contains("someone else must decide"),
            "{}",
            a.message
        );
        assert_eq!(a.on_key(Key::Char('r')), Action::None);
        // The message goes away once a decision is possible again.
        a.on_key(Key::Up);
        a.on_key(Key::Char('a'));
        assert!(a.message.is_empty());
    }

    #[test]
    fn an_empty_list_explains_itself() {
        let mut a = Approvals::default();
        assert!(drawn(&a, 90, 14).contains("Nothing is waiting for approval."));
        assert_eq!(a.on_key(Key::Char('a')), Action::None);
        assert_eq!(a.on_key(Key::Down), Action::None);
    }

    #[test]
    fn the_list_and_details_show_what_would_happen() {
        let t = drawn(&screen(), 90, 18);
        assert!(t.contains("Approvals  3 waiting"));
        assert!(
            t.contains("mail.send") && t.contains("asked by alice"),
            "{t}"
        );
        assert!(t.contains("to=bob@example.com subject=Invoice"), "{t}");
        assert!(t.contains("approve (a) or refuse (r)"));
        let mut a = screen();
        a.on_key(Key::Down);
        let t = drawn(&a, 90, 18);
        assert!(t.contains("You asked for this yourself"), "{t}");
        assert!(!t.contains("approve (a) or refuse (r)"));
    }

    #[test]
    fn the_selection_survives_the_list_shrinking() {
        let mut a = screen();
        a.on_key(Key::End);
        assert_eq!(a.cursor.selected, 2);
        a.set_rows(vec![row("apr-9", "mail.send", "bob", true)]);
        assert_eq!(a.cursor.selected, 0);
        a.set_rows(Vec::new());
        assert_eq!(a.on_key(Key::Char('a')), Action::None);
        assert!(drawn(&a, 80, 12).contains("Nothing is waiting"));
    }

    #[test]
    fn long_details_wrap_and_tiny_areas_do_not_panic() {
        let mut a = screen();
        a.rows[0].detail = "body=".to_string() + &"very long text ".repeat(30);
        for (w, h) in [(0, 0), (5, 5), (29, 20), (30, 6), (60, 8), (200, 60)] {
            let _ = drawn(&a, w, h);
        }
        let t = drawn(&a, 90, 22);
        assert!(t.contains("very long text"), "{t}");
    }
}
