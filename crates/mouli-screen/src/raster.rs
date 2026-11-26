//! Turning a [`Grid`] into pixels, and pixels into a PNG file.

use crate::font::{glyph, GLYPH};
use crate::grid::Grid;
use crate::typeface::{blend, draw_glyph, mono, CELL_H, CELL_W};

/// How big to draw each cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterCfg {
    /// Each font pixel becomes `scale` by `scale` screen pixels.
    pub scale: usize,
    /// Extra pixels under each cell row, so lines of text do not touch.
    pub line_gap: usize,
}

impl Default for RasterCfg {
    fn default() -> Self {
        Self {
            scale: 2,
            line_gap: 4,
        }
    }
}

impl RasterCfg {
    /// Width of one cell in pixels.
    pub const fn cell_w(&self) -> usize {
        GLYPH * self.scale
    }

    /// Height of one cell in pixels.
    pub const fn cell_h(&self) -> usize {
        GLYPH * self.scale + self.line_gap
    }

    /// How many columns and rows fit a screen of `width` by `height` pixels.
    pub const fn grid_for(&self, width: usize, height: usize) -> (usize, usize) {
        (width / self.cell_w(), height / self.cell_h())
    }
}

/// A picture: `width` by `height` pixels of `0x00RRGGBB`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Pixels, row by row.
    pub pixels: Vec<u32>,
}

impl Canvas {
    /// The pixel at (`x`, `y`), if inside.
    pub fn get(&self, x: usize, y: usize) -> Option<u32> {
        if x < self.width && y < self.height {
            self.pixels.get(y * self.width + x).copied()
        } else {
            None
        }
    }

    /// A high-contrast copy: the interface colours become strong ones (black ground, white
    /// text, saturated accents, a vivid selection bar) and every other pixel, such as the
    /// edge of a letter or a picture, has its channels stretched so darks go black and lights
    /// go bright.
    #[must_use]
    pub fn high_contrast(&self) -> Canvas {
        use crate::grid::theme;
        let table = [
            (theme::BG.0, 0x000000),
            (theme::PANEL.0, 0x000000),
            (theme::SELECT.0, 0x0047AB),
            (theme::TEXT.0, 0xFFFFFF),
            (theme::DIM.0, 0xE0E0E0),
            (theme::ACCENT.0, 0x66E0FF),
            (theme::OK.0, 0x33FF66),
            (theme::WARN.0, 0xFFE600),
            (theme::BAD.0, 0xFF6666),
        ];
        let stretch = |v: u32| -> u32 {
            match v {
                0..=0x3F => 0,
                0xB0..=0xFF => 0xFF,
                v => (v - 0x40) * 0xFF / (0xB0 - 0x40),
            }
        };
        let pixels = self
            .pixels
            .iter()
            .map(|&p| {
                let p = p & 0x00FF_FFFF;
                if let Some((_, to)) = table.iter().find(|(from, _)| *from == p) {
                    return *to;
                }
                (stretch(p >> 16 & 0xFF) << 16) | (stretch(p >> 8 & 0xFF) << 8) | stretch(p & 0xFF)
            })
            .collect();
        Canvas {
            width: self.width,
            height: self.height,
            pixels,
        }
    }

    /// Every pixel repeated `k` times across and down (larger text for small or far screens).
    /// `k` of 0 or 1 gives an unchanged copy.
    #[must_use]
    pub fn scaled(&self, k: usize) -> Canvas {
        let k = k.max(1);
        if k == 1 {
            return self.clone();
        }
        let (w, h) = (self.width * k, self.height * k);
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = &self.pixels[(y / k) * self.width..(y / k + 1) * self.width];
            for x in 0..w {
                pixels.push(row[x / k]);
            }
        }
        Canvas {
            width: w,
            height: h,
            pixels,
        }
    }

    /// The picture as 32-bit little-endian `XRGB8888` bytes (blue, green, red, unused), the
    /// layout of most firmware framebuffers.
    pub fn to_xrgb8888(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for p in &self.pixels {
            out.extend_from_slice(&[
                (*p & 0xFF) as u8,
                ((*p >> 8) & 0xFF) as u8,
                ((*p >> 16) & 0xFF) as u8,
                0,
            ]);
        }
        out
    }

    /// The picture as 16-bit little-endian `RGB565` bytes.
    pub fn to_rgb565(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 2);
        for p in &self.pixels {
            let (r, g, b) = (
                ((*p >> 16) & 0xFF) as u16,
                ((*p >> 8) & 0xFF) as u16,
                (*p & 0xFF) as u16,
            );
            let v = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3);
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// True for box-drawing and block characters, which should join the cell below.
fn connects_down(ch: char) -> bool {
    matches!(ch as u32, 0x2500..=0x259F)
}

