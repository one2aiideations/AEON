//! The tabbed shell: a header with the screens, the active screen, and a footer with key hints.

use crate::grid::{fit, theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Screen, Tab};
use crate::screens::{
    Approvals, Audit, BrowserScreen, Chat, EditorScreen, ModelPicker, PlayerScreen, PreviewScreen,
    Review, SecurityScreen, SheetScreen, StorageScreen, StudioScreen,
};

/// The whole interface.
#[derive(Debug, Clone)]
pub struct Shell {
    /// The conversation.
    pub chat: Chat,
    /// The model list.
    pub models: ModelPicker,
    /// Requests held for a decision.
    pub approvals: Approvals,
    /// The audit log.
    pub audit: Audit,
    /// Risks, anomalies and cases.
    pub security: SecurityScreen,
    /// The code change under review.
    pub review: Review,
    /// The spreadsheet.
    pub sheet: SheetScreen,
    /// The document editor.
    pub studio: StudioScreen,
    /// The code editor.
    pub editor: EditorScreen,
    /// The video and YouTube player.
    pub player: PlayerScreen,
    /// What is on the data disk.
    pub storage: StorageScreen,
    /// The rendered preview of the open file.
    pub preview: PreviewScreen,
    /// The web browser.
    pub browser: BrowserScreen,
    /// Which screen is showing.
    pub active: Tab,
    /// Text on the right of the header (for example the clock or the network name).
    pub status: String,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            chat: Chat::default(),
            models: ModelPicker::default(),
            approvals: Approvals::default(),
            audit: Audit::default(),
            security: SecurityScreen::default(),
            review: Review::default(),
            sheet: SheetScreen::default(),
            studio: StudioScreen::default(),
            editor: EditorScreen::default(),
            player: PlayerScreen::default(),
            storage: StorageScreen::default(),
            preview: PreviewScreen::default(),
            browser: BrowserScreen::default(),
            active: Tab::Models,
            status: String::new(),
        }
    }
}

impl Shell {
    /// A shell showing the model list (the first thing after boot).
    pub fn new() -> Self {
        Self::default()
    }

    /// The screen for `tab`.
    pub fn screen(&self, tab: Tab) -> &dyn Screen {
        match tab {
            Tab::Chat => &self.chat,
            Tab::Models => &self.models,
            Tab::Approvals => &self.approvals,
            Tab::Audit => &self.audit,
            Tab::Security => &self.security,
            Tab::Review => &self.review,
            Tab::Sheet => &self.sheet,
            Tab::Studio => &self.studio,
            Tab::Editor => &self.editor,
            Tab::Player => &self.player,
            Tab::Storage => &self.storage,
            Tab::Preview => &self.preview,
            Tab::Browser => &self.browser,
        }
    }

    /// The screen for `tab`, to send it keys.
    pub fn screen_mut(&mut self, tab: Tab) -> &mut dyn Screen {
        match tab {
            Tab::Chat => &mut self.chat,
            Tab::Models => &mut self.models,
            Tab::Approvals => &mut self.approvals,
            Tab::Audit => &mut self.audit,
            Tab::Security => &mut self.security,
            Tab::Review => &mut self.review,
            Tab::Sheet => &mut self.sheet,
            Tab::Studio => &mut self.studio,
            Tab::Editor => &mut self.editor,
            Tab::Player => &mut self.player,
            Tab::Storage => &mut self.storage,
            Tab::Preview => &mut self.preview,
            Tab::Browser => &mut self.browser,
        }
    }

    fn current(&mut self) -> &mut dyn Screen {
        self.screen_mut(self.active)
    }

    fn current_ref(&self) -> &dyn Screen {
        self.screen(self.active)
    }

    /// Handles a key: global keys first (F1 to F6 switch screens), then the active screen.
    pub fn on_key(&mut self, key: Key) -> Action {
        for tab in Tab::ALL {
            if key == tab.key() {
                self.active = tab;
                return Action::Show(tab);
            }
        }
        let action = self.current().on_key(key);
        if let Action::Show(tab) = action {
            self.active = tab;
        }
        action
    }

