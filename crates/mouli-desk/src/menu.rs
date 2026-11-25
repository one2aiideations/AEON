//! The menu bar and its pull-down menus.

use crate::geom::Rect;
use crate::paint::{fill, hline, outline, shadow, text, text_width};
use crate::theme::*;
use mouli_screen::raster::Canvas;
use mouli_screen::screen::Tab;
use mouli_screen::typeface::{ui, ui_medium, ui_small};

/// Height of the menu bar.
pub const MENU_H: i32 = 32;
/// Height of one menu item.
pub const ITEM_H: i32 = 32;
const SEPARATOR_H: i32 = 13;

/// What choosing a menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Nothing (a greyed-out item).
    Nothing,
    /// Show the About box.
    About,
    /// Choose another user.
    SwitchUser,
    /// Start a new conversation.
    NewConversation,
    /// Close the front window.
    CloseWindow,
    /// Shut the machine down.
    Quit,
    /// Ask whether to restart the machine.
    Restart,
    /// Show the window for a screen.
    Open(Tab),
    /// Enlarge or restore the front window.
    Zoom,
    /// Stack the windows.
    Cascade,
    /// Tile the windows.
    Tile,
    /// Show the key help.
    Help,
}

/// One line of a menu.
#[derive(Debug, Clone, Copy)]
pub struct Item {
    /// The text.
    pub label: &'static str,
    /// The shortcut shown on the right.
    pub shortcut: &'static str,
    /// What it does; `None` makes the line a separator.
    pub command: Option<Command>,
}

const fn item(label: &'static str, shortcut: &'static str, command: Command) -> Item {
    Item {
        label,
        shortcut,
        command: Some(command),
    }
}

const SEP: Item = Item {
    label: "",
    shortcut: "",
    command: None,
};

/// A pull-down menu.
#[derive(Debug, Clone)]
pub struct Menu {
    /// The title in the bar.
    pub title: &'static str,
    /// Its lines.
    pub items: Vec<Item>,
}

/// The menu bar and which menu is open.
#[derive(Debug, Clone)]
pub struct MenuBar {
    /// The menus, left to right.
    pub menus: Vec<Menu>,
    /// The open menu, if any.
    pub open: Option<usize>,
    /// The item the pointer is over in the open menu.
    pub hover: Option<usize>,
    width: i32,
}

impl MenuBar {
    /// The standard MOULI menus for a bar `width` pixels wide.
    pub fn standard(width: i32) -> Self {
        Self {
            menus: vec![
                Menu {
                    title: "MOULI",
                    items: vec![
                        item("About MOULI...", "", Command::About),
                        SEP,
                        item("Switch User", "", Command::SwitchUser),
                    ],
                },
                Menu {
                    title: "File",
                    items: vec![
                        item("New Conversation", "^N", Command::NewConversation),
                        item("Close Window", "^W", Command::CloseWindow),
                        SEP,
                        item("Shut Down", "^Q", Command::Quit),
                        item("Restart", "", Command::Restart),
                    ],
                },
                Menu {
                    title: "Edit",
                    items: vec![
                        item("Cut", "", Command::Nothing),
                        item("Copy", "", Command::Nothing),
                        item("Paste", "", Command::Nothing),
                    ],
                },
                Menu {
                    title: "View",
                    items: vec![
                        item("Chat", "F1", Command::Open(Tab::Chat)),
                        item("Models", "F2", Command::Open(Tab::Models)),
                        item("Code", "F9", Command::Open(Tab::Editor)),
                        item("Storage", "F11", Command::Open(Tab::Storage)),
                        SEP,
                        item("Admin", "", Command::Nothing),
                        item("Approvals", "F3", Command::Open(Tab::Approvals)),
                        item("Audit", "F4", Command::Open(Tab::Audit)),
                        item("Security", "F5", Command::Open(Tab::Security)),
                        item("Code Review", "F6", Command::Open(Tab::Review)),
                    ],
                },
                Menu {
                    title: "Window",
                    items: vec![
                        item("Zoom Window", "", Command::Zoom),
                        item("Cascade", "", Command::Cascade),
                        item("Tile", "", Command::Tile),
                    ],
                },
                Menu {
                    title: "Help",
                    items: vec![item("MOULI Help", "", Command::Help)],
                },
            ],
            open: None,
            hover: None,
            width,
        }
    }

