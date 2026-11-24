//! A grid of character cells: the one thing every screen draws into.

/// A colour as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    /// Red, green and blue as bytes.
    pub fn rgb(self) -> (u8, u8, u8) {
        (
            ((self.0 >> 16) & 0xFF) as u8,
            ((self.0 >> 8) & 0xFF) as u8,
            (self.0 & 0xFF) as u8,
        )
    }
}

/// The palette. Dark background, one accent colour, and colours that carry meaning (ok, warning,
/// bad) so a person can read state at a glance.
pub mod theme {
    use super::Color;
    /// Screen background.
    pub const BG: Color = Color(0x0B1020);
    /// A raised panel's background.
    pub const PANEL: Color = Color(0x121A2F);
    /// A selected row's background.
    pub const SELECT: Color = Color(0x1F3A6B);
    /// Normal text.
    pub const TEXT: Color = Color(0xE6EAF2);
    /// Secondary text.
    pub const DIM: Color = Color(0x8A94A8);
    /// Accent (titles, focus).
    pub const ACCENT: Color = Color(0x4F8CFF);
    /// Good / allowed / verified.
    pub const OK: Color = Color(0x3DDC97);
    /// Needs attention.
    pub const WARN: Color = Color(0xFFC857);
    /// Bad / denied / failed.
    pub const BAD: Color = Color(0xFF5C7A);
}

/// One character cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character.
    pub ch: char,
    /// Text colour.
    pub fg: Color,
    /// Background colour.
    pub bg: Color,
}

/// A rectangle of cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left column.
    pub x: usize,
    /// Top row.
    pub y: usize,
    /// Width in cells.
    pub w: usize,
    /// Height in cells.
    pub h: usize,
}

impl Rect {
    /// A rectangle.
    pub const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    /// The rectangle shrunk by `n` cells on every side (empty if too small).
    pub fn inset(self, n: usize) -> Self {
        Self {
            x: self.x + n,
            y: self.y + n,
            w: self.w.saturating_sub(2 * n),
            h: self.h.saturating_sub(2 * n),
        }
    }
}

/// A `cols` by `rows` grid of cells. Drawing outside it is silently clipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
}

impl Grid {
    /// A grid filled with spaces.
    pub fn new(cols: usize, rows: usize) -> Self {
        let blank = Cell {
            ch: ' ',
            fg: theme::TEXT,
            bg: theme::BG,
        };
        Self {
            cols,
            rows,
            cells: vec![blank; cols * rows],
        }
    }

