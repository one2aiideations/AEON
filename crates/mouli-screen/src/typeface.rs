//! Anti-aliased text from Google's Roboto and Roboto Mono (SIL Open Font License; see
//! `fonts/OFL.txt`). The fonts are embedded as pre-rendered glyph bitmaps made by
//! `fonts/gen_fonts.py`, so there is no font parser and no dependency here: a [`Face`] is a table
//! of glyphs with their metrics plus a block of 8-bit coverage values.

use crate::raster::Canvas;
use std::sync::OnceLock;

/// Width of a character cell drawn with the monospace face, in pixels.
pub const CELL_W: usize = 10;
/// Height of a character cell drawn with the monospace face, in pixels.
pub const CELL_H: usize = 22;

const MAGIC: &[u8; 4] = b"MFN1";
const HEADER: usize = 4 + 1 + 1 + 1 + 1 + 2;
const ENTRY: usize = 4 + 1 + 1 + 1 + 1 + 1 + 4;

/// One character's bitmap and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// How far the pen moves after drawing it.
    pub advance: i32,
    /// Offset of the bitmap's left edge from the pen.
    pub left: i32,
    /// Distance from the baseline up to the bitmap's top edge.
    pub top: i32,
    /// Bitmap width.
    pub w: i32,
    /// Bitmap height.
    pub h: i32,
    offset: usize,
}

