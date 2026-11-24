//! Windows: where they are, which one is in front, and what a click on them means.
//!
//! A window shows one MOULI screen. Its content is a grid of character cells, so a window is
//! always a whole number of cells wide and high; resizing snaps to cells.

use crate::geom::Rect;
use mouli_screen::screen::Tab;

/// Width of one character cell in pixels (the screens are drawn at 16 by 20).
pub const CELL_W: i32 = 16;
/// Height of one character cell in pixels.
pub const CELL_H: i32 = 20;
/// Height of the title bar.
pub const TITLE_H: i32 = 22;
/// Height of the strip under the content (key hints and the grow box).
pub const STATUS_H: i32 = 16;
/// Smallest window, in cells.
pub const MIN_COLS: i32 = 30;
/// Smallest window, in rows.
pub const MIN_ROWS: i32 = 8;

/// One window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Identifier, unique for the life of the desktop.
    pub id: u32,
    /// The screen it shows.
    pub tab: Tab,
    /// Left edge of the whole window.
    pub x: i32,
    /// Top edge of the whole window.
    pub y: i32,
    /// Content width in cells.
    pub cols: i32,
    /// Content height in cells.
    pub rows: i32,
    /// Whether it is enlarged to fill the desktop.
    pub zoomed: bool,
    saved: (i32, i32, i32, i32),
}

impl Window {
    /// The whole window including its frame.
    pub fn outer(&self) -> Rect {
        Rect::new(
            self.x,
            self.y,
            self.cols * CELL_W + 2,
            1 + TITLE_H + 1 + self.rows * CELL_H + STATUS_H + 1,
        )
    }

    /// The title bar.
    pub fn title_bar(&self) -> Rect {
        Rect::new(self.x + 1, self.y + 1, self.cols * CELL_W, TITLE_H)
    }

    /// The close box.
    pub fn close_box(&self) -> Rect {
        Rect::new(self.x + 8, self.y + 1 + (TITLE_H - 14) / 2, 14, 14)
    }

    /// The zoom box.
    pub fn zoom_box(&self) -> Rect {
        Rect::new(
            self.x + 1 + self.cols * CELL_W - 8 - 14,
            self.y + 1 + (TITLE_H - 14) / 2,
            14,
            14,
        )
    }

    /// Where the screen is drawn.
    pub fn content(&self) -> Rect {
        Rect::new(
            self.x + 1,
            self.y + 1 + TITLE_H + 1,
            self.cols * CELL_W,
            self.rows * CELL_H,
        )
    }

    /// The strip under the content.
    pub fn status_strip(&self) -> Rect {
        let c = self.content();
        Rect::new(c.x, c.bottom(), c.w, STATUS_H)
    }

    /// The box in the bottom-right corner that resizes the window.
    pub fn grow_box(&self) -> Rect {
        let s = self.status_strip();
        Rect::new(s.right() - 16, s.y, 16, STATUS_H)
    }
}

/// What a point is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The close box of this window.
    Close(u32),
    /// The zoom box.
    Zoom(u32),
    /// The title bar (drag to move).
    Title(u32),
    /// The grow box (drag to resize).
    Grow(u32),
    /// The screen inside the window; `col` and `row` are the cell under the point.
    Content {
        /// The window.
        id: u32,
        /// Column of the cell.
        col: i32,
        /// Row of the cell.
        row: i32,
    },
    /// Some other part of the frame.
    Frame(u32),
}

/// All the windows, back to front.
#[derive(Debug, Clone)]
pub struct WindowManager {
    /// The windows, the last one is in front.
    pub windows: Vec<Window>,
    next_id: u32,
    width: i32,
    height: i32,
    menu_h: i32,
}

impl WindowManager {
    /// A manager for a desktop of `width` by `height` pixels whose menu bar is `menu_h` high.
    pub fn new(width: i32, height: i32, menu_h: i32) -> Self {
        Self {
            windows: Vec::new(),
            next_id: 1,
            width,
            height,
            menu_h,
        }
    }

    fn max_cols(&self) -> i32 {
        ((self.width - 2) / CELL_W).max(MIN_COLS)
    }

    fn max_rows(&self) -> i32 {
        ((self.height - self.menu_h - 3 - TITLE_H - STATUS_H) / CELL_H).max(MIN_ROWS)
    }

