//! The tabbed shell: a header with the screens, the active screen, and a footer with key hints.

use crate::grid::{fit, theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Screen, Tab};
use crate::screens::{Chat, ModelPicker};

/// The whole interface.
#[derive(Debug, Clone)]
pub struct Shell {
    /// The conversation.
    pub chat: Chat,
    /// The model list.
    pub models: ModelPicker,
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

    fn current(&mut self) -> &mut dyn Screen {
        match self.active {
            // The other tabs share the model list's slot until their screens exist.
            Tab::Chat => &mut self.chat,
            _ => &mut self.models,
        }
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
        for (i, tab) in Tab::ALL.iter().enumerate() {
            let label = format!(" F{} {} ", i + 1, tab.label());
            let on = *tab == self.active;
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
        match self.active {
            Tab::Chat => self.chat.draw(grid, body),
            _ => self.models.draw(grid, body),
        }
        // Footer.
        grid.fill(Rect::new(0, area.h - 1, area.w, 1), theme::PANEL);
        let hints = match self.active {
            Tab::Chat => self.chat.hints(),
            _ => self.models.hints(),
        };
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
        let t = drawn(&s, 100, 20);
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
        assert!(drawn(&s, 100, 20)
            .lines()
            .nth(19)
            .unwrap()
            .contains("Enter send"));
    }

    #[test]
    fn all_six_tabs_fit_an_80_column_screen() {
        let t = drawn(&Shell::new(), 80, 20);
        let header = t.lines().next().unwrap();
        assert!(header.trim_end().ends_with("F6 Review"), "{header}");
        assert!(header.chars().count() <= 80);
    }

    #[test]
    fn the_status_is_shown_only_when_it_fits() {
        let mut s = Shell::new();
        s.status = "wlan0 connected".into();
        let narrow = drawn(&s, 80, 20);
        assert!(
            !narrow.lines().next().unwrap().contains("wlan0"),
            "no room at 80 columns"
        );
        assert!(
            !narrow.lines().next().unwrap().contains(".."),
            "no stray dots"
        );
        assert!(drawn(&s, 100, 20)
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
}