/// A font at one size and weight.
#[derive(Debug)]
pub struct Face {
    /// Size in pixels.
    pub size: i32,
    /// Distance from the top of a line to its baseline.
    pub ascent: i32,
    /// Height of a line.
    pub line_height: i32,
    /// Advance of the digit zero.
    pub digit_width: i32,
    glyphs: Vec<(u32, Glyph)>,
    pixels: &'static [u8],
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

impl Face {
    /// Reads an atlas, checking every offset and size so damaged data is an error, not a panic.
    ///
    /// # Errors
    /// A short description of what is wrong with `bytes`.
    pub fn parse(bytes: &'static [u8]) -> Result<Self, &'static str> {
        if bytes.get(..4) != Some(MAGIC.as_slice()) {
            return Err("not a font atlas");
        }
        let (size, ascent, line, cell) = (
            i32::from(*bytes.get(4).ok_or("short header")?),
            i32::from(*bytes.get(5).ok_or("short header")? as i8),
            i32::from(*bytes.get(6).ok_or("short header")?),
            i32::from(*bytes.get(7).ok_or("short header")?),
        );
        let count = usize::from(le16(bytes, 8).ok_or("short header")?);
        let table_end = HEADER + count * ENTRY;
        let pixels = bytes.get(table_end..).ok_or("short glyph table")?;
        let mut glyphs: Vec<(u32, Glyph)> = Vec::with_capacity(count);
        for i in 0..count {
            let at = HEADER + i * ENTRY;
            let cp = le32(bytes, at).ok_or("short glyph table")?;
            let b = |k: usize| bytes.get(at + k).copied().ok_or("short glyph table");
            let (w, h) = (i32::from(b(7)?), i32::from(b(8)?));
            let offset = le32(bytes, at + 9).ok_or("short glyph table")? as usize;
            let need = (w * h) as usize;
            if offset
                .checked_add(need)
                .is_none_or(|end| end > pixels.len())
            {
                return Err("glyph bitmap outside the data");
            }
            if glyphs.last().is_some_and(|(prev, _)| *prev >= cp) {
                return Err("glyphs are not sorted");
            }
            glyphs.push((
                cp,
                Glyph {
                    advance: i32::from(b(4)?),
                    left: i32::from(b(5)? as i8),
                    top: i32::from(b(6)? as i8),
                    w,
                    h,
                    offset,
                },
            ));
        }
        Ok(Self {
            size,
            ascent,
            line_height: line,
            digit_width: cell,
            glyphs,
            pixels,
        })
    }

    /// The glyph for `ch`; characters the font does not have show as `?`.
    pub fn glyph(&self, ch: char) -> &Glyph {
        let find = |c: char| {
            self.glyphs
                .binary_search_by_key(&(c as u32), |(cp, _)| *cp)
                .ok()
                .map(|i| &self.glyphs[i].1)
        };
        find(ch).or_else(|| find('?')).unwrap_or(&self.glyphs[0].1)
    }

    /// Coverage values (0 to 255) of a glyph, row by row.
    pub fn coverage(&self, g: &Glyph) -> &[u8] {
        &self.pixels[g.offset..g.offset + (g.w * g.h) as usize]
    }

    /// Width in pixels of `s`.
    pub fn width(&self, s: &str) -> i32 {
        s.chars().map(|c| self.glyph(c).advance).sum()
    }
}

/// `fg` over `bg` with coverage `alpha` (0 to 255), per channel, rounded.
pub fn blend(bg: u32, fg: u32, alpha: u8) -> u32 {
    let a = u32::from(alpha);
    let mix = |shift: u32| {
        let (b, f) = ((bg >> shift) & 0xFF, (fg >> shift) & 0xFF);
        ((f * a + b * (255 - a) + 127) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// Draws one glyph with its pen at (`pen_x`, `baseline`), blending into the canvas.
pub fn draw_glyph(c: &mut Canvas, face: &Face, ch: char, pen_x: i32, baseline: i32, colour: u32) {
    let g = face.glyph(ch);
    let rows = face.coverage(g);
    for gy in 0..g.h {
        let y = baseline - g.top + gy;
        if y < 0 || y as usize >= c.height {
            continue;
        }
        for gx in 0..g.w {
            let x = pen_x + g.left + gx;
            if x < 0 || x as usize >= c.width {
                continue;
            }
            let a = rows[(gy * g.w + gx) as usize];
            if a != 0 {
                let p = &mut c.pixels[y as usize * c.width + x as usize];
                *p = blend(*p, colour, a);
            }
        }
    }
}

/// Draws `s` with the top of its line at `y`, starting at `x`. Returns the width drawn.
pub fn draw_text(c: &mut Canvas, face: &Face, x: i32, y: i32, s: &str, colour: u32) -> i32 {
    let mut pen = x;
    for ch in s.chars() {
        draw_glyph(c, face, ch, pen, y + face.ascent, colour);
        pen += face.glyph(ch).advance;
    }
    pen - x
}

macro_rules! face {
    ($name:ident, $file:literal) => {
        /// The embedded face (parsed once).
        pub fn $name() -> &'static Face {
            static CELL: OnceLock<Face> = OnceLock::new();
            CELL.get_or_init(|| {
                Face::parse(include_bytes!(concat!("../fonts/", $file)))
                    .expect("the embedded font atlas is valid")
            })
        }
    };
}

face!(ui_small, "ui_small.bin");
face!(ui, "ui.bin");
face!(ui_medium, "ui_medium.bin");
face!(mono, "mono.bin");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_four_embedded_atlases_load_and_cover_ascii() {
        for face in [ui_small(), ui(), ui_medium(), mono()] {
            for c in ' '..='~' {
                assert!(
                    face.glyph(c).advance > 0,
                    "{c:?} at {}px has no advance",
                    face.size
                );
            }
        }
    }

    #[test]
    fn the_monospace_cell_matches_the_constants_and_every_ascii_glyph_fits() {
        let m = mono();
        assert_eq!(m.line_height as usize, CELL_H);
        assert_eq!(m.digit_width as usize, CELL_W);
        for c in '!'..='~' {
            let g = m.glyph(c);
            assert!(
                g.advance == CELL_W as i32 || g.advance == CELL_W as i32 - 1,
                "{c:?} advance {}",
                g.advance
            );
            assert!(
                g.left >= -1 && g.left + g.w <= CELL_W as i32 + 1,
                "{c:?} sticks out of its cell"
            );
            assert!(
                g.top <= m.ascent && g.h - g.top <= m.line_height - m.ascent,
                "{c:?} leaves the line"
            );
        }
    }

    #[test]
    fn proportional_text_is_proportional() {
        let f = ui();
        assert!(f.glyph('W').advance > f.glyph('i').advance + 3);
        assert_eq!(f.width("ii"), 2 * f.glyph('i').advance);
        assert!(
            ui_medium().width("Models") >= ui().width("Models"),
            "medium is not narrower"
        );
        assert!(ui_small().size < ui().size);
    }

    #[test]
    fn missing_characters_fall_back_to_a_question_mark() {
        let f = mono();
        assert_eq!(f.glyph('\u{1F600}'), f.glyph('?'));
        assert_ne!(f.glyph('A'), f.glyph('?'));
        assert!(
            ui().glyph('\u{20AC}').w > 0,
            "the euro sign is in the atlas"
        );
    }

    #[test]
    fn damaged_atlases_are_errors_not_panics() {
        static GOOD: &[u8] = include_bytes!("../fonts/mono.bin");
        assert!(Face::parse(b"").is_err());
        assert!(Face::parse(b"NOPE000000000").is_err());
        for cut in [3, 8, 9, 12, 50, 1000, GOOD.len() / 2, GOOD.len() - 1] {
            // Truncated atlases either fail to parse or have a glyph pointing past the end.
            assert!(
                Face::parse(&GOOD[..cut]).is_err(),
                "cut at {cut} was accepted"
            );
        }
        assert!(Face::parse(GOOD).is_ok());
    }

    #[test]
    fn blend_is_exact_at_the_ends_and_in_the_middle() {
        assert_eq!(blend(0x102030, 0xF0E0D0, 0), 0x102030);
        assert_eq!(blend(0x102030, 0xF0E0D0, 255), 0xF0E0D0);
        assert_eq!(blend(0x000000, 0xFFFFFF, 128), 0x808080);
    }

    #[test]
    fn drawing_text_marks_pixels_inside_its_line_only_and_clips_at_the_edges() {
        let f = ui();
        let mut c = Canvas {
            width: 80,
            height: 30,
            pixels: vec![0; 80 * 30],
        };
        let w = draw_text(&mut c, f, 4, 5, "Hello", 0xFFFFFF);
        assert_eq!(w, f.width("Hello"));
        let rows_with_ink: Vec<usize> = (0..30)
            .filter(|y| (0..80).any(|x| c.pixels[y * 80 + x] != 0))
            .collect();
        assert!(rows_with_ink
            .iter()
            .all(|y| (5..5 + f.line_height as usize + 1).contains(y)));
        assert!(!rows_with_ink.is_empty());
        // Anti-aliasing: some pixels are neither background nor full colour.
        assert!(c.pixels.iter().any(|p| *p != 0 && *p != 0xFFFFFF));
        // Drawing far outside the canvas must not panic.
        draw_text(&mut c, f, -500, -500, "clipped", 0xFFFFFF);
        draw_text(&mut c, f, 500, 500, "clipped", 0xFFFFFF);
        draw_text(&mut c, f, 70, 25, "partly outside", 0xFFFFFF);
    }
}
