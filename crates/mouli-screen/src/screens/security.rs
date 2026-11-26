//! Risks, anomalies and cases.

use crate::grid::{theme, wrap, Color, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Screen};

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing.
    Low,
    /// Should be looked at.
    Medium,
    /// Needs action soon.
    High,
    /// Needs action now.
    Critical,
}

impl Severity {
    /// The word shown on screen.
    pub const fn label(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "CRITICAL",
        }
    }

    /// The colour used for it.
    pub const fn colour(self) -> Color {
        match self {
            Severity::Low => theme::DIM,
            Severity::Medium => theme::WARN,
            Severity::High | Severity::Critical => theme::BAD,
        }
    }
}

/// One finding: a rule that fired, an anomaly, or an open case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// A short id, for example `case-7`.
    pub id: String,
    /// How serious it is.
    pub severity: Severity,
    /// One line: what it is.
    pub title: String,
    /// What was seen and what to do.
    pub detail: String,
    /// Whether someone has already looked at it.
    pub acknowledged: bool,
}

/// The security screen.
#[derive(Debug, Clone, Default)]
pub struct SecurityScreen {
    /// Overall risk from 0 (calm) to 100.
    pub score: u8,
    /// Findings, most serious first (see [`SecurityScreen::set`]).
    pub findings: Vec<Finding>,
    /// The selection.
    pub cursor: Cursor,
    /// A message under the list (for example why something cannot be acknowledged).
    pub message: String,
}

impl SecurityScreen {
    /// Replaces the findings (sorted most serious first, then by id) and the score.
    pub fn set(&mut self, score: u8, mut findings: Vec<Finding>) {
        findings.sort_by(|a, b| b.severity.cmp(&a.severity).then_with(|| a.id.cmp(&b.id)));
        self.score = score.min(100);
        self.findings = findings;
        self.cursor.clamp(self.findings.len());
    }

    /// The selected finding.
    pub fn selected(&self) -> Option<&Finding> {
        self.findings.get(self.cursor.selected)
    }

    fn level(&self) -> (&'static str, Color) {
        match self.score {
            0..=24 => ("calm", theme::OK),
            25..=59 => ("watch", theme::WARN),
            _ => ("at risk", theme::BAD),
        }
    }
}

impl Screen for SecurityScreen {
    fn on_key(&mut self, key: Key) -> Action {
        let len = self.findings.len();
        match key {
            Key::Up | Key::Char('k') => self.cursor.step(-1, len),
            Key::Down | Key::Char('j') => self.cursor.step(1, len),
            Key::Home => self.cursor.jump(false, len),
            Key::End => self.cursor.jump(true, len),
            Key::Char('s') => return Action::RefreshSecurity,
            Key::Char('a') => {
                let Some((id, seen)) = self.selected().map(|f| (f.id.clone(), f.acknowledged))
                else {
                    return Action::None;
                };
                if seen {
                    self.message = "Already acknowledged.".into();
                    return Action::None;
                }
                self.message.clear();
                return Action::AckFinding(id);
            }
            _ => {}
        }
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.boxed(area, Some("Security"), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 30 || inner.h < 6 {
            return;
        }
        let (word, colour) = self.level();
        g.text(inner.x + 2, inner.y, "risk", theme::DIM, theme::BG);
        let bar_w = inner.w.saturating_sub(26).clamp(10, 40);
        g.bar(
            (inner.x + 8, inner.y),
            bar_w,
            u64::from(self.score),
            100,
            colour,
            theme::BG,
        );
        g.text(
            inner.x + 9 + bar_w,
            inner.y,
            &format!("{}/100  {}", self.score, word),
            colour,
            theme::BG,
        );
        if self.findings.is_empty() {
            g.text(
                inner.x + 2,
                inner.y + 2,
                "No findings. Nothing unusual has been seen.",
                theme::DIM,
                theme::BG,
            );
            return;
        }
        let list_h = ((inner.h - 2) / 2).clamp(3, 10).min(self.findings.len());
        let top = self.cursor.window(list_h);
        for (i, f) in self.findings.iter().enumerate().skip(top).take(list_h) {
            let y = inner.y + 2 + (i - top);
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
                9,
                f.severity.label(),
                f.severity.colour(),
                bg,
            );
            g.text_fit(inner.x + 13, y, 10, &f.id, theme::DIM, bg);
            let mark = if f.acknowledged { "seen " } else { "" };
            g.text_fit(
                inner.x + 24,
                y,
                inner.w.saturating_sub(26),
                &format!("{mark}{}", f.title),
                theme::TEXT,
                bg,
            );
        }
        if !self.message.is_empty() {
            g.text_fit(
                inner.x + 2,
                inner.y + 1,
                inner.w - 4,
                &self.message,
                theme::WARN,
                theme::BG,
            );
        }
        if let Some(f) = self.selected() {
            let dy = inner.y + 3 + list_h;
            if dy + 3 <= inner.y + inner.h {
                g.boxed(
                    Rect::new(inner.x, dy, inner.w, inner.y + inner.h - dy),
                    Some("What was seen"),
                    theme::DIM,
                    theme::BG,
                );
                let limit = inner.y + inner.h - 1;
                for (i, part) in wrap(&f.detail, inner.w.saturating_sub(4))
                    .into_iter()
                    .enumerate()
                {
                    if dy + 1 + i >= limit {
                        break;
                    }
                    g.text(inner.x + 2, dy + 1 + i, &part, theme::TEXT, theme::BG);
                }
            }
        }
    }

