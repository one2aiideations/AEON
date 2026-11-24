//! Points and rectangles in whole pixels. Negative values are allowed so windows can be dragged
//! partly off screen.

/// A rectangle: top-left corner and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

impl Rect {
    /// A rectangle.
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Whether the point is inside (the right and bottom edges are outside).
    pub const fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    /// Shrinks every side by `n`.
    pub const fn inset(&self, n: i32) -> Self {
        Self::new(self.x + n, self.y + n, self.w - 2 * n, self.h - 2 * n)
    }

    /// The part shared with `o` (zero-sized when they do not overlap).
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }

    /// The right edge.
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    /// The bottom edge.
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_excludes_the_far_edges() {
        let r = Rect::new(10, 10, 5, 5);
        assert!(r.contains(10, 10) && r.contains(14, 14));
        assert!(!r.contains(15, 10) && !r.contains(10, 15) && !r.contains(9, 10));
    }

    #[test]
    fn intersect_of_disjoint_rectangles_is_empty() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.intersect(&Rect::new(5, 5, 10, 10)), Rect::new(5, 5, 5, 5));
        let none = a.intersect(&Rect::new(20, 20, 4, 4));
        assert_eq!((none.w, none.h), (0, 0));
    }
}