    /// The window in front.
    pub fn top(&self) -> Option<&Window> {
        self.windows.last()
    }

    /// The window with this id.
    pub fn get(&self, id: u32) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// Shows the window for `tab`: brings it to the front if it exists, otherwise opens it a
    /// little below and right of the previous one. Returns its id.
    pub fn open(&mut self, tab: Tab) -> u32 {
        if let Some(id) = self.windows.iter().find(|w| w.tab == tab).map(|w| w.id) {
            self.raise(id);
            return id;
        }
        let n = self.windows.len() as i32;
        let cols = 48.min(self.max_cols());
        let rows = 20.min(self.max_rows());
        let id = self.next_id;
        self.next_id += 1;
        let mut w = Window {
            id,
            tab,
            x: 24 + 28 * (n % 8),
            y: self.menu_h + 12 + 28 * (n % 8),
            cols,
            rows,
            zoomed: false,
            saved: (0, 0, cols, rows),
        };
        w.saved = (w.x, w.y, w.cols, w.rows);
        self.windows.push(w);
        self.clamp_into_view(id);
        id
    }

    /// Brings a window to the front.
    pub fn raise(&mut self, id: u32) {
        if let Some(i) = self.windows.iter().position(|w| w.id == id) {
            let w = self.windows.remove(i);
            self.windows.push(w);
        }
    }

    /// Closes a window.
    pub fn close(&mut self, id: u32) {
        self.windows.retain(|w| w.id != id);
    }

    /// What is under the point, looking from the front window backwards.
    pub fn hit(&self, px: i32, py: i32) -> Option<Hit> {
        for w in self.windows.iter().rev() {
            if !w.outer().contains(px, py) {
                continue;
            }
            let id = w.id;
            let is_front = self.windows.last().map(|t| t.id) == Some(id);
            if is_front && w.close_box().contains(px, py) {
                return Some(Hit::Close(id));
            }
            if is_front && w.zoom_box().contains(px, py) {
                return Some(Hit::Zoom(id));
            }
            if w.title_bar().contains(px, py) {
                return Some(Hit::Title(id));
            }
            if w.grow_box().contains(px, py) {
                return Some(Hit::Grow(id));
            }
            let c = w.content();
            if c.contains(px, py) {
                return Some(Hit::Content {
                    id,
                    col: (px - c.x) / CELL_W,
                    row: (py - c.y) / CELL_H,
                });
            }
            return Some(Hit::Frame(id));
        }
        None
    }

    /// Keeps the title bar reachable: the window may hang off the sides and the bottom, but its
    /// title bar must stay below the menu bar and at least partly on screen.
    fn clamp_into_view(&mut self, id: u32) {
        let (width, height, menu_h) = (self.width, self.height, self.menu_h);
        if let Some(w) = self.get_mut(id) {
            let ow = w.cols * CELL_W + 2;
            w.x = w.x.clamp(-(ow - 48), width - 48);
            w.y = w.y.clamp(menu_h, height - TITLE_H - 2);
        }
    }

    /// Moves a window so its top-left corner is at (`x`, `y`), within limits.
    pub fn move_to(&mut self, id: u32, x: i32, y: i32) {
        if let Some(w) = self.get_mut(id) {
            w.x = x;
            w.y = y;
            w.zoomed = false;
        }
        self.clamp_into_view(id);
    }

    /// Resizes a window so its bottom-right corner is as near (`px`, `py`) as whole cells allow,
    /// without growing past the screen from where the window is. A window that cannot reach its
    /// minimum size from where it sits is moved up and left to make room.
    pub fn resize_to(&mut self, id: u32, px: i32, py: i32) {
        let (width, height) = (self.width, self.height);
        if let Some(w) = self.get_mut(id) {
            let c = w.content();
            let room_cols = ((width - w.x - 2) / CELL_W).max(MIN_COLS);
            let room_rows = ((height - c.y - STATUS_H - 1) / CELL_H).max(MIN_ROWS);
            w.cols = ((px - c.x) / CELL_W).clamp(MIN_COLS, room_cols);
            w.rows = ((py - c.y - STATUS_H) / CELL_H).clamp(MIN_ROWS, room_rows);
            w.zoomed = false;
            let o = w.outer();
            if o.right() > width {
                w.x = (width - o.w).max(0);
            }
            if o.bottom() > height {
                w.y = (height - o.h).max(0);
            }
        }
        self.clamp_into_view(id);
    }