    fn hints(&self) -> String {
        "Up/Down choose  a mark as seen  s scan again".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, sev: Severity) -> Finding {
        Finding {
            id: id.into(),
            severity: sev,
            title: format!("title of {id}"),
            detail: "Five failed sign-ins from one address within a minute, then a success.".into(),
            acknowledged: false,
        }
    }

    fn screen() -> SecurityScreen {
        let mut s = SecurityScreen::default();
        s.set(
            72,
            vec![
                finding("case-2", Severity::Low),
                finding("case-9", Severity::Critical),
                finding("case-4", Severity::High),
            ],
        );
        s
    }

    fn drawn(s: &SecurityScreen, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        let a = g.area();
        s.draw(&mut g, a);
        g.to_text()
    }

    #[test]
    fn findings_are_sorted_most_serious_first() {
        let s = screen();
        let ids: Vec<_> = s.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["case-9", "case-4", "case-2"]);
    }

    #[test]
    fn the_score_is_capped_and_named() {
        let mut s = SecurityScreen::default();
        s.set(250, vec![]);
        assert_eq!(s.score, 100);
        assert_eq!(s.level().0, "at risk");
        s.set(10, vec![]);
        assert_eq!(s.level().0, "calm");
        s.set(40, vec![]);
        assert_eq!(s.level().0, "watch");
    }

    #[test]
    fn it_draws_the_score_the_list_and_the_selected_detail() {
        let t = drawn(&screen(), 80, 22);
        assert!(t.contains("72/100  at risk"), "{t}");
        assert!(t.contains("CRITICAL"), "{t}");
        assert!(t.contains("title of case-9"), "{t}");
        assert!(t.contains("Five failed sign-ins"), "{t}");
    }

    #[test]
    fn an_empty_screen_says_so() {
        assert!(drawn(&SecurityScreen::default(), 80, 14).contains("No findings"));
    }

    #[test]
    fn keys_move_the_selection_and_s_asks_for_a_scan() {
        let mut s = screen();
        assert_eq!(s.on_key(Key::Down), Action::None);
        assert_eq!(s.selected().map(|f| f.id.as_str()), Some("case-4"));
        s.on_key(Key::End);
        assert_eq!(s.selected().map(|f| f.id.as_str()), Some("case-2"));
        assert_eq!(s.on_key(Key::Char('s')), Action::RefreshSecurity);
        assert_eq!(s.on_key(Key::Char('x')), Action::None);
    }

    #[test]
    fn drawing_never_panics_at_any_size() {
        let s = screen();
        for w in 0..60 {
            for h in 0..24 {
                drawn(&s, w, h);
            }
        }
    }
}
