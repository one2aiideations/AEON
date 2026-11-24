//! Drawing into a [`Canvas`]: rectangles, bevels, text and bitmaps. Everything is clipped to the
//! canvas, so callers can draw partly off screen.

use crate::geom::Rect;
use mouli_screen::font::glyph;
use mouli_screen::raster::Canvas;

/// A canvas filled with one colour.
pub fn canvas(width: usize, height: usize, colour: u32) -> Canvas {
    Canvas {
        width,
        height,
        pixels: vec![colour; width * height],
    }
}

/// Sets one pixel (ignored outside the canvas).
pub fn px(c: &mut Canvas, x: i32, y: i32, colour: u32) {
    if x >= 0 && y >= 0 && (x as usize) < c.width && (y as usize) < c.height {
        c.pixels[y as usize * c.width + x as usize] = colour;
    }
}

/// Fills a rectangle.
pub fn fill(c: &mut Canvas, r: Rect, colour: u32) {
    let r = r.intersect(&Rect::new(0, 0, c.width as i32, c.height as i32));
    for y in r.y..r.bottom() {
        let row = y as usize * c.width;
        for x in r.x..r.right() {
            c.pixels[row + x as usize] = colour;
        }
    }
}

/// A horizontal line of `len` pixels.
pub fn hline(c: &mut Canvas, x: i32, y: i32, len: i32, colour: u32) {
    fill(c, Rect::new(x, y, len, 1), colour);
}

/// A vertical line of `len` pixels.
pub fn vline(c: &mut Canvas, x: i32, y: i32, len: i32, colour: u32) {
    fill(c, Rect::new(x, y, 1, len), colour);
}

/// A one-pixel outline just inside `r`.
pub fn outline(c: &mut Canvas, r: Rect, colour: u32) {
    hline(c, r.x, r.y, r.w, colour);
    hline(c, r.x, r.bottom() - 1, r.w, colour);
    vline(c, r.x, r.y, r.h, colour);
    vline(c, r.right() - 1, r.y, r.h, colour);
}

/// A one-pixel bevel just inside `r`: `light` on the top and left, `dark` on the bottom and
/// right.
pub fn bevel(c: &mut Canvas, r: Rect, light: u32, dark: u32) {
    hline(c, r.x, r.y, r.w, light);
    vline(c, r.x, r.y, r.h, light);
    hline(c, r.x, r.bottom() - 1, r.w, dark);
    vline(c, r.right() - 1, r.y, r.h, dark);
}

/// Width in pixels of `s` drawn at `scale`.
pub fn text_width(s: &str, scale: i32) -> i32 {
    s.chars().count() as i32 * 8 * scale
}

/// Draws `s` with its top-left at (`x`, `y`) using the 8x8 font enlarged `scale` times. Returns
/// the width drawn.
pub fn text(c: &mut Canvas, x: i32, y: i32, s: &str, colour: u32, scale: i32) -> i32 {
    let mut cx = x;
    for ch in s.chars() {
        let bits = glyph(if ch.is_control() { ' ' } else { ch });
        for (gy, row) in bits.iter().enumerate() {
            for gx in 0..8 {
                if (row >> gx) & 1 == 1 {
                    fill(
                        c,
                        Rect::new(cx + gx * scale, y + gy as i32 * scale, scale, scale),
                        colour,
                    );
                }
            }
        }
        cx += 8 * scale;
    }
    cx - x
}

/// Copies `src` to (`x`, `y`), but only inside `clip`.
pub fn blit(c: &mut Canvas, src: &Canvas, x: i32, y: i32, clip: Rect) {
    let area = Rect::new(x, y, src.width as i32, src.height as i32)
        .intersect(&clip)
        .intersect(&Rect::new(0, 0, c.width as i32, c.height as i32));
    for dy in area.y..area.bottom() {
        let s0 = (dy - y) as usize * src.width + (area.x - x) as usize;
        let d0 = dy as usize * c.width + area.x as usize;
        let n = area.w as usize;
        c.pixels[d0..d0 + n].copy_from_slice(&src.pixels[s0..s0 + n]);
    }
}

/// Draws a small picture made of characters: each character is looked up in `palette`, and any
/// other character is left transparent. Every picture pixel becomes `scale` by `scale`.
pub fn sprite(c: &mut Canvas, x: i32, y: i32, rows: &[&str], palette: &[(char, u32)], scale: i32) {
    for (ry, row) in rows.iter().enumerate() {
        for (rx, ch) in row.chars().enumerate() {
            if let Some((_, colour)) = palette.iter().find(|(k, _)| *k == ch) {
                fill(
                    c,
                    Rect::new(x + rx as i32 * scale, y + ry as i32 * scale, scale, scale),
                    *colour,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_outside_the_canvas_is_ignored() {
        let mut c = canvas(4, 4, 0);
        px(&mut c, -1, 0, 9);
        px(&mut c, 4, 0, 9);
        fill(&mut c, Rect::new(-5, -5, 100, 100), 7);
        assert!(c.pixels.iter().all(|p| *p == 7));
        text(&mut c, 2, 2, "hello", 1, 3);
        blit(&mut c, &canvas(10, 10, 5), -3, -3, Rect::new(0, 0, 4, 4));
        assert!(c.pixels.iter().all(|p| *p == 5));
    }

    #[test]
    fn outline_and_bevel_touch_only_the_edges() {
        let mut c = canvas(6, 6, 0);
        outline(&mut c, Rect::new(0, 0, 6, 6), 1);
        assert_eq!(c.get(0, 3), Some(1));
        assert_eq!(c.get(3, 3), Some(0));
        bevel(&mut c, Rect::new(1, 1, 4, 4), 2, 3);
        assert_eq!((c.get(1, 1), c.get(4, 4)), (Some(2), Some(3)));
    }

    #[test]
    fn text_draws_a_known_glyph_and_reports_its_width() {
        let mut c = canvas(20, 10, 0);
        let w = text(&mut c, 0, 0, "I", 1, 1);
        assert_eq!(w, 8);
        assert!(c.pixels.contains(&1));
        assert_eq!(text_width("abc", 2), 48);
    }

    #[test]
    fn sprites_skip_transparent_characters() {
        let mut c = canvas(4, 4, 0);
        sprite(&mut c, 0, 0, &["a.", ".a"], &[('a', 9)], 2);
        assert_eq!(c.get(0, 0), Some(9));
        assert_eq!(c.get(2, 0), Some(0), "the transparent cell stays untouched");
        assert_eq!(c.get(2, 2), Some(9));
    }
}
