//! What every screen has in common.

use crate::grid::{Grid, Rect};
use crate::key::Key;

/// What a screen asks the rest of MOULI to do. The screens never act themselves: they report
/// what the person chose, and the controller (which owns the model, the policy and the disk)
/// carries it out and updates the screens' data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do.
    None,
    /// Power off.
    Quit,
    /// Restart the machine.
    Reboot,
    /// Show another screen.
    Show(Tab),
    /// Send this text to the model (or run it as a command).
    Send(String),
    /// Forget the conversation and start again.
    NewConversation,
    /// Load the model at this index in the model list.
    ChooseModel(usize),
    /// Look for models again (a USB stick may have been plugged in).
    RescanModels,
    /// Look at the data disk again.
    RescanStorage,
    /// Remove the stored model at this index in the Storage list (the host decides if it may).
    DeleteStorage(usize),
    /// Open this web address in the browser (the host fetches it and reports back).
    Navigate(String),
    /// Open Wi-Fi setup.
    Wifi,
    /// Use a hosted model online.
    Hosted,
    /// Show the hardware report.
    HardwareInfo,
    /// Approve or refuse a held request.
    Decide {
        /// The approval id.
        id: String,
        /// True to approve, false to refuse.
        approve: bool,
    },
    /// Choose another user to work as.
    SwitchUser,
    /// Run a capability with these arguments, through the governed gate.
    Call {
        /// The capability, for example `sheet.set`.
        capability: String,
        /// Its arguments, as name and value.
        args: Vec<(String, String)>,
    },
    /// Run a fresh security scan.
    RefreshSecurity,
    /// Apply the patch under review.
    ApplyPatch,
    /// Undo the patch that was applied.
    RollbackPatch,
    /// Throw the patch away.
    RejectPatch,
}

/// The screens, in the order of the tab bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    /// The conversation.
    Chat,
    /// Choosing and loading a model.
    Models,
    /// Requests waiting for a decision.
    Approvals,
    /// The tamper-evident record of what happened.
    Audit,
    /// Risks, anomalies and cases.
    Security,
    /// Reviewing a proposed code change.
    Review,
    /// The spreadsheet.
    Sheet,
    /// The document editor.
    Studio,
    /// The code editor.
    Editor,
    /// The video and YouTube player.
    Player,
    /// What is on the data disk.
    Storage,
    /// A rendered preview of the file open in the code editor.
    Preview,
    /// The web browser.
    Browser,
}

impl Tab {
    /// All tabs in display order.
    pub const ALL: [Tab; 13] = [
        Tab::Chat,
        Tab::Models,
        Tab::Approvals,
        Tab::Audit,
        Tab::Security,
        Tab::Review,
        Tab::Sheet,
        Tab::Studio,
        Tab::Editor,
        Tab::Player,
        Tab::Storage,
        Tab::Preview,
        Tab::Browser,
    ];

    /// The name shown in the tab bar.
    pub const fn label(self) -> &'static str {
        match self {
            Tab::Chat => "Chat",
            Tab::Models => "Models",
            Tab::Approvals => "Approvals",
            Tab::Audit => "Audit",
            Tab::Security => "Security",
            Tab::Review => "Review",
            Tab::Sheet => "Sheet",
            Tab::Studio => "Studio",
            Tab::Editor => "Code",
            Tab::Player => "Player",
            Tab::Storage => "Storage",
            Tab::Preview => "Preview",
            Tab::Browser => "Browser",
        }
    }

    /// The key that opens it: F1 to F12 for the first twelve, Ctrl-B for the browser.
    pub const fn key(self) -> Key {
        match self {
            Tab::Browser => Key::Ctrl('b'),
            t => Key::F(t as u8 + 1),
        }
    }

    /// The key as shown to people (`F3`, `^B`).
    pub fn key_name(self) -> String {
        match self {
            Tab::Browser => "^B".into(),
            t => format!("F{}", t as u8 + 1),
        }
    }
}