    /// Draws the whole interface into `grid`.
    pub fn draw(&self, grid: &mut Grid) {
        let area = grid.area();
        grid.fill(area, theme::BG);
        if area.h < 4 || area.w < 20 {
            grid.text(0, 0, "MOULI", theme::ACCENT, theme::BG);
            return;
        }
        // Header: brand, tabs, status.
        grid.fill(Rect::new(0, 0, area.w, 1), theme::PANEL);
        grid.text(1, 0, "MOULI", theme::ACCENT, theme::PANEL);
        let mut x = 8;
        // When every full name does not fit, only the active tab keeps its name; the others show
        // just their function key, so every screen stays reachable.
        let full_width: usize = 8 + Tab::ALL
            .iter()
            .map(|t| format!(" {} {} ", t.key_name(), t.label()).chars().count())
            .sum::<usize>();
        let compact = full_width + 1 > area.w;
        for tab in Tab::ALL {
            let on = tab == self.active;
            let label = if compact && !on {
                format!(" {} ", tab.key_name())
            } else {
                format!(" {} {} ", tab.key_name(), tab.label())
            };
            let (fg, bg) = if on {
                (theme::BG, theme::ACCENT)
            } else {
                (theme::DIM, theme::PANEL)
            };
            x += grid.text(x, 0, &label, fg, bg);
        }
        // The status goes on the right, but only when there is room for something readable.
        let room = area.w.saturating_sub(x + 2);
        if !self.status.is_empty() && room >= 8 {
            let s = fit(&self.status, room);
            let sx = area.w.saturating_sub(s.chars().count() + 1);
            grid.text(sx, 0, &s, theme::DIM, theme::PANEL);
        }
        // Body.
        let body = Rect::new(0, 1, area.w, area.h - 2);
        self.current_ref().draw(grid, body);
        // Footer.
        grid.fill(Rect::new(0, area.h - 1, area.w, 1), theme::PANEL);
        let hints = self.current_ref().hints();
        grid.text_fit(1, area.h - 1, area.w - 2, &hints, theme::DIM, theme::PANEL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(s: &Shell, w: usize, h: usize) -> String {
        let mut g = Grid::new(w, h);
        s.draw(&mut g);
        g.to_text()
    }

    #[test]
    fn function_keys_switch_screens() {
        let mut s = Shell::new();
        assert_eq!(s.active, Tab::Models);
        assert_eq!(s.on_key(Key::F(1)), Action::Show(Tab::Chat));
        assert_eq!(s.active, Tab::Chat);
        assert_eq!(s.on_key(Key::F(2)), Action::Show(Tab::Models));
        assert_eq!(s.active, Tab::Models);
    }

    #[test]
    fn keys_go_to_the_active_screen() {
        let mut s = Shell::new();
        s.on_key(Key::F(1));
        s.on_key(Key::Char('h'));
        s.on_key(Key::Char('i'));
        assert_eq!(s.chat.input(), "hi");
        assert_eq!(s.on_key(Key::Enter), Action::Send("hi".into()));
        s.on_key(Key::F(2));
        assert_eq!(s.on_key(Key::Char('q')), Action::Quit);
    }

    #[test]
    fn the_header_lists_every_screen_and_the_footer_shows_hints() {
        let mut s = Shell::new();
        s.status = "wlan0 connected".into();
        let t = drawn(&s, 200, 20);
        let header = t.lines().next().unwrap();
        for tab in Tab::ALL {
            assert!(header.contains(tab.label()), "{header}");
        }
        assert!(
            header.contains("MOULI") && header.contains("wlan0 connected"),
            "{header}"
        );
        let footer = t.lines().nth(19).unwrap();
        assert!(footer.contains("Enter load"), "{footer}");
        s.on_key(Key::F(1));
        assert!(drawn(&s, 200, 20)
            .lines()
            .nth(19)
            .unwrap()
            .contains("Enter send"));
    }

    #[test]
    fn every_tab_stays_reachable_on_an_80_column_screen() {
        let mut sh = Shell::new();
        let t = drawn(&sh, 80, 20);
        let header = t.lines().next().unwrap();
        for n in 1..=8 {
            assert!(header.contains(&format!("F{n}")), "F{n} missing: {header}");
        }
        assert!(
            header.contains("F2 Models"),
            "the active tab keeps its name: {header}"
        );
        assert!(header.chars().count() <= 80);
        sh.on_key(Key::F(8));
        let header = drawn(&sh, 80, 20).lines().next().unwrap().to_string();
        assert!(
            header.contains("F8 Studio") && !header.contains("F2 Models"),
            "{header}"
        );
    }

    #[test]
    fn the_status_is_shown_only_when_it_fits() {
        let mut s = Shell::new();
        s.status = "wlan0 connected".into();
        let narrow = drawn(&s, 50, 20);
        assert!(
            !narrow.lines().next().unwrap().contains("wlan0"),
            "no room at 50 columns"
        );
        assert!(
            !narrow.lines().next().unwrap().contains(".."),
            "no stray dots"
        );
        assert!(drawn(&s, 200, 20)
            .lines()
            .next()
            .unwrap()
            .contains("wlan0 connected"));
    }

    #[test]
    fn a_tiny_screen_still_draws_something_sensible() {
        let s = Shell::new();
        for (w, h) in [(0, 0), (5, 2), (19, 30), (30, 3), (30, 4)] {
            let _ = drawn(&s, w, h);
        }
        assert_eq!(drawn(&s, 10, 2).lines().next().unwrap(), "MOULI");
    }

    #[test]
    fn approvals_and_audit_have_their_own_screens() {
        use crate::screens::{ApprovalRow, AuditRow, Chain};
        let mut s = Shell::new();
        s.approvals.set_rows(vec![ApprovalRow {
            id: "apr-1".into(),
            capability: "mail.send".into(),
            requester: "alice".into(),
            detail: "to=bob".into(),
            can_decide: true,
            why_not: String::new(),
        }]);
        s.audit.set(
            vec![AuditRow {
                seq: 1,
                time: "12:00:00".into(),
                actor: "owner".into(),
                action: "chat".into(),
                target: "-".into(),
                outcome: "ok".into(),
                hash: "ab".into(),
            }],
            Chain::Verified,
        );
        assert_eq!(s.on_key(Key::F(3)), Action::Show(Tab::Approvals));
        assert!(drawn(&s, 100, 24).contains("Approvals  1 waiting"));
        assert_eq!(
            s.on_key(Key::Char('a')),
            Action::Decide {
                id: "apr-1".into(),
                approve: true
            }
        );
        assert!(drawn(&s, 100, 24)
            .lines()
            .nth(23)
            .unwrap()
            .contains("a approve"));
        assert_eq!(s.on_key(Key::F(4)), Action::Show(Tab::Audit));
        let t = drawn(&s, 100, 24);
        assert!(
            t.contains("Audit  1 records") && t.contains("Chain verified"),
            "{t}"
        );
        assert!(t.lines().nth(23).unwrap().contains("show problems only"));
    }
}