    /// Enlarges a window to fill the desktop, or puts it back.
    pub fn toggle_zoom(&mut self, id: u32) {
        let (max_cols, max_rows, menu_h) = (self.max_cols(), self.max_rows(), self.menu_h);
        if let Some(w) = self.get_mut(id) {
            if w.zoomed {
                (w.x, w.y, w.cols, w.rows) = w.saved;
                w.zoomed = false;
            } else {
                w.saved = (w.x, w.y, w.cols, w.rows);
                w.x = 0;
                w.y = menu_h;
                w.cols = max_cols;
                w.rows = max_rows;
                w.zoomed = true;
            }
        }
    }

    /// Stacks all windows with their title bars showing.
    pub fn cascade(&mut self) {
        let ids: Vec<u32> = self.windows.iter().map(|w| w.id).collect();
        for (n, id) in ids.into_iter().enumerate() {
            let n = n as i32;
            let (menu_h, mc, mr) = (self.menu_h, self.max_cols(), self.max_rows());
            if let Some(w) = self.get_mut(id) {
                w.zoomed = false;
                w.cols = 48.min(mc);
                w.rows = 20.min(mr);
                w.x = 24 + 28 * (n % 8);
                w.y = menu_h + 12 + 28 * (n % 8);
            }
            self.clamp_into_view(id);
        }
    }