    /// Width in cells.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Height in cells.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The whole grid as a rectangle.
    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.cols, self.rows)
    }

    /// The cell at (`col`, `row`), if inside the grid.
    pub fn get(&self, col: usize, row: usize) -> Option<Cell> {
        if col < self.cols && row < self.rows {
            self.cells.get(row * self.cols + col).copied()
        } else {
            None
        }
    }

    /// Sets one cell (ignored when outside the grid).
    pub fn put(&mut self, col: usize, row: usize, ch: char, fg: Color, bg: Color) {
        if col < self.cols && row < self.rows {
            if let Some(c) = self.cells.get_mut(row * self.cols + col) {
                *c = Cell { ch, fg, bg };
            }
        }
    }

    /// Fills a rectangle with spaces on `bg`.
    pub fn fill(&mut self, r: Rect, bg: Color) {
        for y in r.y..r.y.saturating_add(r.h) {
            for x in r.x..r.x.saturating_add(r.w) {
                self.put(x, y, ' ', theme::TEXT, bg);
            }
        }
    }

    /// Writes `s` starting at (`col`, `row`), clipped to the grid. Control characters become
    /// spaces. Returns how many cells were written.
    pub fn text(&mut self, col: usize, row: usize, s: &str, fg: Color, bg: Color) -> usize {
        let mut n = 0;
        for ch in s.chars() {
            let ch = if ch.is_control() { ' ' } else { ch };
            if col + n >= self.cols {
                break;
            }
            self.put(col + n, row, ch, fg, bg);
            n += 1;
        }
        n
    }

    /// Writes `s` into at most `width` cells; when it does not fit, the end is replaced by `..`.
    pub fn text_fit(
        &mut self,
        col: usize,
        row: usize,
        width: usize,
        s: &str,
        fg: Color,
        bg: Color,
    ) {
        let shown = fit(s, width);
        let n = self.text(col, row, &shown, fg, bg);
        for i in n..width {
            self.put(col + i, row, ' ', fg, bg);
        }
    }

    /// Draws a single-line box. When `title` is given it sits in the top edge.
    pub fn boxed(&mut self, r: Rect, title: Option<&str>, edge: Color, bg: Color) {
        if r.w < 2 || r.h < 2 {
            return;
        }
        let (x2, y2) = (r.x + r.w - 1, r.y + r.h - 1);
        for x in r.x + 1..x2 {
            self.put(x, r.y, '\u{2500}', edge, bg);
            self.put(x, y2, '\u{2500}', edge, bg);
        }
        for y in r.y + 1..y2 {
            self.put(r.x, y, '\u{2502}', edge, bg);
            self.put(x2, y, '\u{2502}', edge, bg);
        }
        self.put(r.x, r.y, '\u{250C}', edge, bg);
        self.put(x2, r.y, '\u{2510}', edge, bg);
        self.put(r.x, y2, '\u{2514}', edge, bg);
        self.put(x2, y2, '\u{2518}', edge, bg);
        if let Some(t) = title {
            // Corner, one line segment, a space, the title, a space, then the line again.
            let room = r.w.saturating_sub(6);
            if room > 0 {
                let label = fit(t, room);
                self.put(r.x + 2, r.y, ' ', edge, bg);
                let n = self.text(r.x + 3, r.y, &label, theme::TEXT, bg);
                self.put(r.x + 3 + n, r.y, ' ', edge, bg);
            }
        }
    }

    /// A horizontal bar of `width` cells starting at `at` (column, row), filled in proportion to
    /// `part` of `whole`.
    pub fn bar(
        &mut self,
        at: (usize, usize),
        width: usize,
        part: u64,
        whole: u64,
        fg: Color,
        bg: Color,
    ) {
        let (col, row) = at;
        let filled = if whole == 0 {
            0
        } else {
            ((u128::from(part.min(whole)) * width as u128) / u128::from(whole)) as usize
        };
        for i in 0..width {
            if i < filled {
                self.put(col + i, row, '\u{2588}', fg, bg);
            } else {
                self.put(col + i, row, '\u{2591}', theme::DIM, bg);
            }
        }
    }

    /// The grid as plain text, one line per row with trailing spaces removed (for tests).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for row in 0..self.rows {
            let line: String = (0..self.cols)
                .filter_map(|c| self.get(c, row))
                .map(|c| c.ch)
                .collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// The text of one row with trailing spaces removed.
    pub fn row_text(&self, row: usize) -> String {
        (0..self.cols)
            .filter_map(|c| self.get(c, row))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }
}

/// Shortens `s` to at most `width` characters, ending in `..` when it was cut.
pub fn fit(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count <= width {
        return s
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
    }
    if width <= 2 {
        return ".".repeat(width);
    }
    let mut out: String = s
        .chars()
        .take(width - 2)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    out.push_str("..");
    out
}

