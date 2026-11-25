//! The desktop: windows, menu bar, icons and dialogs, driven by mouse and keyboard events.
//!
//! The desktop owns only the look and the arrangement. What the screens show and do belongs to
//! the [`Shell`] that is passed in with each event; anything the screens ask the rest of MOULI to
//! do comes back as an [`Action`].

use crate::input::{Button, MouseEvent};
use crate::menu::{Command, MenuBar, MENU_H};
use crate::wm::{Hit, WindowManager};
use mouli_screen::key::Key;
use mouli_screen::screen::{Action, Tab};
use mouli_screen::shell::Shell;

/// How close together two clicks must be, in milliseconds, to count as a double click.
pub const DOUBLE_CLICK_MS: u64 = 500;
/// Spacing of the desktop icons.
pub const ICON_CELL: i32 = 76;

/// What pressing a dialog button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertResult {
    /// Just close the dialog.
    Close,
    /// Power the machine off.
    PowerOff,
    /// Restart the machine.
    Reboot,
}

/// A button of a dialog box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertButton {
    /// The label.
    pub label: &'static str,
    /// What it does.
    pub result: AlertResult,
}

/// A dialog box with a message and one or more buttons (OK for a plain message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// The lines of text.
    pub lines: Vec<String>,
    /// The buttons, left to right; the last is the default and is what Enter and Escape choose.
    pub buttons: Vec<AlertButton>,
}

/// One icon on the desktop.
#[derive(Debug, Clone, Copy)]
pub struct IconSpot {
    /// The name under the icon.
    pub label: &'static str,
    /// The screen it opens; `None` is the disk, which shows the About box.
    pub tab: Option<Tab>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    Close(u32),
    Zoom(u32),
    Drag { id: u32, dx: i32, dy: i32 },
    Resize(u32),
}

/// The desktop's state.
#[derive(Debug, Clone)]
pub struct Desktop {
    /// Width of the screen in pixels.
    pub width: i32,
    /// Height of the screen in pixels.
    pub height: i32,
    /// The windows.
    pub wm: WindowManager,
    /// The menu bar.
    pub menu: MenuBar,
    /// Where the pointer is.
    pub pointer: (i32, i32),
    /// Text in the menu bar, left of the system area.
    pub status: String,
    /// What the system area of the menu bar shows (network, Bluetooth, battery, clock).
    pub system: crate::status::SystemStatus,
    /// The icons, top to bottom along the right edge.
    pub icons: Vec<IconSpot>,
    /// The selected icon.
    pub selected_icon: Option<usize>,
    /// The dialog on screen, if any. While it shows, nothing else reacts.
    pub alert: Option<Alert>,
    press: Option<Press>,
    menu_press: bool,
    last_icon_click: Option<(usize, u64)>,
}

/// Applications taken out of this version of the desktop (their code is kept for later versions):
/// no icon, no menu entry and no function key.
pub const MOVED_OUT: [Tab; 3] = [Tab::Sheet, Tab::Studio, Tab::Player];

impl Desktop {
    /// A desktop for a screen of `width` by `height` pixels (at least 640 by 480).
    pub fn new(width: i32, height: i32) -> Self {
        let icons = vec![
            IconSpot {
                label: "MOULI HD",
                tab: None,
            },
            IconSpot {
                label: "Chat",
                tab: Some(Tab::Chat),
            },
            IconSpot {
                label: "Models",
                tab: Some(Tab::Models),
            },
            IconSpot {
                label: "Code",
                tab: Some(Tab::Editor),
            },
        ];
        Self {
            width,
            height,
            wm: WindowManager::new(width, height, MENU_H),
            menu: MenuBar::standard(width),
            pointer: (width / 2, height / 2),
            status: String::new(),
            system: crate::status::SystemStatus::default(),
            icons,
            selected_icon: None,
            alert: None,
            press: None,
            menu_press: false,
            last_icon_click: None,
        }
    }

    /// Where icon `i` sits. Icons run down the right edge; when they do not all fit in one column
    /// they continue in the next column to the left.
    pub fn icon_rect(&self, i: usize) -> crate::geom::Rect {
        let per_column = ((self.height - MENU_H - 12) / ICON_CELL).max(1) as usize;
        let (column, row) = ((i / per_column) as i32, (i % per_column) as i32);
        crate::geom::Rect::new(
            self.width - ICON_CELL * (column + 1),
            MENU_H + 12 + row * ICON_CELL,
            ICON_CELL,
            ICON_CELL - 4,
        )
    }

    fn icon_at(&self, px: i32, py: i32) -> Option<usize> {
        (0..self.icons.len()).find(|&i| self.icon_rect(i).contains(px, py))
    }

    /// Opens the window for `tab` and makes it the one that gets keys.
    pub fn open(&mut self, shell: &mut Shell, tab: Tab) {
        self.wm.open(tab);
        shell.active = tab;
    }

    /// The dialog's frame and its last button (OK for a plain message).
    pub fn alert_rects(&self, alert: &Alert) -> (crate::geom::Rect, crate::geom::Rect) {
        let (frame, buttons) = self.alert_layout(alert);
        let last = buttons
            .last()
            .copied()
            .unwrap_or_else(|| crate::geom::Rect::new(frame.x, frame.y, 0, 0));
        (frame, last)
    }

