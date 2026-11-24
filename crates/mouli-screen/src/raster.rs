//! Turning a [`Grid`] into pixels, and pixels into a PNG file.

use crate::font::{glyph, GLYPH};
use crate::grid::Grid;

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
}