/// Breaks `text` into lines of at most `width` characters, at spaces where possible. Explicit
/// newlines are kept; a word longer than a line is split.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut len = 0;
        for word in para.split(' ') {
            let mut w: Vec<char> = word.chars().collect();
            // Split an over-long word across lines.
            while w.len() > width {
                if len > 0 {
                    lines.push(std::mem::take(&mut line));
                    len = 0;
                }
                let head: String = w.drain(..width).collect();
                lines.push(head);
            }
            let wl = w.len();
            if len > 0 && len + 1 + wl > width {
                lines.push(std::mem::take(&mut line));
                len = 0;
            }
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.extend(w);
            len += wl;
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_outside_the_grid_is_clipped_not_a_panic() {
        let mut g = Grid::new(5, 2);
        assert_eq!(g.text(3, 0, "abcdef", theme::TEXT, theme::BG), 2);
        g.put(99, 99, 'x', theme::TEXT, theme::BG);
        g.fill(Rect::new(4, 1, 50, 50), theme::PANEL);
        assert_eq!(g.get(5, 0), None);
        assert_eq!(g.get(4, 1).map(|c| c.bg), Some(theme::PANEL));
        assert_eq!(g.to_text(), "   ab\n\n");
    }

    #[test]
    fn control_characters_never_reach_the_screen() {
        let mut g = Grid::new(6, 1);
        g.text(0, 0, "a\tb\nc\u{1b}", theme::TEXT, theme::BG);
        assert_eq!(g.row_text(0), "a b c");
    }

    #[test]
    fn fit_shortens_with_dots() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello world", 8), "hello ..");
        assert_eq!(fit("hello", 2), "..");
        assert_eq!(fit("hello", 0), "");
        assert_eq!(fit("ab\ncd", 5), "ab cd");
    }

    #[test]
    fn text_fit_pads_so_old_text_is_erased() {
        let mut g = Grid::new(10, 1);
        g.text(0, 0, "XXXXXXXXXX", theme::TEXT, theme::BG);
        g.text_fit(1, 0, 6, "hi", theme::TEXT, theme::BG);
        assert_eq!(g.row_text(0), "Xhi    XXX");
        assert_eq!(g.get(7, 0).map(|c| c.ch), Some('X'));
    }

    #[test]
    fn boxes_have_corners_and_a_title() {
        let mut g = Grid::new(12, 4);
        g.boxed(
            Rect::new(0, 0, 12, 4),
            Some("Chat"),
            theme::ACCENT,
            theme::BG,
        );
        assert_eq!(g.to_text(), "\u{250c}\u{2500} Chat \u{2500}\u{2500}\u{2500}\u{2510}\n\u{2502}          \u{2502}\n\u{2502}          \u{2502}\n\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}\n");
        // Degenerate boxes are ignored, and a long title is shortened.
        g.boxed(Rect::new(0, 0, 1, 1), Some("x"), theme::ACCENT, theme::BG);
        let mut small = Grid::new(14, 3);
        small.boxed(
            Rect::new(0, 0, 14, 3),
            Some("A very long title"),
            theme::ACCENT,
            theme::BG,
        );
        assert_eq!(
            small.row_text(0),
            "\u{250c}\u{2500} A very.. \u{2500}\u{2510}"
        );
        // With almost no room the title shrinks to dots instead of overflowing the box.
        let mut tiny = Grid::new(8, 3);
        tiny.boxed(
            Rect::new(0, 0, 8, 3),
            Some("A very long title"),
            theme::ACCENT,
            theme::BG,
        );
        assert_eq!(tiny.row_text(0), "\u{250c}\u{2500} .. \u{2500}\u{2510}");
    }

    #[test]
    fn bars_fill_in_proportion() {
        let mut g = Grid::new(10, 1);
        g.bar((0, 0), 10, 3, 10, theme::OK, theme::BG);
        let full = (0..10)
            .filter(|c| g.get(*c, 0).map(|x| x.ch) == Some('\u{2588}'))
            .count();
        assert_eq!(full, 3);
        g.bar((0, 0), 10, 99, 10, theme::OK, theme::BG);
        assert_eq!(
            (0..10)
                .filter(|c| g.get(*c, 0).map(|x| x.ch) == Some('\u{2588}'))
                .count(),
            10
        );
        g.bar((0, 0), 10, 5, 0, theme::OK, theme::BG);
        assert_eq!(
            (0..10)
                .filter(|c| g.get(*c, 0).map(|x| x.ch) == Some('\u{2588}'))
                .count(),
            0
        );
    }

    #[test]
    fn wrapping_respects_width_newlines_and_long_words() {
        assert_eq!(
            wrap("the quick brown fox", 9),
            vec!["the quick", "brown fox"]
        );
        assert_eq!(wrap("a\n\nb", 5), vec!["a", "", "b"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("hi abcdefghij", 4), vec!["hi", "abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 5), vec![""]);
        for line in wrap("lorem ipsum dolor sit amet consectetur adipiscing elit", 11) {
            assert!(line.chars().count() <= 11, "{line:?}");
        }
        assert_eq!(wrap("x", 0), vec!["x"]);
    }
}