    /// The dialog's frame and the rectangle of each button, left to right.
    pub fn alert_layout(&self, alert: &Alert) -> (crate::geom::Rect, Vec<crate::geom::Rect>) {
        use crate::geom::Rect;
        use mouli_screen::typeface::ui_medium;
        const BUTTON_W: i32 = 104;
        const GAP: i32 = 12;
        let longest = alert
            .lines
            .iter()
            .map(|l| crate::paint::text_width(ui_medium(), l))
            .max()
            .unwrap_or(0);
        let n = alert.buttons.len().max(1) as i32;
        let buttons_w = n * BUTTON_W + (n - 1) * GAP;
        let w = (longest + 56)
            .max(buttons_w + 40)
            .clamp(340, (self.width - 20).max(340));
        let h = 24 + alert.lines.len() as i32 * 22 + 16 + 36 + 20;
        let d = Rect::new((self.width - w) / 2, (self.height - h) / 3, w, h);
        let mut x = d.right() - 20 - buttons_w;
        let rects = (0..n)
            .map(|_| {
                let r = Rect::new(x, d.bottom() - 20 - 36, BUTTON_W, 36);
                x += BUTTON_W + GAP;
                r
            })
            .collect();
        (d, rects)
    }

    fn show_alert(&mut self, lines: &[&str]) {
        self.alert = Some(Alert {
            lines: lines.iter().map(|s| (*s).to_string()).collect(),
            buttons: vec![AlertButton {
                label: "OK",
                result: AlertResult::Close,
            }],
        });
    }

    fn show_info(&mut self, lines: Vec<String>) {
        self.alert = Some(Alert {
            lines,
            buttons: vec![AlertButton {
                label: "OK",
                result: AlertResult::Close,
            }],
        });
    }

    /// Asks what to do about power: shut down, restart or cancel.
    pub fn show_power_dialog(&mut self) {
        self.alert = Some(Alert {
            lines: vec![
                "Power".into(),
                String::new(),
                "What do you want to do?".into(),
            ],
            buttons: vec![
                AlertButton {
                    label: "Shut Down",
                    result: AlertResult::PowerOff,
                },
                AlertButton {
                    label: "Restart",
                    result: AlertResult::Reboot,
                },
                AlertButton {
                    label: "Cancel",
                    result: AlertResult::Close,
                },
            ],
        });
    }

    fn show_restart_dialog(&mut self) {
        self.alert = Some(Alert {
            lines: vec!["Restart MOULI OS?".into()],
            buttons: vec![
                AlertButton {
                    label: "Restart",
                    result: AlertResult::Reboot,
                },
                AlertButton {
                    label: "Cancel",
                    result: AlertResult::Close,
                },
            ],
        });
    }

    fn execute(&mut self, shell: &mut Shell, cmd: Command) -> Action {
        match cmd {
            Command::Nothing => Action::None,
            Command::About => {
                self.show_alert(&[
                    "MOULI",
                    "",
                    "An AI-native operating system",
                    "that runs on your own machine.",
                    "",
                    concat!("Desktop version ", env!("CARGO_PKG_VERSION")),
                ]);
                Action::None
            }
            Command::Help => {
                self.show_alert(&[
                    "F1 Chat  F2 Models  F9 Code",
                    "Ctrl-N     new conversation",
                    "Ctrl-W     close the window",
                    "Ctrl-Q     shut down",
                    "Esc        close a menu",
                ]);
                Action::None
            }
            Command::SwitchUser => Action::SwitchUser,
            Command::NewConversation => {
                self.open(shell, Tab::Chat);
                Action::NewConversation
            }
            Command::CloseWindow => {
                if let Some(id) = self.wm.top().map(|w| w.id) {
                    self.wm.close(id);
                }
                Action::None
            }
            Command::Quit => Action::Quit,
            Command::Restart => {
                self.show_restart_dialog();
                Action::None
            }
            Command::Open(tab) => {
                self.open(shell, tab);
                Action::Show(tab)
            }
            Command::Zoom => {
                if let Some(id) = self.wm.top().map(|w| w.id) {
                    self.wm.toggle_zoom(id);
                }
                Action::None
            }
            Command::Cascade => {
                self.wm.cascade();
                Action::None
            }
            Command::Tile => {
                self.wm.tile();
                Action::None
            }
        }
    }

    fn has_window(&self) -> bool {
        self.wm.top().is_some()
    }

    /// Handles a key: dialogs and menus first, then the desktop's shortcuts, then the front
    /// window's screen.
    pub fn on_key(&mut self, shell: &mut Shell, key: Key) -> Action {
        if self.alert.is_some() {
            // Enter, Escape and Space choose the default button, which is the last one (OK or
            // Cancel), so a stray key never powers the machine off.
            if matches!(key, Key::Enter | Key::Esc | Key::Char(' ')) {
                self.alert = None;
            }
            return Action::None;
        }
        if self.menu.open.is_some() {
            if key == Key::Esc {
                self.menu.open = None;
                self.menu.hover = None;
            }
            return Action::None;
        }
        let command = match key {
            Key::Ctrl('n') => Some(Command::NewConversation),
            Key::Ctrl('w') => Some(Command::CloseWindow),
            Key::Ctrl('q') => Some(Command::Quit),
            Key::F(n @ 1..=10) => Tab::ALL
                .get(usize::from(n) - 1)
                .filter(|t| !MOVED_OUT.contains(t))
                .map(|t| Command::Open(*t)),
            _ => None,
        };
        if let Some(cmd) = command {
            return self.execute(shell, cmd);
        }
        match self.wm.top().map(|w| w.tab) {
            Some(tab) => {
                shell.active = tab;
                shell.screen_mut(tab).on_key(key)
            }
            None => Action::None,
        }
    }

