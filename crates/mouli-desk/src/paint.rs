//! Drawing into a [`Canvas`]: rectangles, text, shadows and bitmaps. Everything is clipped to the
//! canvas, so callers can draw partly off screen.

use crate::geom::Rect;
use mouli_screen::raster::Canvas;
use mouli_screen::typeface::{blend, draw_text, Face};

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

fn clipped(c: &Canvas, r: Rect) -> Rect {
    r.intersect(&Rect::new(0, 0, c.width as i32, c.height as i32))
}

/// Fills a rectangle.
pub fn fill(c: &mut Canvas, r: Rect, colour: u32) {
    let r = clipped(c, r);
    for y in r.y..r.bottom() {
        let row = y as usize * c.width;
        for x in r.x..r.right() {
            c.pixels[row + x as usize] = colour;
        }
    }
}

/// Fills a rectangle with `colour` at `alpha` (0 to 255) over what is already there.
pub fn fill_alpha(c: &mut Canvas, r: Rect, colour: u32, alpha: u8) {
    let r = clipped(c, r);
    for y in r.y..r.bottom() {
        let row = y as usize * c.width;
        for x in r.x..r.right() {
            let p = &mut c.pixels[row + x as usize];
            *p = blend(*p, colour, alpha);
        }
    }
}

/// A soft elevation shadow under `r`, as Material Design draws it: a few dark layers that grow
/// outward and fade, shifted down.
pub fn shadow(c: &mut Canvas, r: Rect) {
    for (grow, down, alpha) in [(10, 8, 14u8), (6, 5, 22), (3, 3, 30), (1, 1, 40)] {
        fill_alpha(
            c,
            Rect::new(
                r.x - grow,
                r.y - grow + down,
                r.w + 2 * grow,
                r.h + 2 * grow,
            ),
            0,
            alpha,
        );
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

/// A straight line between two points, one pixel wide.
pub fn line(c: &mut Canvas, from: (i32, i32), to: (i32, i32), colour: u32) {
    let (dx, dy) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
    let steps = dx.max(dy).max(1);
    for i in 0..=steps {
        let x = from.0 + (to.0 - from.0) * i / steps;
        let y = from.1 + (to.1 - from.1) * i / steps;
        px(c, x, y, colour);
    }
}

/// Width in pixels of `s` in `face`.
pub fn text_width(face: &Face, s: &str) -> i32 {
    face.width(s)
}

/// Draws `s` in `face` with the top of its line at `y`. Returns the width drawn.
pub fn text(c: &mut Canvas, face: &Face, x: i32, y: i32, s: &str, colour: u32) -> i32 {
    draw_text(c, face, x, y, s, colour)
}

/// Copies `src` to (`x`, `y`), but only inside `clip`.
pub fn blit(c: &mut Canvas, src: &Canvas, x: i32, y: i32, clip: Rect) {
    let area = clipped(
        c,
        Rect::new(x, y, src.width as i32, src.height as i32).intersect(&clip),
    );
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
    use mouli_screen::typeface::ui;

    #[test]
    fn drawing_outside_the_canvas_is_ignored() {
        let mut c = canvas(4, 4, 0);
        px(&mut c, -1, 0, 9);
        px(&mut c, 4, 0, 9);
        fill(&mut c, Rect::new(-5, -5, 100, 100), 7);
        assert!(c.pixels.iter().all(|p| *p == 7));
        text(&mut c, ui(), 2, 2, "hello", 1);
        fill_alpha(&mut c, Rect::new(-9, -9, 99, 99), 0, 255);
        shadow(&mut c, Rect::new(-20, -20, 5, 5));
        blit(&mut c, &canvas(10, 10, 5), -3, -3, Rect::new(0, 0, 4, 4));
        assert!(c.pixels.iter().all(|p| *p == 5));
        line(&mut c, (-50, -50), (50, 50), 3);
    }

    #[test]
    fn outline_touches_only_the_edges() {
        let mut c = canvas(6, 6, 0);
        outline(&mut c, Rect::new(0, 0, 6, 6), 1);
        assert_eq!(c.get(0, 3), Some(1));
        assert_eq!(c.get(3, 3), Some(0));
    }

    #[test]
    fn alpha_fill_mixes_with_what_is_underneath() {
        let mut c = canvas(2, 1, 0xFFFFFF);
        fill_alpha(&mut c, Rect::new(0, 0, 1, 1), 0, 128);
        assert_eq!(c.get(0, 0), Some(0x7F7F7F));
        assert_eq!(
            c.get(1, 0),
            Some(0xFFFFFF),
            "outside the rectangle is untouched"
        );
    }

    #[test]
    fn a_shadow_darkens_below_more_than_above_and_fades_outward() {
        let mut c = canvas(100, 100, 0xFFFFFF);
        shadow(&mut c, Rect::new(30, 30, 40, 40));
        let lum = |x: usize, y: usize| c.get(x, y).unwrap() & 0xFF;
        assert!(lum(50, 72) < lum(50, 28), "the shadow falls downward");
        assert!(lum(50, 71) < lum(50, 76), "and fades with distance");
        assert_eq!(lum(50, 95), 0xFF, "far away nothing is darkened");
    }

    #[test]
    fn text_is_measured_and_drawn_in_the_face_given() {
        let mut c = canvas(60, 24, 0);
        let w = text(&mut c, ui(), 0, 0, "Hi", 0xFFFFFF);
        assert_eq!(w, text_width(ui(), "Hi"));
        assert!(c.pixels.iter().any(|p| *p != 0));
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