/// Draws `grid` into a new canvas just big enough for it.
pub fn render(grid: &Grid, cfg: RasterCfg) -> Canvas {
    let (cw, ch) = (cfg.cell_w(), cfg.cell_h());
    let (width, height) = (grid.cols() * cw, grid.rows() * ch);
    let mut pixels = vec![0u32; width * height];
    for row in 0..grid.rows() {
        for col in 0..grid.cols() {
            let Some(cell) = grid.get(col, row) else {
                continue;
            };
            let bits = glyph(cell.ch);
            let (x0, y0) = (col * cw, row * ch);
            // Glyphs sit vertically centred in the cell: half of the line gap above, half below.
            let pad_top = cfg.line_gap / 2;
            for py in 0..ch {
                let joins = connects_down(cell.ch);
                let bits_row = match py.checked_sub(pad_top) {
                    // Above the glyph: box-drawing and block cells repeat their top row so lines
                    // and bars run unbroken into the row above; ordinary text stays blank.
                    None if joins => bits.first().copied().unwrap_or(0),
                    None => 0,
                    Some(gy) if gy < GLYPH * cfg.scale => {
                        bits.get(gy / cfg.scale).copied().unwrap_or(0)
                    }
                    // Below the glyph: the same, with the bottom row.
                    Some(_) if joins => bits.get(GLYPH - 1).copied().unwrap_or(0),
                    Some(_) => 0,
                };
                for px in 0..cw {
                    let lit = (bits_row >> (px / cfg.scale)) & 1 == 1;
                    if let Some(p) = pixels.get_mut((y0 + py) * width + x0 + px) {
                        *p = if lit { cell.fg.0 } else { cell.bg.0 };
                    }
                }
            }
        }
    }
    Canvas {
        width,
        height,
        pixels,
    }
}

/// Fills part of a cell, clipped to the canvas.
fn fill_px(c: &mut Canvas, x: usize, y: usize, w: usize, h: usize, colour: u32) {
    for yy in y..(y + h).min(c.height) {
        for xx in x..(x + w).min(c.width) {
            c.pixels[yy * c.width + xx] = colour;
        }
    }
}

/// Line strengths (left, right, up, down) of the box-drawing characters the screens use:
/// 0 none, 1 thin. Double and heavy lines are drawn two pixels thick.
fn box_arms(ch: char) -> Option<[u8; 4]> {
    Some(match ch {
        '\u{2500}' => [1, 1, 0, 0],
        '\u{2502}' => [0, 0, 1, 1],
        '\u{250C}' => [0, 1, 0, 1],
        '\u{2510}' => [1, 0, 0, 1],
        '\u{2514}' => [0, 1, 1, 0],
        '\u{2518}' => [1, 0, 1, 0],
        '\u{251C}' => [0, 1, 1, 1],
        '\u{2524}' => [1, 0, 1, 1],
        '\u{252C}' => [1, 1, 0, 1],
        '\u{2534}' => [1, 1, 1, 0],
        '\u{253C}' => [1, 1, 1, 1],
        '\u{2550}' | '\u{2501}' => [2, 2, 0, 0],
        '\u{2551}' | '\u{2503}' => [0, 0, 2, 2],
        '\u{2554}' | '\u{250F}' => [0, 2, 0, 2],
        '\u{2557}' | '\u{2513}' => [2, 0, 0, 2],
        '\u{255A}' | '\u{2517}' => [0, 2, 2, 0],
        '\u{255D}' | '\u{251B}' => [2, 0, 2, 0],
        _ => return None,
    })
}

