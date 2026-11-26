//! Joining a Wi-Fi network: scan, choose, type the password, connect.

use crate::grid::{theme, Grid, Rect};
use crate::key::Key;
use crate::screen::{Action, Cursor, Hidden, Screen};

/// What kind of login a network wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiSecurity {
    /// No password.
    Open,
    /// A password (WPA or WPA2).
    Password,
    /// Something this device cannot join yet.
    Unsupported,
}

/// One network seen in a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiRow {
    /// The network's name.
    pub ssid: String,
    /// Signal strength from 1 (weak) to 4 (strong).
    pub bars: u8,
    /// What login it wants.
    pub security: WifiSecurity,
}

/// Where the person is in the steps.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WifiPhase {
    /// Nothing asked yet.
    #[default]
    Idle,
    /// Looking for networks.
    Scanning,
    /// Choosing from the list.
    Choosing,
    /// Typing the password for this network.
    Password(String),
    /// Joining this network.
    Joining(String),
}

/// The Wi-Fi screen.
#[derive(Debug, Clone, Default)]
pub struct WifiScreen {
    /// Networks from the last scan, strongest first.
    pub rows: Vec<WifiRow>,
    /// The selection.
    pub cursor: Cursor,
    /// The step the person is at.
    pub phase: WifiPhase,
    /// What happened last (a failure, or the connection that was made).
    pub message: String,
    /// The password being typed (never shown, never printed).
    password: Hidden,
}

/// Longest password the box accepts (WPA allows 63).
const MAX_PASSWORD: usize = 63;

impl WifiScreen {
    /// Called by the host when the screen is shown: asks for a first scan if none was made.
    pub fn wants_first_scan(&mut self) -> bool {
        if self.phase == WifiPhase::Idle {
            self.phase = WifiPhase::Scanning;
            self.message.clear();
            return true;
        }
        false
    }

    /// A scan finished.
    pub fn scanned(&mut self, result: Result<Vec<WifiRow>, String>) {
        match result {
            Ok(rows) if rows.is_empty() => {
                self.rows.clear();
                self.message =
                    "No networks found. Move closer to the router, or check the Wi-Fi switch."
                        .into();
            }
            Ok(rows) => {
                self.rows = rows;
                self.message.clear();
            }
            Err(e) => {
                self.rows.clear();
                self.message = e;
            }
        }
        self.cursor.clamp(self.rows.len());
        self.phase = WifiPhase::Choosing;
    }

    /// Joining finished.
    pub fn joined(&mut self, result: Result<String, String>) {
        self.message = match result {
            Ok(summary) => summary,
            Err(e) => e,
        };
        self.phase = WifiPhase::Choosing;
    }

    fn start_scan(&mut self) -> Action {
        self.phase = WifiPhase::Scanning;
        self.message.clear();
        Action::WifiScan
    }

    fn choose(&mut self) -> Action {
        let Some(row) = self.rows.get(self.cursor.selected).cloned() else {
            return Action::None;
        };
        match row.security {
            WifiSecurity::Unsupported => {
                self.message = format!(
                    "{} uses WPA3-only or enterprise login, which is not supported yet.",
                    row.ssid
                );
                Action::None
            }
            WifiSecurity::Open => {
                self.phase = WifiPhase::Joining(row.ssid.clone());
                Action::WifiJoin {
                    ssid: row.ssid,
                    passphrase: None,
                }
            }
            WifiSecurity::Password => {
                self.password = Hidden::default();
                self.message.clear();
                self.phase = WifiPhase::Password(row.ssid);
                Action::None
            }
        }
    }
}