/// A screen.
pub trait Screen {
    /// Reacts to a key press.
    fn on_key(&mut self, key: Key) -> Action;

    /// Draws the screen into `area` of `grid`.
    fn draw(&self, grid: &mut Grid, area: Rect);

    /// A short line of key hints for the footer.
    fn hints(&self) -> String;
}

/// A selectable list's cursor: keeps `selected` inside `0..len` and the window scrolled so the
/// selection stays visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    /// The selected index.
    pub selected: usize,
    /// The first visible index.
    pub top: usize,
}

impl Cursor {
    /// Moves the selection by `delta` (negative is up), staying within `len` items.
    pub fn step(&mut self, delta: isize, len: usize) {
        if len == 0 {
            *self = Cursor::default();
            return;
        }
        let max = len - 1;
        let next = (self.selected as isize + delta).clamp(0, max as isize);
        self.selected = next as usize;
    }

    /// Jumps to the first or last item.
    pub fn jump(&mut self, last: bool, len: usize) {
        self.selected = if last { len.saturating_sub(1) } else { 0 };
    }

    /// Keeps the selection inside `len` after the list changed.
    pub fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
        self.top = self.top.min(self.selected);
    }

    /// The first visible index for a window of `height` rows, scrolled just enough to show the
    /// selection.
    pub fn window(&self, height: usize) -> usize {
        if height == 0 {
            return 0;
        }
        if self.selected < self.top {
            self.selected
        } else if self.selected >= self.top + height {
            self.selected + 1 - height
        } else {
            self.top
        }
    }

    /// Applies the window result back, so the next draw starts from it.
    pub fn follow(&mut self, height: usize) {
        self.top = self.window(height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_map_to_function_keys() {
        assert_eq!(Tab::Chat.key(), Key::F(1));
        assert_eq!(Tab::Review.key(), Key::F(6));
        assert_eq!(Tab::Studio.key(), Key::F(8));
        assert_eq!(Tab::ALL.len(), 13);
        assert_eq!(Tab::Browser.key(), Key::Ctrl('b'));
        assert_eq!(Tab::Browser.key_name(), "^B");
        assert_eq!(Tab::Chat.key_name(), "F1");
        assert_eq!(Tab::Storage.key(), Key::F(11));
        assert_eq!(Tab::Preview.key(), Key::F(12));
        assert_eq!(Tab::Editor.key(), Key::F(9));
        assert_eq!(Tab::Player.key(), Key::F(10));
        assert_eq!(
            Tab::ALL.iter().map(|t| t.label()).collect::<Vec<_>>(),
            vec![
                "Chat",
                "Models",
                "Approvals",
                "Audit",
                "Security",
                "Review",
                "Sheet",
                "Studio",
                "Code",
                "Player",
                "Storage",
                "Preview",
                "Browser"
            ]
        );
    }

    #[test]
    fn the_cursor_stays_in_range() {
        let mut c = Cursor::default();
        c.step(-5, 3);
        assert_eq!(c.selected, 0);
        c.step(1, 3);
        c.step(10, 3);
        assert_eq!(c.selected, 2);
        c.step(1, 0);
        assert_eq!(c, Cursor::default());
        c.jump(true, 7);
        assert_eq!(c.selected, 6);
        c.jump(false, 7);
        assert_eq!(c.selected, 0);
        c.selected = 9;
        c.clamp(4);
        assert_eq!(c.selected, 3);
        c.clamp(0);
        assert_eq!(c.selected, 0);
    }

    #[test]
    fn the_window_scrolls_to_keep_the_selection_visible() {
        let mut c = Cursor {
            selected: 0,
            top: 0,
        };
        assert_eq!(c.window(5), 0);
        c.selected = 7;
        assert_eq!(c.window(5), 3);
        c.follow(5);
        assert_eq!(c.top, 3);
        c.selected = 1;
        assert_eq!(c.window(5), 1);
        assert_eq!(c.window(0), 0);
    }
}