    /// Whether `cmd` can be chosen now. Closing and zooming need a window; the Edit items are not
    /// connected to anything yet.
    pub fn enabled(cmd: Command, has_window: bool) -> bool {
        match cmd {
            Command::Nothing => false,
            Command::CloseWindow | Command::Zoom | Command::Cascade | Command::Tile => has_window,
            _ => true,
        }
    }

    /// The area of each title in the bar.
    pub fn title_rects(&self) -> Vec<Rect> {
        let mut x = 6;
        self.menus
            .iter()
            .map(|m| {
                let w = text_width(ui_medium(), m.title) + 28;
                let r = Rect::new(x, 0, w, MENU_H - 1);
                x += w;
                r
            })
            .collect()
    }

    /// The menu whose title is at the point.
    pub fn title_at(&self, px: i32, py: i32) -> Option<usize> {
        self.title_rects().iter().position(|r| r.contains(px, py))
    }

    fn item_height(it: &Item) -> i32 {
        if it.command.is_none() {
            SEPARATOR_H
        } else {
            ITEM_H
        }
    }

    /// The pull-down area of menu `i`, including its frame.
    pub fn dropdown_rect(&self, i: usize) -> Rect {
        let (title, menu) = (self.title_rects()[i], &self.menus[i]);
        let label_w = menu
            .items
            .iter()
            .map(|it| text_width(ui(), it.label) + text_width(ui_small(), it.shortcut) + 64)
            .max()
            .unwrap_or(120)
            .max(200);
        let h: i32 = menu.items.iter().map(Self::item_height).sum::<i32>() + 4;
        let x = title.x.min((self.width - label_w - 4).max(0));
        Rect::new(x, MENU_H - 1, label_w + 4, h)
    }

    /// The area of each item of menu `i`.
    pub fn item_rects(&self, i: usize) -> Vec<Rect> {
        let d = self.dropdown_rect(i);
        let mut y = d.y + 2;
        self.menus[i]
            .items
            .iter()
            .map(|it| {
                let r = Rect::new(d.x + 2, y, d.w - 4, Self::item_height(it));
                y += r.h;
                r
            })
            .collect()
    }

    /// The item of the open menu at the point.
    pub fn item_at(&self, px: i32, py: i32) -> Option<usize> {
        let open = self.open?;
        self.item_rects(open)
            .iter()
            .position(|r| r.contains(px, py))
            .filter(|&k| self.menus[open].items[k].command.is_some())
    }

    /// Draws the bar, with the open menu's title highlighted, and `status` at the right.
    pub fn draw_bar(&self, c: &mut Canvas, status: &str, right: i32) {
        let width = c.width as i32;
        let right = right.min(width);
        fill(c, Rect::new(0, 0, width, MENU_H), SURFACE);
        hline(c, 0, MENU_H - 1, width, OUTLINE);
        let face = ui_medium();
        let y = (MENU_H - face.line_height) / 2;
        for (i, (m, r)) in self.menus.iter().zip(self.title_rects()).enumerate() {
            let open = self.open == Some(i);
            if open {
                fill(c, r, HOVER);
            }
            // The first menu carries the brand, in the primary colour.
            let colour = if i == 0 { PRIMARY } else { ON_SURFACE };
            text(c, face, r.x + 14, y, m.title, colour);
        }
        if !status.is_empty() {
            let w = text_width(face, status);
            let x = (right - w - 14).max(0);
            text(c, face, x, y, status, ON_SURFACE_DIM);
        }
    }