    /// Handles a mouse event. `now_ms` is a monotonic clock, used to recognise double clicks.
    pub fn on_mouse(&mut self, shell: &mut Shell, ev: MouseEvent, now_ms: u64) -> Action {
        match ev {
            MouseEvent::Move { dx, dy } => {
                self.pointer.0 = (self.pointer.0 + dx).clamp(0, self.width - 1);
                self.pointer.1 = (self.pointer.1 + dy).clamp(0, self.height - 1);
                self.pointer_moved();
                Action::None
            }
            MouseEvent::Down(Button::Left) => self.press_left(shell, now_ms),
            MouseEvent::Up(Button::Left) => self.release_left(shell),
            MouseEvent::Wheel(n) => self.wheel(shell, n),
            MouseEvent::Down(Button::Right) | MouseEvent::Up(Button::Right) => Action::None,
        }
    }

    fn pointer_moved(&mut self) {
        let (px, py) = self.pointer;
        if self.menu.open.is_some() {
            self.menu.hover = self.menu.item_at(px, py);
            if let Some(i) = self.menu.title_at(px, py) {
                if self.menu.open != Some(i) {
                    self.menu.open = Some(i);
                    self.menu.hover = None;
                }
            }
        }
        match self.press {
            Some(Press::Drag { id, dx, dy }) => self.wm.move_to(id, px - dx, py - dy),
            Some(Press::Resize(id)) => self.wm.resize_to(id, px, py),
            _ => {}
        }
    }

    fn press_left(&mut self, shell: &mut Shell, now_ms: u64) -> Action {
        let (px, py) = self.pointer;
        if let Some(alert) = &self.alert {
            let (_, rects) = self.alert_layout(alert);
            let pressed = rects
                .iter()
                .position(|r| r.contains(px, py))
                .and_then(|i| alert.buttons.get(i).copied());
            if let Some(b) = pressed {
                self.alert = None;
                return match b.result {
                    AlertResult::Close => Action::None,
                    AlertResult::PowerOff => Action::Quit,
                    AlertResult::Reboot => Action::Reboot,
                };
            }
            return Action::None;
        }
        if self.menu.open.is_some() {
            let chosen = self.menu.item_at(px, py).and_then(|k| {
                let open = self.menu.open?;
                self.menu.menus[open].items[k].command
            });
            self.menu.open = None;
            self.menu.hover = None;
            return match chosen {
                Some(cmd) if MenuBar::enabled(cmd, self.has_window()) => self.execute(shell, cmd),
                // A click on the open title, or anywhere else, just closes the menu.
                _ => Action::None,
            };
        }
        if py < MENU_H {
            if let Some(item) = crate::status::item_at(self.width, &self.system, px, py) {
                if item == crate::status::StatusItem::Power {
                    self.show_power_dialog();
                } else {
                    let lines = crate::status::info_lines(item, &self.system);
                    self.show_info(lines);
                }
                return Action::None;
            }
            if let Some(i) = self.menu.title_at(px, py) {
                self.menu.open = Some(i);
                self.menu.hover = None;
                self.menu_press = true;
            }
            return Action::None;
        }
        match self.wm.hit(px, py) {
            Some(Hit::Close(id)) => {
                self.focus(shell, id);
                self.press = Some(Press::Close(id));
            }
            Some(Hit::Zoom(id)) => {
                self.focus(shell, id);
                self.press = Some(Press::Zoom(id));
            }
            Some(Hit::Title(id)) => {
                self.focus(shell, id);
                if let Some(w) = self.wm.get(id) {
                    self.press = Some(Press::Drag {
                        id,
                        dx: px - w.x,
                        dy: py - w.y,
                    });
                }
            }
            Some(Hit::Grow(id)) => {
                self.focus(shell, id);
                self.press = Some(Press::Resize(id));
            }
            Some(Hit::Content { id, .. } | Hit::Frame(id)) => self.focus(shell, id),
            None => return self.click_desktop(shell, px, py, now_ms),
        }
        Action::None
    }

    fn focus(&mut self, shell: &mut Shell, id: u32) {
        self.wm.raise(id);
        if let Some(tab) = self.wm.get(id).map(|w| w.tab) {
            shell.active = tab;
        }
        self.selected_icon = None;
    }

    fn click_desktop(&mut self, shell: &mut Shell, px: i32, py: i32, now_ms: u64) -> Action {
        let Some(i) = self.icon_at(px, py) else {
            self.selected_icon = None;
            self.last_icon_click = None;
            return Action::None;
        };
        self.selected_icon = Some(i);
        let double = matches!(self.last_icon_click,
            Some((j, t)) if j == i && now_ms.saturating_sub(t) <= DOUBLE_CLICK_MS);
        if !double {
            self.last_icon_click = Some((i, now_ms));
            return Action::None;
        }
        self.last_icon_click = None;
        match self.icons[i].tab {
            Some(tab) => self.execute(shell, Command::Open(tab)),
            None => self.execute(shell, Command::About),
        }
    }