/// Draws box-drawing and block characters exactly, so lines join between cells and bars are
/// solid. Returns false for characters it does not handle.
fn paint_special(c: &mut Canvas, x0: usize, y0: usize, cell: crate::grid::Cell) -> bool {
    let (cw, ch) = (CELL_W, CELL_H);
    let fg = cell.fg.0;
    if let Some(arms) = box_arms(cell.ch) {
        let thick = |w: u8| if w >= 2 { 2 } else { 1 };
        let [l, r, u, d] = arms;
        let (cx, cy) = (cw / 2, ch / 2);
        let (tv, th) = (thick(u.max(d)), thick(l.max(r)));
        let (xs, ys) = (cx - tv / 2, cy - th / 2);
        if l > 0 {
            fill_px(c, x0, y0 + cy - thick(l) / 2, xs + tv, thick(l), fg);
        }
        if r > 0 {
            fill_px(c, x0 + xs, y0 + cy - thick(r) / 2, cw - xs, thick(r), fg);
        }
        if u > 0 {
            fill_px(c, x0 + cx - thick(u) / 2, y0, thick(u), ys + th, fg);
        }
        if d > 0 {
            fill_px(c, x0 + cx - thick(d) / 2, y0 + ys, thick(d), ch - ys, fg);
        }
        return true;
    }
    let cp = cell.ch as u32;
    match cp {
        0x2580 => fill_px(c, x0, y0, cw, ch / 2, fg),
        0x2581..=0x2587 => {
            let h = ch * (cp - 0x2580) as usize / 8;
            fill_px(c, x0, y0 + ch - h, cw, h, fg);
        }
        0x2588 => fill_px(c, x0, y0, cw, ch, fg),
        0x2589..=0x258F => fill_px(c, x0, y0, cw * (0x2590 - cp) as usize / 8, ch, fg),
        0x2590 => fill_px(c, x0 + cw / 2, y0, cw - cw / 2, ch, fg),
        0x2591..=0x2593 => {
            let alpha = [64u8, 128, 192][(cp - 0x2591) as usize];
            fill_px(c, x0, y0, cw, ch, blend(cell.bg.0, fg, alpha));
        }
        _ => return false,
    }
    true
}

/// Draws `grid` with Roboto Mono: smooth text in cells of [`CELL_W`] by [`CELL_H`] pixels. Box
/// lines and blocks are drawn exactly, so frames and bars have no gaps.
pub fn render_text(grid: &Grid) -> Canvas {
    let (width, height) = (grid.cols() * CELL_W, grid.rows() * CELL_H);
    let mut canvas = Canvas {
        width,
        height,
        pixels: vec![0; width * height],
    };
    let face = mono();
    for row in 0..grid.rows() {
        for col in 0..grid.cols() {
            let Some(cell) = grid.get(col, row) else {
                continue;
            };
            let (x0, y0) = (col * CELL_W, row * CELL_H);
            fill_px(&mut canvas, x0, y0, CELL_W, CELL_H, cell.bg.0);
            if cell.ch == ' ' || paint_special(&mut canvas, x0, y0, cell) {
                continue;
            }
            let ch = if cell.ch.is_control() { ' ' } else { cell.ch };
            let pen = x0 as i32 + (CELL_W as i32 - face.glyph(ch).advance) / 2;
            draw_glyph(
                &mut canvas,
                face,
                ch,
                pen,
                y0 as i32 + face.ascent,
                cell.fg.0,
            );
        }
    }
    canvas
}