    /// Draws the open menu's pull-down, if any.
    pub fn draw_dropdown(&self, c: &mut Canvas, has_window: bool) {
        let Some(open) = self.open else { return };
        let d = self.dropdown_rect(open);
        shadow(c, d);
        fill(c, d, SURFACE_HIGH);
        outline(c, d, OUTLINE);
        let (face, small) = (ui(), ui_small());
        for (k, (it, r)) in self.menus[open]
            .items
            .iter()
            .zip(self.item_rects(open))
            .enumerate()
        {
            let Some(cmd) = it.command else {
                hline(c, r.x + 8, r.y + r.h / 2, r.w - 16, OUTLINE);
                continue;
            };
            let enabled = Self::enabled(cmd, has_window);
            if enabled && self.hover == Some(k) {
                fill(c, r, HOVER);
            }
            let colour = if enabled { ON_SURFACE } else { DISABLED };
            text(
                c,
                face,
                r.x + 16,
                r.y + (r.h - face.line_height) / 2,
                it.label,
                colour,
            );
            let sw = text_width(small, it.shortcut);
            let dim = if enabled { ON_SURFACE_DIM } else { DISABLED };
            text(
                c,
                small,
                r.right() - sw - 16,
                r.y + (r.h - small.line_height) / 2,
                it.shortcut,
                dim,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_do_not_overlap_and_are_found_by_position() {
        let bar = MenuBar::standard(1024);
        let r = bar.title_rects();
        for w in r.windows(2) {
            assert!(w[0].right() <= w[1].x);
        }
        assert_eq!(bar.title_at(r[1].x + 2, 5), Some(1));
        assert_eq!(bar.title_at(900, 5), None);
        assert_eq!(
            bar.title_at(5, MENU_H + 5),
            None,
            "below the bar is not a title"
        );
    }

    #[test]
    fn items_are_found_inside_the_open_menu_and_separators_are_skipped() {
        let mut bar = MenuBar::standard(1024);
        assert_eq!(bar.item_at(10, 40), None, "nothing is open yet");
        bar.open = Some(0);
        let rects = bar.item_rects(0);
        assert_eq!(bar.item_at(rects[0].x + 4, rects[0].y + 4), Some(0));
        assert_eq!(
            bar.item_at(rects[1].x + 4, rects[1].y + 2),
            None,
            "the separator"
        );
        assert_eq!(bar.item_at(rects[2].x + 4, rects[2].y + 4), Some(2));
        assert_eq!(bar.item_at(2000, 2000), None);
    }

    #[test]
    fn dropdowns_stay_on_the_screen() {
        // 640 pixels is the smallest screen the desktop supports.
        let bar = MenuBar::standard(640);
        for i in 0..bar.menus.len() {
            let d = bar.dropdown_rect(i);
            assert!(
                d.x >= 0 && d.right() <= 640 + 4,
                "{} at {d:?}",
                bar.menus[i].title
            );
        }
    }

    #[test]
    fn commands_need_a_window_where_they_act_on_one() {
        assert!(!MenuBar::enabled(Command::CloseWindow, false));
        assert!(MenuBar::enabled(Command::CloseWindow, true));
        assert!(MenuBar::enabled(Command::Open(Tab::Chat), false));
        assert!(!MenuBar::enabled(Command::Nothing, true));
    }

    #[test]
    fn the_bar_and_a_dropdown_draw_without_panicking_at_small_sizes() {
        let mut bar = MenuBar::standard(200);
        bar.open = Some(3);
        bar.hover = Some(1);
        for (w, h) in [(0usize, 0usize), (10, 10), (200, 100), (1024, 768)] {
            let mut c = crate::paint::canvas(w, h, 0);
            bar.draw_bar(&mut c, "owner", 200);
            bar.draw_dropdown(&mut c, true);
        }
    }
}