    fn release_left(&mut self, shell: &mut Shell) -> Action {
        let (px, py) = self.pointer;
        if self.menu.open.is_some() && self.menu_press {
            self.menu_press = false;
            // Press, drag and release on an item chooses it; releasing anywhere else leaves the
            // menu open for a second click.
            let chosen = self.menu.item_at(px, py).and_then(|k| {
                let open = self.menu.open?;
                self.menu.menus[open].items[k].command
            });
            return match chosen {
                Some(cmd) if MenuBar::enabled(cmd, self.has_window()) => {
                    self.menu.open = None;
                    self.menu.hover = None;
                    self.execute(shell, cmd)
                }
                _ => Action::None,
            };
        }
        match self.press.take() {
            Some(Press::Close(id))
                if self
                    .wm
                    .get(id)
                    .is_some_and(|w| w.close_box().contains(px, py)) =>
            {
                self.wm.close(id);
            }
            Some(Press::Zoom(id))
                if self
                    .wm
                    .get(id)
                    .is_some_and(|w| w.zoom_box().contains(px, py)) =>
            {
                self.wm.toggle_zoom(id);
            }
            _ => {}
        }
        Action::None
    }

    fn wheel(&mut self, shell: &mut Shell, n: i32) -> Action {
        if self.alert.is_some() || self.menu.open.is_some() || n == 0 {
            return Action::None;
        }
        let (px, py) = self.pointer;
        let Some(Hit::Content { id, .. }) = self.wm.hit(px, py) else {
            return Action::None;
        };
        let Some(tab) = self.wm.get(id).map(|w| w.tab) else {
            return Action::None;
        };
        let up = n > 0;
        let screen = shell.screen_mut(tab);
        if tab == Tab::Chat {
            screen.on_key(if up { Key::PageUp } else { Key::PageDown })
        } else {
            let mut last = Action::None;
            for _ in 0..3 {
                last = screen.on_key(if up { Key::Up } else { Key::Down });
            }
            last
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouli_screen::screens::{ApprovalRow, ModelRow};

    fn setup() -> (Desktop, Shell) {
        (Desktop::new(1024, 768), Shell::new())
    }

    fn goto(d: &mut Desktop, s: &mut Shell, x: i32, y: i32) {
        let (px, py) = d.pointer;
        d.on_mouse(
            s,
            MouseEvent::Move {
                dx: x - px,
                dy: y - py,
            },
            0,
        );
    }

    fn click(d: &mut Desktop, s: &mut Shell, x: i32, y: i32, t: u64) -> Action {
        goto(d, s, x, y);
        let a = d.on_mouse(s, MouseEvent::Down(Button::Left), t);
        let b = d.on_mouse(s, MouseEvent::Up(Button::Left), t + 1);
        if a == Action::None {
            b
        } else {
            a
        }
    }

    fn centre(r: crate::geom::Rect) -> (i32, i32) {
        (r.x + r.w / 2, r.y + r.h / 2)
    }

    #[test]
    fn a_double_click_on_an_icon_opens_its_window_a_single_click_only_selects() {
        let (mut d, mut s) = setup();
        let (x, y) = centre(d.icon_rect(3)); // Code
        assert_eq!(click(&mut d, &mut s, x, y, 1000), Action::None);
        assert_eq!(d.selected_icon, Some(3));
        assert!(d.wm.top().is_none());
        // Too slow to be a double click.
        click(&mut d, &mut s, x, y, 1000 + DOUBLE_CLICK_MS + 50);
        assert!(d.wm.top().is_none());
        // A real double click.
        let a = click(&mut d, &mut s, x, y, 5000);
        assert_eq!(a, Action::None);
        let a = click(&mut d, &mut s, x, y, 5200);
        assert_eq!(a, Action::Show(Tab::Editor));
        assert_eq!(d.wm.top().map(|w| w.tab), Some(Tab::Editor));
        assert_eq!(s.active, Tab::Editor);
    }

    #[test]
    fn clicking_empty_desktop_deselects() {
        let (mut d, mut s) = setup();
        let (x, y) = centre(d.icon_rect(1));
        click(&mut d, &mut s, x, y, 0);
        click(&mut d, &mut s, 300, 600, 100);
        assert_eq!(d.selected_icon, None);
    }

    #[test]
    fn dragging_the_title_bar_moves_the_window_and_releasing_stops() {
        let (mut d, mut s) = setup();
        d.open(&mut s, Tab::Chat);
        let w = d.wm.top().unwrap().clone();
        let t = w.title_bar();
        goto(&mut d, &mut s, t.x + 200, t.y + 8);
        d.on_mouse(&mut s, MouseEvent::Down(Button::Left), 0);
        d.on_mouse(&mut s, MouseEvent::Move { dx: 60, dy: 40 }, 1);
        let moved = d.wm.top().unwrap().clone();
        assert_eq!((moved.x, moved.y), (w.x + 60, w.y + 40));
        d.on_mouse(&mut s, MouseEvent::Up(Button::Left), 2);
        d.on_mouse(&mut s, MouseEvent::Move { dx: 100, dy: 100 }, 3);
        let still = d.wm.top().unwrap();
        assert_eq!(
            (still.x, still.y),
            (moved.x, moved.y),
            "no drag after release"
        );
    }

    #[test]
    fn the_close_box_closes_only_if_released_inside_it() {
        let (mut d, mut s) = setup();
        d.open(&mut s, Tab::Chat);
        let b = d.wm.top().unwrap().close_box();
        let (cx, cy) = centre(b);
        goto(&mut d, &mut s, cx, cy);
        d.on_mouse(&mut s, MouseEvent::Down(Button::Left), 0);
        d.on_mouse(&mut s, MouseEvent::Move { dx: 100, dy: 100 }, 1);
        d.on_mouse(&mut s, MouseEvent::Up(Button::Left), 2);
        assert!(
            d.wm.top().is_some(),
            "released away from the box: stays open"
        );
        let b = d.wm.top().unwrap().close_box();
        let (cx, cy) = centre(b);
        click(&mut d, &mut s, cx, cy, 10);
        assert!(d.wm.top().is_none());
    }

    #[test]
    fn the_zoom_box_toggles_and_the_grow_box_resizes_in_whole_cells() {
        let (mut d, mut s) = setup();
        d.open(&mut s, Tab::Chat);
        let (zx, zy) = centre(d.wm.top().unwrap().zoom_box());
        click(&mut d, &mut s, zx, zy, 0);
        assert!(d.wm.top().unwrap().zoomed);
        let (zx, zy) = centre(d.wm.top().unwrap().zoom_box());
        click(&mut d, &mut s, zx, zy, 10);
        assert!(!d.wm.top().unwrap().zoomed);

        let g = d.wm.top().unwrap().grow_box();
        let c = d.wm.top().unwrap().content();
        goto(&mut d, &mut s, g.x + 5, g.y + 5);
        d.on_mouse(&mut s, MouseEvent::Down(Button::Left), 20);
        goto(
            &mut d,
            &mut s,
            c.x + 50 * crate::wm::CELL_W + 3,
            c.y + 12 * crate::wm::CELL_H + crate::wm::STATUS_H + 2,
        );
        d.on_mouse(&mut s, MouseEvent::Up(Button::Left), 21);
        let w = d.wm.top().unwrap();
        assert_eq!((w.cols, w.rows), (50, 12));
    }

    #[test]
    fn clicking_a_back_window_brings_it_forward_and_gives_it_the_keys() {
        let (mut d, mut s) = setup();
        d.open(&mut s, Tab::Approvals);
        d.open(&mut s, Tab::Chat);
        let back = d.wm.windows[0].clone();
        assert_eq!(back.tab, Tab::Approvals);
        let t = back.title_bar();
        click(&mut d, &mut s, t.x + 300, t.y + 5, 0);
        assert_eq!(d.wm.top().map(|w| w.tab), Some(Tab::Approvals));
        assert_eq!(s.active, Tab::Approvals);
    }

    #[test]
    fn governance_windows_are_off_the_desktop_but_still_reachable() {
        let (mut d, mut s) = setup();
        let labels: Vec<&str> = d.icons.iter().map(|i| i.label).collect();
        for gone in ["Approvals", "Audit", "Security", "Review"] {
            assert!(
                !labels.contains(&gone),
                "{gone} is not a desktop icon: {labels:?}"
            );
        }
        for kept in ["MOULI HD", "Chat", "Models", "Code"] {
            assert!(labels.contains(&kept), "{kept} stays: {labels:?}");
        }
        for moved in ["Sheet", "Studio", "Player"] {
            assert!(!labels.contains(&moved), "{moved} is moved out: {labels:?}");
        }
        // The keys still open them.
        for (n, tab) in [
            (3u8, Tab::Approvals),
            (4, Tab::Audit),
            (5, Tab::Security),
            (6, Tab::Review),
        ] {
            d.on_key(&mut s, Key::F(n));
            assert_eq!(d.wm.top().map(|w| w.tab), Some(tab), "F{n}");
        }
        // The View menu lists them under a greyed "Admin" heading.
        let view = &d.menu.menus[3];
        let titles: Vec<&str> = view.items.iter().map(|i| i.label).collect();
        let admin = titles.iter().position(|l| *l == "Admin").unwrap();
        assert!(
            view.items[admin].command == Some(crate::menu::Command::Nothing),
            "the heading does nothing"
        );
        for l in ["Approvals", "Audit", "Security", "Code Review"] {
            assert!(
                titles.iter().position(|x| *x == l).unwrap() > admin,
                "{l} is below the Admin heading"
            );
        }
    }

    #[test]
    fn menus_open_on_click_choose_items_and_ignore_disabled_ones() {
        let (mut d, mut s) = setup();
        let view = d.menu.title_rects()[3];
        click(&mut d, &mut s, view.x + 5, 8, 0);
        assert_eq!(d.menu.open, Some(3), "a click leaves the menu open");
        let item = d.menu.item_rects(3)[7]; // Admin > Security
        let a = click(&mut d, &mut s, item.x + 10, item.y + 5, 10);
        assert_eq!(a, Action::Show(Tab::Security));
        assert_eq!(d.menu.open, None);
        assert_eq!(d.wm.top().map(|w| w.tab), Some(Tab::Security));

        // A disabled item (Edit > Cut) closes the menu and does nothing.
        let edit = d.menu.title_rects()[2];
        click(&mut d, &mut s, edit.x + 5, 8, 20);
        let cut = d.menu.item_rects(2)[0];
        let before = d.wm.windows.len();
        assert_eq!(
            click(&mut d, &mut s, cut.x + 10, cut.y + 5, 30),
            Action::None
        );
        assert_eq!(d.menu.open, None);
        assert_eq!(d.wm.windows.len(), before);
    }

    #[test]
    fn press_drag_release_chooses_an_item_and_hover_switches_menus() {
        let (mut d, mut s) = setup();
        let file = d.menu.title_rects()[1];
        goto(&mut d, &mut s, file.x + 5, 8);
        d.on_mouse(&mut s, MouseEvent::Down(Button::Left), 0);
        // Slide across to the View title: that menu opens.
        let view = d.menu.title_rects()[3];
        goto(&mut d, &mut s, view.x + 5, 8);
        assert_eq!(d.menu.open, Some(3));
        let item = d.menu.item_rects(3)[1]; // Models
        goto(&mut d, &mut s, item.x + 10, item.y + 5);
        assert_eq!(d.menu.hover, Some(1));
        let a = d.on_mouse(&mut s, MouseEvent::Up(Button::Left), 5);
        assert_eq!(a, Action::Show(Tab::Models));
        assert_eq!(d.menu.open, None);
    }

    #[test]
    fn releasing_a_press_on_a_title_keeps_the_menu_open_and_escape_or_an_outside_click_closes_it() {
        let (mut d, mut s) = setup();
        let file = d.menu.title_rects()[1];
        click(&mut d, &mut s, file.x + 5, 8, 0);
        assert_eq!(d.menu.open, Some(1));
        d.on_key(&mut s, Key::Esc);
        assert_eq!(d.menu.open, None);
        click(&mut d, &mut s, file.x + 5, 8, 10);
        click(&mut d, &mut s, 500, 500, 20);
        assert_eq!(d.menu.open, None);
    }

    #[test]
    fn menu_items_that_need_a_window_are_refused_when_there_is_none() {
        let (mut d, mut s) = setup();
        let win = d.menu.title_rects()[4];
        click(&mut d, &mut s, win.x + 5, 8, 0);
        let tile = d.menu.item_rects(4)[2];
        click(&mut d, &mut s, tile.x + 5, tile.y + 5, 10);
        assert!(d.wm.windows.is_empty());
    }

    #[test]
    fn dialogs_block_everything_until_dismissed() {
        let (mut d, mut s) = setup();
        d.open(&mut s, Tab::Chat);
        let (x, y) = centre(d.icon_rect(0));
        click(&mut d, &mut s, x, y, 0);
        click(&mut d, &mut s, x, y, 100);
        let alert = d.alert.clone().expect("the disk icon shows About");
        // Keys and clicks elsewhere do nothing while it shows.
        assert_eq!(d.on_key(&mut s, Key::Char('a')), Action::None);
        assert_eq!(
            d.on_key(&mut s, Key::Ctrl('q')),
            Action::None,
            "no shut down behind a dialog"
        );
        click(&mut d, &mut s, 600, 600, 200);
        assert!(d.alert.is_some());
        let (_, ok) = d.alert_rects(&alert);
        let (ox, oy) = centre(ok);
        click(&mut d, &mut s, ox, oy, 300);
        assert!(d.alert.is_none());
        // Enter also dismisses.
        d.alert = Some(alert);
        d.on_key(&mut s, Key::Enter);
        assert!(d.alert.is_none());
    }

    #[test]
    fn shortcuts_and_keys_reach_the_right_place() {
        let (mut d, mut s) = setup();
        assert_eq!(
            d.on_key(&mut s, Key::Char('x')),
            Action::None,
            "no window: keys are ignored"
        );
        assert_eq!(d.on_key(&mut s, Key::F(5)), Action::Show(Tab::Security));
        assert_eq!(d.wm.top().map(|w| w.tab), Some(Tab::Security));
        assert_eq!(d.on_key(&mut s, Key::Ctrl('n')), Action::NewConversation);
        assert_eq!(
            d.wm.top().map(|w| w.tab),
            Some(Tab::Chat),
            "a new conversation shows the chat"
        );
        d.on_key(&mut s, Key::Char('h'));
        d.on_key(&mut s, Key::Char('i'));
        assert_eq!(d.on_key(&mut s, Key::Enter), Action::Send("hi".into()));
        assert_eq!(d.on_key(&mut s, Key::Ctrl('w')), Action::None);
        assert_eq!(
            d.wm.top().map(|w| w.tab),
            Some(Tab::Security),
            "the chat closed, security is next"
        );
        d.on_key(&mut s, Key::Ctrl('w'));
        assert!(d.wm.top().is_none());
        assert_eq!(d.on_key(&mut s, Key::Ctrl('q')), Action::Quit);
    }

    #[test]
    fn keys_go_to_the_front_window_only() {
        let (mut d, mut s) = setup();
        s.models.rows = (0..3)
            .map(|i| ModelRow {
                name: format!("m{i}"),
                size_mb: 1,
                arch: None,
                needs_mb: 1,
                fits: true,
            })
            .collect();
        d.open(&mut s, Tab::Models);
        d.open(&mut s, Tab::Approvals);
        d.on_key(&mut s, Key::Down);
        assert_eq!(s.models.cursor.selected, 0, "the Models window is behind");
        d.open(&mut s, Tab::Models);
        d.on_key(&mut s, Key::Down);
        assert_eq!(s.models.cursor.selected, 1);
    }

    #[test]
    fn the_wheel_scrolls_the_window_under_the_pointer() {
        let (mut d, mut s) = setup();
        s.approvals.set_rows(
            (0..6)
                .map(|i| ApprovalRow {
                    id: format!("a{i}"),
                    capability: "x.y".into(),
                    requester: "u".into(),
                    detail: String::new(),
                    can_decide: true,
                    why_not: String::new(),
                })
                .collect(),
        );
        d.open(&mut s, Tab::Approvals);
        let c = d.wm.top().unwrap().content();
        goto(&mut d, &mut s, c.x + 50, c.y + 50);
        d.on_mouse(&mut s, MouseEvent::Wheel(-1), 0);
        assert_eq!(s.approvals.cursor.selected, 3);
        d.on_mouse(&mut s, MouseEvent::Wheel(1), 1);
        assert_eq!(s.approvals.cursor.selected, 0);
        goto(&mut d, &mut s, 5, 700); // over the bare desktop
        d.on_mouse(&mut s, MouseEvent::Wheel(-1), 2);
        assert_eq!(s.approvals.cursor.selected, 0);
    }

    #[test]
    fn the_pointer_stays_on_the_screen() {
        let (mut d, mut s) = setup();
        d.on_mouse(
            &mut s,
            MouseEvent::Move {
                dx: -5000,
                dy: -5000,
            },
            0,
        );
        assert_eq!(d.pointer, (0, 0));
        d.on_mouse(&mut s, MouseEvent::Move { dx: 9000, dy: 9000 }, 0);
        assert_eq!(d.pointer, (1023, 767));
    }

    #[test]
    fn the_rendered_picture_has_the_right_size_and_look() {
        let (mut d, mut s) = setup();
        d.open(&mut s, Tab::Chat);
        d.open(&mut s, Tab::Audit);
        d.pointer = (700, 650);
        let c = d.render(&s);
        assert_eq!((c.width, c.height), (1024, 768));
        // The menu bar is the base surface with an outline under it; the desktop is the wallpaper.
        assert_eq!(c.get(500, 5), Some(crate::theme::SURFACE));
        assert_eq!(c.get(500, MENU_H as usize - 1), Some(crate::theme::OUTLINE));
        assert_eq!(c.get(5, 700), Some(crate::theme::WALLPAPER));
        // The pointer's tip is black at the hot spot.
        assert_eq!(c.get(700, 650), Some(0));
        // The front window's title bar is raised; the one behind it is on the base surface.
        let front = d.wm.top().unwrap().title_bar();
        assert_eq!(
            c.get((front.x + 60) as usize, (front.y + 4) as usize),
            Some(crate::theme::SURFACE_HIGH)
        );
        let back = d.wm.windows[0].title_bar();
        assert_eq!(
            c.get((back.x + 60) as usize, (back.y + 4) as usize),
            Some(crate::theme::SURFACE)
        );
    }

    #[test]
    fn a_windows_content_is_the_screen_drawn_at_the_cell_size() {
        use mouli_screen::grid::Grid;
        use mouli_screen::raster::render_text;
        use mouli_screen::screen::Screen;
        let (mut d, mut s) = setup();
        s.chat
            .push(mouli_screen::screens::Role::User, "hello desktop");
        d.open(&mut s, Tab::Chat);
        let w = d.wm.top().unwrap().clone();
        let mut grid = Grid::new(w.cols as usize, w.rows as usize);
        let area = grid.area();
        s.chat.draw(&mut grid, area);
        let expect = render_text(&grid);
        let got = d.render(&s);
        let k = w.content();
        for (x, y) in [(0usize, 0usize), (40, 30), (200, 25), (500, 300)] {
            assert_eq!(
                got.get(k.x as usize + x, k.y as usize + y),
                expect.get(x, y),
                "({x},{y})"
            );
        }
    }

    #[test]
    fn every_icon_is_fully_on_the_screen_and_none_overlap_at_supported_sizes() {
        for (w, h) in [(640, 480), (800, 600), (1024, 768), (1280, 1024)] {
            let d = Desktop::new(w, h);
            let rects: Vec<_> = (0..d.icons.len()).map(|i| d.icon_rect(i)).collect();
            for (i, r) in rects.iter().enumerate() {
                // The label sits 40 pixels below the picture and is about 16 high.
                assert!(r.x >= 0 && r.right() <= w, "{w}x{h} icon {i} {r:?}");
                assert!(
                    r.y + 40 + 18 <= h,
                    "{w}x{h} icon {i} label is cut off: {r:?}"
                );
                for o in rects.iter().skip(i + 1) {
                    let x = r.intersect(o);
                    assert!(x.w == 0 || x.h == 0, "{w}x{h} overlap {r:?} {o:?}");
                }
            }
        }
    }

    #[test]
    fn drawing_never_panics_at_supported_sizes_with_everything_open() {
        for (w, h) in [(640, 480), (800, 600), (1024, 768), (1280, 1024)] {
            let mut d = Desktop::new(w, h);
            let mut s = Shell::new();
            for t in Tab::ALL {
                d.open(&mut s, t);
            }
            d.wm.tile();
            d.menu.open = Some(3);
            d.menu.hover = Some(2);
            d.show_alert(&["one", "two"]);
            let c = d.render(&s);
            assert_eq!((c.width as i32, c.height as i32), (w, h));
            d.wm.cascade();
            d.render(&s);
        }
    }

    fn with_system(d: &mut Desktop) {
        use crate::status::{Battery, Bluetooth, NetKind, Network, SystemStatus};
        d.system = SystemStatus {
            time: "17:42".into(),
            date: "Tue 25 Nov".into(),
            zone: "UTC".into(),
            network: Network {
                kind: NetKind::Wifi,
                name: "HomeNet".into(),
                bars: 4,
            },
            bluetooth: Bluetooth::On,
            battery: Some(Battery {
                percent: 55,
                charging: true,
            }),
        };
    }

    fn status_centre(d: &Desktop, item: crate::status::StatusItem) -> (i32, i32) {
        let r = crate::status::layout(d.width, &d.system)
            .into_iter()
            .find(|(i, _)| *i == item)
            .unwrap()
            .1;
        (r.x + r.w / 2, 8)
    }

    #[test]
    fn the_power_button_asks_and_each_button_does_its_own_thing() {
        use crate::status::StatusItem;
        for (button, want) in [
            (0usize, Action::Quit),
            (1, Action::Reboot),
            (2, Action::None),
        ] {
            let (mut d, mut s) = setup();
            with_system(&mut d);
            let (x, y) = status_centre(&d, StatusItem::Power);
            assert_eq!(
                click(&mut d, &mut s, x, y, 0),
                Action::None,
                "the click only asks"
            );
            let alert = d.alert.clone().expect("a power dialog");
            assert_eq!(alert.buttons.len(), 3);
            assert_eq!(alert.buttons[0].label, "Shut Down");
            let (_, rects) = d.alert_layout(&alert);
            let r = rects[button];
            assert_eq!(
                click(&mut d, &mut s, r.x + 5, r.y + 5, 10),
                want,
                "button {button}"
            );
            assert!(d.alert.is_none());
        }
    }

    #[test]
    fn keys_in_the_power_dialog_never_power_off() {
        use crate::status::StatusItem;
        for key in [Key::Enter, Key::Esc, Key::Char(' ')] {
            let (mut d, mut s) = setup();
            with_system(&mut d);
            let (x, y) = status_centre(&d, StatusItem::Power);
            click(&mut d, &mut s, x, y, 0);
            assert_eq!(d.on_key(&mut s, key), Action::None);
            assert!(d.alert.is_none(), "{key:?} cancels");
        }
        // Other keys do nothing while the dialog is open.
        let (mut d, mut s) = setup();
        with_system(&mut d);
        let (x, y) = status_centre(&d, StatusItem::Power);
        click(&mut d, &mut s, x, y, 0);
        assert_eq!(d.on_key(&mut s, Key::Ctrl('q')), Action::None);
        assert!(d.alert.is_some());
    }

    #[test]
    fn a_click_beside_the_buttons_keeps_the_power_dialog_open() {
        use crate::status::StatusItem;
        let (mut d, mut s) = setup();
        with_system(&mut d);
        let (x, y) = status_centre(&d, StatusItem::Power);
        click(&mut d, &mut s, x, y, 0);
        click(&mut d, &mut s, 5, 400, 10);
        assert!(d.alert.is_some());
    }

    #[test]
    fn the_other_status_items_show_what_they_mean() {
        use crate::status::StatusItem;
        for (item, word) in [
            (StatusItem::Network, "HomeNet"),
            (StatusItem::Bluetooth, "Bluetooth"),
            (StatusItem::Battery, "55%"),
            (StatusItem::Clock, "17:42"),
        ] {
            let (mut d, mut s) = setup();
            with_system(&mut d);
            let (x, y) = status_centre(&d, item);
            assert_eq!(click(&mut d, &mut s, x, y, 0), Action::None);
            let alert = d.alert.clone().expect("an information dialog");
            assert!(
                alert.lines.join(" ").contains(word),
                "{item:?}: {:?}",
                alert.lines
            );
            assert_eq!(alert.buttons.len(), 1);
            let (_, ok) = d.alert_rects(&alert);
            click(&mut d, &mut s, ok.x + 5, ok.y + 5, 10);
            assert!(d.alert.is_none());
        }
    }

    #[test]
    fn file_restart_asks_before_restarting() {
        let (mut d, mut s) = setup();
        let file = d.menu.title_rects()[1];
        click(&mut d, &mut s, file.x + 5, 8, 0);
        let items: Vec<&str> = d.menu.menus[1].items.iter().map(|i| i.label).collect();
        let k = items
            .iter()
            .position(|l| *l == "Restart")
            .expect("Restart in File");
        let item = d.menu.item_rects(1)[k];
        assert_eq!(
            click(&mut d, &mut s, item.x + 10, item.y + 5, 10),
            Action::None
        );
        let alert = d.alert.clone().expect("a restart dialog");
        assert_eq!(alert.buttons.len(), 2);
        let (_, rects) = d.alert_layout(&alert);
        assert_eq!(
            click(&mut d, &mut s, rects[0].x + 5, rects[0].y + 5, 20),
            Action::Reboot
        );
    }

    #[test]
    fn the_status_text_stops_before_the_system_area() {
        let (mut d, _s) = setup();
        with_system(&mut d);
        let shell = Shell::new();
        d.status =
            "MOULI OS  |  12 waiting for approval (F3)  |  and a very long extra note".into();
        let with_text = d.render(&shell);
        d.status = String::new();
        let without = d.render(&shell);
        let left = crate::status::left_edge(d.width, &d.system);
        for y in 0..crate::menu::MENU_H {
            for x in left..d.width {
                assert_eq!(
                    with_text.get(x as usize, y as usize),
                    without.get(x as usize, y as usize),
                    "the text reached the system area at ({x},{y})"
                );
            }
        }
        // And the text itself was drawn somewhere to the left.
        let differs = (0..left).any(|x| {
            (0..crate::menu::MENU_H).any(|y| {
                with_text.get(x as usize, y as usize) != without.get(x as usize, y as usize)
            })
        });
        assert!(differs, "the status text is visible");
    }

    #[test]
    fn dialogs_with_three_buttons_fit_on_a_small_screen() {
        let mut d = Desktop::new(640, 480);
        d.show_power_dialog();
        let alert = d.alert.clone().unwrap();
        let (frame, rects) = d.alert_layout(&alert);
        for r in &rects {
            assert!(
                r.x >= frame.x && r.right() <= frame.right(),
                "{r:?} in {frame:?}"
            );
        }
        for w in rects.windows(2) {
            assert!(w[0].right() <= w[1].x);
        }
        assert!(frame.x >= 0 && frame.right() <= 640);
    }
}