/// CRC-32 (IEEE 802.3) as used by PNG.
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 == 1 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc = table[((crc ^ u32::from(*b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Adler-32 as used by zlib.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for byte in chunk {
            a += u32::from(*byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// Encodes the canvas as an RGB PNG. The image data is stored uncompressed (valid zlib "stored"
/// blocks), which keeps this dependency-free and exact.
pub fn to_png(canvas: &Canvas) -> Vec<u8> {
    let mut raw = Vec::with_capacity(canvas.height * (canvas.width * 3 + 1));
    for y in 0..canvas.height {
        raw.push(0); // filter: none
        for x in 0..canvas.width {
            let p = canvas
                .pixels
                .get(y * canvas.width + x)
                .copied()
                .unwrap_or(0);
            raw.extend_from_slice(&[
                ((p >> 16) & 0xFF) as u8,
                ((p >> 8) & 0xFF) as u8,
                (p & 0xFF) as u8,
            ]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        z.push(u8::from(last));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(canvas.width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(canvas.height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{theme, Color};

    #[test]
    fn high_contrast_maps_interface_colours_and_stretches_the_rest() {
        let c = Canvas {
            width: 5,
            height: 1,
            pixels: vec![
                theme::BG.0,
                theme::TEXT.0,
                theme::SELECT.0,
                0x808080,
                0x20_30_F0,
            ],
        };
        let h = c.high_contrast();
        assert_eq!(h.pixels[0], 0x000000);
        assert_eq!(h.pixels[1], 0xFFFFFF);
        assert_eq!(h.pixels[2], 0x0047AB);
        // Mid grey is pushed apart from black and white but stays in between.
        let mid = h.pixels[3] & 0xFF;
        assert!(mid > 0 && mid < 0xFF);
        assert_eq!(h.pixels[3], (mid << 16) | (mid << 8) | mid);
        assert_eq!(h.pixels[4], 0x0000FF);
        // Text on the background is now far apart in brightness.
        assert!(h.pixels[1] - h.pixels[0] > c.pixels[1] - c.pixels[0]);
    }

    #[test]
    fn scaling_repeats_pixels_and_one_is_a_copy() {
        let c = Canvas {
            width: 2,
            height: 2,
            pixels: vec![1, 2, 3, 4],
        };
        assert_eq!(c.scaled(1), c);
        assert_eq!(c.scaled(0), c);
        let s = c.scaled(2);
        assert_eq!((s.width, s.height), (4, 4));
        assert_eq!(s.pixels[..4], [1, 1, 2, 2]);
        assert_eq!(s.pixels[4..8], [1, 1, 2, 2]);
        assert_eq!(s.pixels[8..12], [3, 3, 4, 4]);
    }

    #[test]
    fn checksum_known_vectors() {
        // Catalogue check values for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
        // A long run exercises the modulo batching.
        assert_eq!(adler32(&vec![0xFFu8; 100_000]), {
            let (mut a, mut b) = (1u64, 0u64);
            for _ in 0..100_000 {
                a = (a + 255) % 65_521;
                b = (b + a) % 65_521;
            }
            ((b << 16) | a) as u32
        });
    }

    #[test]
    fn a_cell_becomes_a_block_of_pixels() {
        let mut g = Grid::new(2, 1);
        g.put(0, 0, '\u{2588}', Color(0xFF0000), Color(0x00FF00)); // solid red on green
        g.put(1, 0, ' ', Color(0xFF0000), Color(0x0000FF)); // blank on blue
        let cfg = RasterCfg {
            scale: 2,
            line_gap: 4,
        };
        let c = render(&g, cfg);
        assert_eq!((c.width, c.height), (32, 20));
        assert_eq!(c.get(0, 0), Some(0xFF0000));
        assert_eq!(c.get(15, 15), Some(0xFF0000), "last pixel of the glyph");
        assert_eq!(
            c.get(0, 16),
            Some(0xFF0000),
            "a block cell joins the cell below"
        );
        assert_eq!(
            c.get(16, 16),
            Some(0x0000FF),
            "an ordinary cell's line gap shows its background"
        );
        assert_eq!(c.get(16, 0), Some(0x0000FF));
        assert_eq!(c.get(32, 0), None);
    }

    #[test]
    fn scaling_and_glyph_orientation_are_right() {
        // The glyph 'A' has its first lit pixel in row 0 at column 2 (bit 2 of 0x3C).
        let mut g = Grid::new(1, 1);
        g.put(0, 0, 'A', Color(0xFFFFFF), Color(0));
        let c = render(
            &g,
            RasterCfg {
                scale: 1,
                line_gap: 0,
            },
        );
        let row0: String = (0..8)
            .map(|x| {
                if c.get(x, 0) == Some(0xFFFFFF) {
                    'X'
                } else {
                    '.'
                }
            })
            .collect();
        assert_eq!(row0, "..XX....");
        let c3 = render(
            &g,
            RasterCfg {
                scale: 3,
                line_gap: 0,
            },
        );
        assert_eq!((c3.width, c3.height), (24, 24));
        assert_eq!(c3.get(6, 0), Some(0xFFFFFF));
        assert_eq!(c3.get(5, 2), Some(0));
    }

    #[test]
    fn vertical_lines_and_bars_run_unbroken_through_the_row_gap() {
        let mut g = Grid::new(1, 2);
        g.put(0, 0, '\u{2502}', Color(0xFFFFFF), Color(0));
        g.put(0, 1, 'x', Color(0xFFFFFF), Color(0));
        let cfg = RasterCfg {
            scale: 2,
            line_gap: 4,
        };
        let c = render(&g, cfg);
        // The vertical stroke of U+2502 is at font column 3, so screen x = 6 or 7 when scaled.
        for y in 0..cfg.cell_h() {
            assert_eq!(c.get(6, y), Some(0xFFFFFF), "stroke missing at y={y}");
        }
        // An ordinary character leaves its gap blank.
        let mut t = Grid::new(1, 1);
        t.put(0, 0, '|', Color(0xFFFFFF), Color(0));
        let c = render(&t, cfg);
        assert!((16..20).all(|y| (0..16).all(|x| c.get(x, y) == Some(0))));
        // A full block fills its gap too, so bars are solid.
        let mut b = Grid::new(1, 1);
        b.put(0, 0, '\u{2588}', Color(0xFF00FF), Color(0));
        let c = render(&b, cfg);
        assert!((0..20).all(|y| (0..16).all(|x| c.get(x, y) == Some(0xFF00FF))));
    }

    #[test]
    fn ordinary_glyphs_are_centred_vertically_in_the_cell() {
        // 'A' row 0 is lit at font columns 2 and 3. With scale 2 and a 4-pixel gap, the glyph
        // starts 2 pixels down (half the gap above, half below).
        let mut g = Grid::new(1, 1);
        g.put(0, 0, 'A', Color(0xFFFFFF), Color(0));
        let c = render(
            &g,
            RasterCfg {
                scale: 2,
                line_gap: 4,
            },
        );
        assert_eq!(c.get(4, 1), Some(0), "blank above the glyph");
        assert_eq!(c.get(4, 2), Some(0xFFFFFF), "first glyph row");
        assert_eq!(c.get(4, 17), Some(0), "row 7 of 'A' is blank");
        assert!(
            (18..20).all(|y| c.get(4, y) == Some(0)),
            "blank below the glyph"
        );
    }

    #[test]
    fn grid_size_for_a_screen() {
        let cfg = RasterCfg::default();
        assert_eq!((cfg.cell_w(), cfg.cell_h()), (16, 20));
        assert_eq!(cfg.grid_for(1280, 800), (80, 40));
        assert_eq!(cfg.grid_for(640, 480), (40, 24));
        assert_eq!(cfg.grid_for(10, 10), (0, 0));
    }

    #[test]
    fn framebuffer_byte_layouts() {
        let c = Canvas {
            width: 2,
            height: 1,
            pixels: vec![0x112233, 0xFFFFFF],
        };
        assert_eq!(
            c.to_xrgb8888(),
            vec![0x33, 0x22, 0x11, 0, 0xFF, 0xFF, 0xFF, 0]
        );
        // 0x112233: r=0x11>>3=2, g=0x22>>2=8, b=0x33>>3=6 -> 2<<11 | 8<<5 | 6
        assert_eq!(c.to_rgb565(), vec![0x06, 0x11, 0xFF, 0xFF]);
    }

    /// Splits PNG bytes into (kind, data) chunks, checking every CRC.
    fn chunks(png: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let mut i = 8;
        let mut out = Vec::new();
        while i < png.len() {
            let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
            let kind = &png[i + 4..i + 8];
            let data = &png[i + 8..i + 8 + len];
            let crc = u32::from_be_bytes(png[i + 8 + len..i + 12 + len].try_into().unwrap());
            assert_eq!(
                crc,
                crc32(&png[i + 4..i + 8 + len]),
                "CRC of {}",
                String::from_utf8_lossy(kind)
            );
            out.push((String::from_utf8_lossy(kind).into_owned(), data.to_vec()));
            i += 12 + len;
        }
        out
    }

    #[test]
    fn png_structure_and_pixels_round_trip() {
        let mut g = Grid::new(6, 2);
        g.text(0, 0, "MOULI", theme::ACCENT, theme::BG);
        let canvas = render(
            &g,
            RasterCfg {
                scale: 1,
                line_gap: 0,
            },
        );
        let png = to_png(&canvas);
        let ch = chunks(&png);
        assert_eq!(
            ch.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            vec!["IHDR", "IDAT", "IEND"]
        );
        let ihdr = &ch[0].1;
        assert_eq!(
            u32::from_be_bytes(ihdr[..4].try_into().unwrap()) as usize,
            canvas.width
        );
        assert_eq!(
            u32::from_be_bytes(ihdr[4..8].try_into().unwrap()) as usize,
            canvas.height
        );
        // Undo the stored-deflate framing and compare the raw scanlines with the canvas.
        let z = &ch[1].1;
        assert_eq!(&z[..2], &[0x78, 0x01]);
        let mut raw = Vec::new();
        let mut i = 2;
        loop {
            let last = z[i] == 1;
            let len = u16::from_le_bytes([z[i + 1], z[i + 2]]) as usize;
            assert_eq!(u16::from_le_bytes([z[i + 3], z[i + 4]]), !(len as u16));
            raw.extend_from_slice(&z[i + 5..i + 5 + len]);
            i += 5 + len;
            if last {
                break;
            }
        }
        assert_eq!(
            u32::from_be_bytes(z[i..i + 4].try_into().unwrap()),
            adler32(&raw)
        );
        assert_eq!(raw.len(), canvas.height * (canvas.width * 3 + 1));
        for y in 0..canvas.height {
            let line = &raw[y * (canvas.width * 3 + 1)..][..canvas.width * 3 + 1];
            assert_eq!(line[0], 0);
            for x in 0..canvas.width {
                let want = canvas.get(x, y).unwrap();
                let got = (u32::from(line[1 + x * 3]) << 16)
                    | (u32::from(line[2 + x * 3]) << 8)
                    | u32::from(line[3 + x * 3]);
                assert_eq!(got, want, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn large_images_span_several_stored_blocks() {
        let canvas = Canvas {
            width: 400,
            height: 300,
            pixels: vec![0x123456; 400 * 300],
        };
        let png = to_png(&canvas);
        let ch = chunks(&png);
        let z = &ch[1].1;
        let blocks = (400 * 3 + 1) * 300 / 65_535 + 1;
        assert!(z.len() > 400 * 300 * 3 && blocks >= 5);
        // The final block carries the "last" flag and only that one.
        let mut i = 2;
        let mut seen = 0;
        loop {
            let last = z[i] == 1;
            let len = u16::from_le_bytes([z[i + 1], z[i + 2]]) as usize;
            i += 5 + len;
            seen += 1;
            if last {
                break;
            }
            assert!(i < z.len());
        }
        assert_eq!(seen, blocks);
    }

    #[test]
    fn an_empty_canvas_is_still_a_valid_png() {
        let png = to_png(&Canvas {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        });
        assert_eq!(chunks(&png).len(), 3);
    }

    mod smooth {
        use super::*;
        use crate::grid::{theme, Grid};

        fn cell(ch: char) -> Canvas {
            let mut g = Grid::new(3, 3);
            g.fill(crate::grid::Rect::new(0, 0, 3, 3), theme::BG);
            g.put(1, 1, ch, theme::TEXT, theme::BG);
            render_text(&g)
        }

        fn px(c: &Canvas, col: usize, row: usize, x: usize, y: usize) -> u32 {
            c.pixels[(row * CELL_H + y) * c.width + col * CELL_W + x]
        }

        fn ink(c: &Canvas, col: usize, row: usize) -> usize {
            (0..CELL_H)
                .flat_map(|y| (0..CELL_W).map(move |x| (x, y)))
                .filter(|&(x, y)| px(c, col, row, x, y) != theme::BG.0)
                .count()
        }

        #[test]
        fn the_picture_is_sized_by_the_cell_and_blank_cells_are_background() {
            let c = cell('A');
            assert_eq!((c.width, c.height), (3 * CELL_W, 3 * CELL_H));
            assert_eq!(ink(&c, 0, 0), 0);
            assert_eq!(ink(&cell(' '), 1, 1), 0);
        }

        #[test]
        fn a_letter_is_drawn_smoothly_inside_its_own_cell() {
            let c = cell('g');
            assert!(ink(&c, 1, 1) > 10);
            for (col, row) in [(0, 1), (2, 1), (1, 0), (1, 2)] {
                assert_eq!(ink(&c, col, row), 0, "ink leaked into cell ({col},{row})");
            }
            let shades: std::collections::HashSet<u32> = (0..CELL_H)
                .flat_map(|y| (0..CELL_W).map(move |x| (x, y)))
                .map(|(x, y)| px(&c, 1, 1, x, y))
                .collect();
            assert!(shades.len() > 3, "anti-aliasing gives in-between colours");
        }

        #[test]
        fn vertical_and_horizontal_lines_join_across_cells() {
            let mut g = Grid::new(2, 2);
            g.fill(crate::grid::Rect::new(0, 0, 2, 2), theme::BG);
            g.put(0, 0, '\u{2502}', theme::TEXT, theme::BG);
            g.put(0, 1, '\u{2502}', theme::TEXT, theme::BG);
            g.put(1, 0, '\u{2500}', theme::TEXT, theme::BG);
            let c = render_text(&g);
            let col = CELL_W / 2;
            for y in 0..2 * CELL_H {
                assert_eq!(
                    c.pixels[y * c.width + col],
                    theme::TEXT.0,
                    "gap in the line at row {y}"
                );
            }
            let row = CELL_H / 2;
            for x in CELL_W..2 * CELL_W {
                assert_eq!(
                    c.pixels[row * c.width + x],
                    theme::TEXT.0,
                    "gap at column {x}"
                );
            }
        }

        #[test]
        fn corners_draw_only_their_two_arms() {
            let c = cell('\u{250C}'); // down and right
            let (cx, cy) = (CELL_W / 2, CELL_H / 2);
            assert_eq!(px(&c, 1, 1, cx, cy), theme::TEXT.0);
            assert_eq!(px(&c, 1, 1, CELL_W - 1, cy), theme::TEXT.0);
            assert_eq!(px(&c, 1, 1, cx, CELL_H - 1), theme::TEXT.0);
            assert_eq!(px(&c, 1, 1, 0, cy), theme::BG.0, "no arm to the left");
            assert_eq!(px(&c, 1, 1, cx, 0), theme::BG.0, "no arm upwards");
        }

        #[test]
        fn blocks_fill_exactly_and_shades_blend() {
            let full = cell('\u{2588}');
            assert_eq!(ink(&full, 1, 1), CELL_W * CELL_H);
            let half = cell('\u{2584}');
            assert_eq!(ink(&half, 1, 1), CELL_W * (CELL_H / 2));
            assert_eq!(px(&half, 1, 1, 0, CELL_H - 1), theme::TEXT.0);
            assert_eq!(px(&half, 1, 1, 0, 0), theme::BG.0);
            let shade = cell('\u{2591}');
            assert_eq!(
                px(&shade, 1, 1, 3, 3),
                blend(theme::BG.0, theme::TEXT.0, 64)
            );
        }

        #[test]
        fn unknown_and_control_characters_do_not_panic() {
            for ch in ['\u{1F600}', '\u{0}', '\n', '\u{2599}', '\u{256C}', 'é', '€'] {
                cell(ch);
            }
        }
    }
}