    /// Lays all windows side by side without overlap, as large as they can be. When the windows
    /// cannot all be shown at their minimum size, they are cascaded instead.
    pub fn tile(&mut self) {
        let n = self.windows.len() as i32;
        if n == 0 {
            return;
        }
        let fits_across = (self.width / (MIN_COLS * CELL_W + 2)).max(1);
        let across = (1..=n).find(|k| k * k >= n).unwrap_or(1).min(fits_across);
        let down = (n + across - 1) / across;
        let cell_w = self.width / across;
        let cell_h = (self.height - self.menu_h) / down;
        let min_cell_h = 3 + TITLE_H + STATUS_H + MIN_ROWS * CELL_H;
        if cell_h < min_cell_h {
            self.cascade();
            return;
        }
        let ids: Vec<u32> = self.windows.iter().map(|w| w.id).collect();
        for (i, id) in ids.into_iter().enumerate() {
            let i = i as i32;
            let menu_h = self.menu_h;
            if let Some(w) = self.get_mut(id) {
                w.zoomed = false;
                w.cols = ((cell_w - 2) / CELL_W).max(MIN_COLS);
                w.rows = ((cell_h - 3 - TITLE_H - STATUS_H) / CELL_H).max(MIN_ROWS);
                w.x = (i % across) * cell_w;
                w.y = menu_h + (i / across) * cell_h;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wm() -> WindowManager {
        WindowManager::new(1024, 768, 24)
    }

    #[test]
    fn opening_cascades_and_reopening_raises() {
        let mut m = wm();
        let chat = m.open(Tab::Chat);
        let audit = m.open(Tab::Audit);
        assert_ne!(chat, audit);
        assert!(m.get(audit).unwrap().x > m.get(chat).unwrap().x);
        assert_eq!(m.top().unwrap().id, audit);
        assert_eq!(
            m.open(Tab::Chat),
            chat,
            "no second window for the same screen"
        );
        assert_eq!(m.top().unwrap().id, chat);
        assert_eq!(m.windows.len(), 2);
    }

    #[test]
    fn hits_name_the_part_under_the_pointer() {
        let mut m = wm();
        let id = m.open(Tab::Chat);
        let w = m.get(id).unwrap().clone();
        let c = w.close_box();
        assert_eq!(m.hit(c.x + 3, c.y + 3), Some(Hit::Close(id)));
        let z = w.zoom_box();
        assert_eq!(m.hit(z.x + 3, z.y + 3), Some(Hit::Zoom(id)));
        let t = w.title_bar();
        assert_eq!(m.hit(t.x + t.w / 2, t.y + 2), Some(Hit::Title(id)));
        let g = w.grow_box();
        assert_eq!(m.hit(g.x + 4, g.y + 4), Some(Hit::Grow(id)));
        let k = w.content();
        assert_eq!(
            m.hit(k.x + 2 * CELL_W + 3, k.y + 5 * CELL_H + 1),
            Some(Hit::Content { id, col: 2, row: 5 })
        );
        assert_eq!(m.hit(w.x, w.y + 100), Some(Hit::Frame(id)));
        assert_eq!(m.hit(1020, 760), None);
    }

    #[test]
    fn the_front_window_wins_where_windows_overlap() {
        let mut m = wm();
        let back = m.open(Tab::Chat);
        let front = m.open(Tab::Audit);
        let f = m.get(front).unwrap().content();
        let hit = m.hit(f.x + 5, f.y + 5);
        assert!(matches!(hit, Some(Hit::Content { id, .. }) if id == front));
        m.raise(back);
        assert!(matches!(m.hit(f.x + 400, f.y + 300), Some(Hit::Content { id, .. }) if id == back));
    }

    #[test]
    fn only_the_front_window_has_working_boxes() {
        let mut m = wm();
        let back = m.open(Tab::Chat);
        let front = m.open(Tab::Audit);
        let b = m.get(back).unwrap().close_box();
        // The back window's close box is covered by the front window? Use a point that is only on
        // the back window's frame and check it is not treated as a close click.
        let hit = m.hit(b.x + 1, b.y + 1);
        if let Some(Hit::Close(id)) = hit {
            assert_eq!(id, front, "a close box can only belong to the front window");
        }
    }

    #[test]
    fn dragging_cannot_lose_the_title_bar() {
        let mut m = wm();
        let id = m.open(Tab::Chat);
        m.move_to(id, -5000, -5000);
        let w = m.get(id).unwrap();
        assert!(w.y >= 24 && w.x + w.outer().w >= 48);
        m.move_to(id, 9000, 9000);
        let w = m.get(id).unwrap();
        assert!(w.x <= 1024 - 48 && w.y <= 768 - TITLE_H - 2);
    }

    #[test]
    fn resizing_snaps_to_whole_cells_within_limits() {
        let mut m = wm();
        let id = m.open(Tab::Chat);
        let c = m.get(id).unwrap().content();
        m.resize_to(id, c.x + 40 * CELL_W + 5, c.y + 15 * CELL_H + STATUS_H + 3);
        let w = m.get(id).unwrap();
        assert_eq!((w.cols, w.rows), (40, 15));
        m.resize_to(id, 0, 0);
        let w = m.get(id).unwrap();
        assert_eq!((w.cols, w.rows), (MIN_COLS, MIN_ROWS));
        m.resize_to(id, 99_999, 99_999);
        let w = m.get(id).unwrap().outer();
        assert!(w.right() <= 1024 && w.bottom() <= 768, "{w:?}");
    }

    #[test]
    fn zoom_fills_the_desktop_and_restores_exactly() {
        let mut m = wm();
        let id = m.open(Tab::Chat);
        let before = m.get(id).unwrap().clone();
        m.toggle_zoom(id);
        let z = m.get(id).unwrap();
        assert!(z.zoomed && z.x == 0 && z.y == 24);
        assert!(z.outer().w <= 1024 && z.outer().bottom() <= 768);
        m.toggle_zoom(id);
        let back = m.get(id).unwrap();
        assert_eq!(
            (back.x, back.y, back.cols, back.rows),
            (before.x, before.y, before.cols, before.rows)
        );
        assert!(!back.zoomed);
    }

    #[test]
    fn tiling_does_not_overlap_and_cascade_keeps_title_bars_visible() {
        let mut m = wm();
        for t in Tab::ALL {
            m.open(t);
        }
        m.tile();
        for (i, a) in m.windows.iter().enumerate() {
            for b in m.windows.iter().skip(i + 1) {
                let o = a.outer().intersect(&b.outer());
                assert!(
                    o.w == 0 || o.h == 0,
                    "{:?} overlaps {:?}",
                    a.outer(),
                    b.outer()
                );
            }
        }
        m.cascade();
        let ys: Vec<i32> = m.windows.iter().map(|w| w.y).collect();
        assert!(ys.windows(2).all(|p| p[1] > p[0]));
    }

    #[test]
    fn closing_removes_the_window() {
        let mut m = wm();
        let id = m.open(Tab::Chat);
        m.close(id);
        assert!(m.top().is_none() && m.hit(100, 100).is_none());
    }
}