impl Screen for WifiScreen {
    fn on_key(&mut self, key: Key) -> Action {
        match self.phase.clone() {
            WifiPhase::Idle => self.start_scan(),
            WifiPhase::Scanning | WifiPhase::Joining(_) => Action::None,
            WifiPhase::Password(ssid) => {
                match key {
                    Key::Char(c) if !c.is_control() => {
                        if self.password.len() < MAX_PASSWORD {
                            self.password.push(c);
                        }
                    }
                    Key::Backspace => self.password.pop(),
                    Key::Esc => {
                        self.password = Hidden::default();
                        self.phase = WifiPhase::Choosing;
                    }
                    Key::Enter => {
                        if self.password.len() < 8 {
                            self.message = "A Wi-Fi password has 8 to 63 characters.".into();
                        } else {
                            let pass = std::mem::take(&mut self.password);
                            self.message.clear();
                            self.phase = WifiPhase::Joining(ssid.clone());
                            return Action::WifiJoin {
                                ssid,
                                passphrase: Some(pass),
                            };
                        }
                    }
                    _ => {}
                }
                Action::None
            }
            WifiPhase::Choosing => {
                let len = self.rows.len();
                match key {
                    Key::Up | Key::Char('k') => self.cursor.step(-1, len),
                    Key::Down | Key::Char('j') => self.cursor.step(1, len),
                    Key::Home => self.cursor.jump(false, len),
                    Key::End => self.cursor.jump(true, len),
                    Key::Char('r') => return self.start_scan(),
                    Key::Enter => return self.choose(),
                    _ => {}
                }
                Action::None
            }
        }
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.boxed(area, Some("Wi-Fi"), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 30 || inner.h < 6 {
            return;
        }
        let line = |g: &mut Grid, y: usize, text: &str, c| {
            g.text_fit(inner.x + 2, y, inner.w - 4, text, c, theme::BG);
        };
        match &self.phase {
            WifiPhase::Idle | WifiPhase::Scanning => {
                line(g, inner.y + 1, "Looking for networks...", theme::DIM);
            }
            WifiPhase::Joining(ssid) => {
                line(
                    g,
                    inner.y + 1,
                    &format!("Connecting to {ssid}..."),
                    theme::DIM,
                );
                line(g, inner.y + 2, "This can take half a minute.", theme::DIM);
            }
            WifiPhase::Password(ssid) => {
                line(g, inner.y + 1, &format!("Password for {ssid}"), theme::TEXT);
                let stars: String = "*".repeat(self.password.len());
                g.boxed(
                    Rect::new(inner.x + 2, inner.y + 3, inner.w - 4, 3),
                    Some("typing is hidden"),
                    theme::ACCENT,
                    theme::BG,
                );
                g.text_fit(
                    inner.x + 4,
                    inner.y + 4,
                    inner.w - 8,
                    &stars,
                    theme::TEXT,
                    theme::BG,
                );
            }
            WifiPhase::Choosing => {
                if self.rows.is_empty() {
                    line(
                        g,
                        inner.y + 1,
                        "No networks to show. Press r to look again.",
                        theme::DIM,
                    );
                }
                let list_h = inner.h.saturating_sub(5).max(1);
                let top = self.cursor.window(list_h);
                for (i, r) in self.rows.iter().enumerate().skip(top).take(list_h) {
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
                    let bars = "*".repeat(usize::from(r.bars.clamp(1, 4)));
                    g.text(inner.x + 3, y, &format!("{bars:<4}"), theme::OK, bg);
                    g.text_fit(
                        inner.x + 8,
                        y,
                        inner.w.saturating_sub(26),
                        &r.ssid,
                        theme::TEXT,
                        bg,
                    );
                    let (label, c) = match r.security {
                        WifiSecurity::Open => ("open", theme::WARN),
                        WifiSecurity::Password => ("password", theme::DIM),
                        WifiSecurity::Unsupported => ("not supported", theme::BAD),
                    };
                    let lx = (inner.x + inner.w).saturating_sub(16);
                    g.text_fit(lx, y, 15, label, c, bg);
                }
            }
        }
        if !self.message.is_empty() {
            g.text_fit(
                inner.x + 2,
                inner.y + inner.h - 2,
                inner.w - 4,
                &self.message,
                theme::WARN,
                theme::BG,
            );
        }
    }

    fn hints(&self) -> String {
        match self.phase {
            WifiPhase::Password(_) => "Enter connect  Esc back".into(),
            WifiPhase::Choosing => "Up/Down choose  Enter join  r look again".into(),
            _ => "Please wait".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ssid: &str, security: WifiSecurity) -> WifiRow {
        WifiRow {
            ssid: ssid.into(),
            bars: 3,
            security,
        }
    }

    fn with_rows() -> WifiScreen {
        let mut s = WifiScreen::default();
        s.scanned(Ok(vec![
            row("Home", WifiSecurity::Password),
            row("Cafe", WifiSecurity::Open),
            row("Corp", WifiSecurity::Unsupported),
        ]));
        s
    }

    fn drawn(s: &WifiScreen) -> String {
        let mut g = Grid::new(70, 14);
        let a = g.area();
        s.draw(&mut g, a);
        g.to_text()
    }

    fn type_text(s: &mut WifiScreen, t: &str) {
        for c in t.chars() {
            s.on_key(Key::Char(c));
        }
    }

    #[test]
    fn the_first_view_asks_for_one_scan_only() {
        let mut s = WifiScreen::default();
        assert!(s.wants_first_scan());
        assert!(!s.wants_first_scan());
        assert!(drawn(&s).contains("Looking for networks"));
        s.scanned(Err("No Wi-Fi adapter was found.".into()));
        assert_eq!(s.phase, WifiPhase::Choosing);
        assert!(drawn(&s).contains("No Wi-Fi adapter"));
        assert_eq!(s.on_key(Key::Char('r')), Action::WifiScan);
        assert_eq!(s.phase, WifiPhase::Scanning);
    }

    #[test]
    fn a_password_network_asks_for_a_hidden_password_then_joins() {
        let mut s = with_rows();
        assert!(drawn(&s).contains("Home") && drawn(&s).contains("password"));
        assert_eq!(s.on_key(Key::Enter), Action::None);
        assert_eq!(s.phase, WifiPhase::Password("Home".into()));
        type_text(&mut s, "hunter22x");
        let t = drawn(&s);
        assert!(t.contains("*********") && !t.contains("hunter"), "{t}");
        assert!(
            !format!("{s:?}").contains("hunter"),
            "Debug must not show it"
        );
        s.on_key(Key::Backspace);
        let a = s.on_key(Key::Enter);
        assert_eq!(
            a,
            Action::WifiJoin {
                ssid: "Home".into(),
                passphrase: Some(Hidden::from("hunter22"))
            }
        );
        assert!(
            !format!("{a:?}").contains("hunter"),
            "Debug must not show it"
        );
        assert_eq!(s.phase, WifiPhase::Joining("Home".into()));
        assert!(drawn(&s).contains("Connecting to Home"));
        // The box is empty if the person comes back to it.
        s.joined(Err("could not connect: the password may be wrong".into()));
        s.on_key(Key::Enter);
        assert_eq!(s.phase, WifiPhase::Password("Home".into()));
        assert!(!drawn(&s).contains('*'));
    }

    #[test]
    fn short_passwords_are_refused_and_esc_goes_back() {
        let mut s = with_rows();
        s.on_key(Key::Enter);
        type_text(&mut s, "short");
        assert_eq!(s.on_key(Key::Enter), Action::None);
        assert!(drawn(&s).contains("8 to 63"));
        s.on_key(Key::Esc);
        assert_eq!(s.phase, WifiPhase::Choosing);
    }

    #[test]
    fn open_networks_join_at_once_and_unsupported_ones_explain() {
        let mut s = with_rows();
        s.on_key(Key::Down);
        assert_eq!(
            s.on_key(Key::Enter),
            Action::WifiJoin {
                ssid: "Cafe".into(),
                passphrase: None
            }
        );
        s.joined(Ok("Connected: 192.168.1.20".into()));
        assert!(drawn(&s).contains("Connected: 192.168.1.20"));
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::Enter), Action::None);
        assert!(s.message.contains("not supported"));
        assert_eq!(s.phase, WifiPhase::Choosing);
    }

    #[test]
    fn keys_do_nothing_while_busy_and_the_password_is_capped() {
        let mut s = with_rows();
        s.on_key(Key::Char('r'));
        assert_eq!(s.on_key(Key::Enter), Action::None);
        let mut s = with_rows();
        s.on_key(Key::Enter);
        type_text(&mut s, &"x".repeat(100));
        match s.on_key(Key::Enter) {
            Action::WifiJoin { passphrase, .. } => assert_eq!(passphrase.unwrap().len(), 63),
            other => panic!("{other:?}"),
        }
        // Tiny areas and empty lists do not panic.
        let mut g = Grid::new(10, 3);
        let a = g.area();
        WifiScreen::default().draw(&mut g, a);
        let mut e = WifiScreen::default();
        e.scanned(Ok(vec![]));
        assert!(drawn(&e).contains("No networks found"));
    }
}
