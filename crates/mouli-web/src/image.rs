//! Picture decoders, written from scratch: PNG (all colour types, bit depths and interlacing),
//! baseline JPEG, and the first frame of a GIF. Output is a [`Bitmap`] of `0xAARRGGBB` pixels.
//!
//! Everything is bounded: pictures larger than [`MAX_PIXELS`] or [`MAX_SIDE`] are refused before
//! any memory is reserved for them, decompression stops at the size the header promised, and a
//! damaged file gives an error, never a panic.
//!
//! Not supported (an error that says so): progressive JPEG, arithmetic-coded JPEG, CMYK JPEG,
//! 16-bit-per-sample output precision (reduced to 8 bits), GIF animation (first frame only),
//! WebP and SVG.

/// Most pixels in one picture.
pub const MAX_PIXELS: usize = 8_000_000;
/// Longest side of a picture.
pub const MAX_SIDE: usize = 8192;

/// A decoded picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    /// Width in pixels.
    pub w: usize,
    /// Height in pixels.
    pub h: usize,
    /// Pixels row by row, `0xAARRGGBB`.
    pub px: Vec<u32>,
}

type Res<T> = Result<T, &'static str>;

fn rgb_table(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| 0xFF00_0000 | u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2]))
        .collect()
}

fn check_size(w: usize, h: usize) -> Res<()> {
    if w == 0 || h == 0 {
        return Err("the picture is empty");
    }
    if w > MAX_SIDE || h > MAX_SIDE || w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err("the picture is too large");
    }
    Ok(())
}

/// Decodes a picture, recognising the format from its first bytes.
///
/// # Errors
/// A short message: unknown format, damaged data, too large, or a feature not supported.
pub fn decode(bytes: &[u8]) -> Res<Bitmap> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        decode_png(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        decode_jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        decode_gif(bytes)
    } else {
        Err("this picture format is not supported")
    }
}

// ------------------------------------------------------------------ inflate

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    cnt: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Res<()> {
        while self.cnt < n {
            let b = *self
                .d
                .get(self.pos)
                .ok_or("the compressed data ends too soon")?;
            self.pos += 1;
            self.bit |= u32::from(b) << self.cnt;
            self.cnt += 8;
        }
        Ok(())
    }

    fn get(&mut self, n: u32) -> Res<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.cnt -= n;
        Ok(v)
    }
}

struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Res<Huff> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        let mut left: i32 = 1;
        for c in count.iter().skip(1) {
            left <<= 1;
            left -= i32::from(*c);
            if left < 0 {
                return Err("the compressed data is damaged");
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[usize::from(offs[usize::from(l)])] = s as u16;
                offs[usize::from(l)] += 1;
            }
        }
        Ok(Huff { count, symbol })
    }

    fn decode(&self, b: &mut Bits) -> Res<usize> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.get(1)? as i32;
            let count = i32::from(self.count[len]);
            if code - count < first {
                return self
                    .symbol
                    .get((index + (code - first)) as usize)
                    .map(|&s| usize::from(s))
                    .ok_or("the compressed data is damaged");
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("the compressed data is damaged")
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Decompresses raw DEFLATE data, producing at most `limit` bytes (more is an error).
///
/// # Errors
/// A short message when the data is damaged, ends early, or is larger than `limit`.
pub fn inflate(data: &[u8], limit: usize) -> Res<Vec<u8>> {
    let mut b = Bits {
        d: data,
        pos: 0,
        bit: 0,
        cnt: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = b.get(1)?;
        match b.get(2)? {
            0 => {
                b.bit = 0;
                b.cnt = 0;
                let h = data
                    .get(b.pos..b.pos + 4)
                    .ok_or("the compressed data ends too soon")?;
                let len = usize::from(h[0]) | usize::from(h[1]) << 8;
                let nlen = usize::from(h[2]) | usize::from(h[3]) << 8;
                if len != (!nlen & 0xFFFF) {
                    return Err("the compressed data is damaged");
                }
                b.pos += 4;
                let body = data
                    .get(b.pos..b.pos + len)
                    .ok_or("the compressed data ends too soon")?;
                if out.len() + len > limit {
                    return Err("the compressed data is larger than it claims");
                }
                out.extend_from_slice(body);
                b.pos += len;
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 {
                    let mut l = [0u8; 288];
                    for (i, x) in l.iter_mut().enumerate() {
                        *x = match i {
                            0..=143 => 8,
                            144..=255 => 9,
                            256..=279 => 7,
                            _ => 8,
                        };
                    }
                    (Huff::new(&l)?, Huff::new(&[5u8; 30])?)
                } else {
                    let nlen = b.get(5)? as usize + 257;
                    let ndist = b.get(5)? as usize + 1;
                    let ncode = b.get(4)? as usize + 4;
                    if nlen > 286 || ndist > 30 {
                        return Err("the compressed data is damaged");
                    }
                    const ORDER: [usize; 19] = [
                        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                    ];
                    let mut cl = [0u8; 19];
                    for &o in ORDER.iter().take(ncode) {
                        cl[o] = b.get(3)? as u8;
                    }
                    let clh = Huff::new(&cl)?;
                    let mut lengths = vec![0u8; nlen + ndist];
                    let mut i = 0;
                    while i < nlen + ndist {
                        let sym = clh.decode(&mut b)?;
                        if sym < 16 {
                            lengths[i] = sym as u8;
                            i += 1;
                        } else {
                            let (prev, rep) = match sym {
                                16 => {
                                    if i == 0 {
                                        return Err("the compressed data is damaged");
                                    }
                                    (lengths[i - 1], 3 + b.get(2)? as usize)
                                }
                                17 => (0, 3 + b.get(3)? as usize),
                                _ => (0, 11 + b.get(7)? as usize),
                            };
                            if i + rep > nlen + ndist {
                                return Err("the compressed data is damaged");
                            }
                            for _ in 0..rep {
                                lengths[i] = prev;
                                i += 1;
                            }
                        }
                    }
                    (Huff::new(&lengths[..nlen])?, Huff::new(&lengths[nlen..])?)
                };
                loop {
                    let sym = lit.decode(&mut b)?;
                    if sym < 256 {
                        if out.len() >= limit {
                            return Err("the compressed data is larger than it claims");
                        }
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let s = sym - 257;
                        if s >= 29 {
                            return Err("the compressed data is damaged");
                        }
                        let len = usize::from(LBASE[s]) + b.get(u32::from(LEXT[s]))? as usize;
                        let ds = dist.decode(&mut b)?;
                        if ds >= 30 {
                            return Err("the compressed data is damaged");
                        }
                        let d = usize::from(DBASE[ds]) + b.get(u32::from(DEXT[ds]))? as usize;
                        if d > out.len() {
                            return Err("the compressed data is damaged");
                        }
                        if out.len() + len > limit {
                            return Err("the compressed data is larger than it claims");
                        }
                        for _ in 0..len {
                            let v = out[out.len() - d];
                            out.push(v);
                        }
                    }
                }
            }
            _ => return Err("the compressed data is damaged"),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

/// Decompresses zlib data (the two-byte header is checked, the checksum is not).
///
/// # Errors
/// As [`inflate`].
pub fn zlib_decompress(data: &[u8], limit: usize) -> Res<Vec<u8>> {
    if data.len() < 2
        || data[0] & 0x0F != 8
        || (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 != 0
        || data[1] & 0x20 != 0
    {
        return Err("the compressed data is damaged");
    }
    inflate(&data[2..], limit)
}

// ------------------------------------------------------------------ PNG

fn be32(d: &[u8], at: usize) -> Res<u32> {
    let s = d.get(at..at + 4).ok_or("the file ends too soon")?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i32::from(a), i32::from(b), i32::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn unfilter(raw: &[u8], w: usize, h: usize, bpp: usize, row_bytes: usize) -> Res<Vec<u8>> {
    let mut out = vec![0u8; row_bytes * h];
    let _ = w;
    for y in 0..h {
        let line = raw
            .get(y * (row_bytes + 1)..(y + 1) * (row_bytes + 1))
            .ok_or("the picture data ends too soon")?;
        let (ft, src) = (line[0], &line[1..]);
        for x in 0..row_bytes {
            let a = if x >= bpp {
                out[y * row_bytes + x - bpp]
            } else {
                0
            };
            let b = if y > 0 {
                out[(y - 1) * row_bytes + x]
            } else {
                0
            };
            let c = if x >= bpp && y > 0 {
                out[(y - 1) * row_bytes + x - bpp]
            } else {
                0
            };
            let v = match ft {
                0 => src[x],
                1 => src[x].wrapping_add(a),
                2 => src[x].wrapping_add(b),
                3 => src[x].wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => src[x].wrapping_add(paeth(a, b, c)),
                _ => return Err("the picture data is damaged"),
            };
            out[y * row_bytes + x] = v;
        }
    }
    Ok(out)
}

fn decode_png(d: &[u8]) -> Res<Bitmap> {
    let mut pos = 8;
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0usize, 0usize, 0u8, 0u8, 0u8);
    let mut have_header = false;
    let mut palette: Vec<u32> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut ended = false;
    while pos + 8 <= d.len() && !ended {
        let len = be32(d, pos)? as usize;
        let kind = d.get(pos + 4..pos + 8).ok_or("the file ends too soon")?;
        let body = d
            .get(pos + 8..pos + 8 + len)
            .ok_or("the file ends too soon")?;
        match kind {
            b"IHDR" => {
                if len < 13 {
                    return Err("the picture header is damaged");
                }
                w = be32(body, 0)? as usize;
                h = be32(body, 4)? as usize;
                depth = body[8];
                ctype = body[9];
                interlace = body[12];
                check_size(w, h)?;
                have_header = true;
            }
            b"PLTE" => {
                palette = rgb_table(&body[..body.len().min(768)]);
            }
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => {
                if idat.len() + len > 64 * 1024 * 1024 {
                    return Err("the picture is too large");
                }
                idat.extend_from_slice(body);
            }
            b"IEND" => ended = true,
            _ => {}
        }
        pos += 12 + len;
    }
    if !have_header || idat.is_empty() {
        return Err("the picture has no data");
    }
    let channels = match ctype {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err("this PNG colour type is not supported"),
    };
    if !matches!(
        (ctype, depth),
        (0, 1 | 2 | 4 | 8 | 16) | (2 | 4 | 6, 8 | 16) | (3, 1 | 2 | 4 | 8)
    ) {
        return Err("this PNG bit depth is not supported");
    }
    if ctype == 3 && palette.is_empty() {
        return Err("the picture has no palette");
    }
    for (i, a) in trns.iter().enumerate().take(palette.len()) {
        if ctype == 3 {
            palette[i] = (palette[i] & 0x00FF_FFFF) | u32::from(*a) << 24;
        }
    }
    let bits_pp = channels * usize::from(depth);
    let bpp = bits_pp.div_ceil(8);
    // Passes: one for a plain picture, seven for Adam7.
    let passes: Vec<(usize, usize, usize, usize)> = if interlace == 0 {
        vec![(0, 0, 1, 1)]
    } else {
        vec![
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
    };
    let mut total = 0usize;
    for &(x0, y0, dx, dy) in &passes {
        let (pw, ph) = ((w + dx - 1 - x0) / dx, (h + dy - 1 - y0) / dy);
        if pw > 0 && ph > 0 {
            total += ph * ((pw * bits_pp).div_ceil(8) + 1);
        }
    }
    let raw = zlib_decompress(&idat, total)?;
    let mut px = vec![0u32; w * h];
    let mut at = 0usize;
    let sample = |row: &[u8], i: usize| -> u32 {
        // The i-th sample of a row (for depths below 16: packed; 16: high byte).
        match depth {
            8 => u32::from(row[i]),
            16 => u32::from(row[i * 2]),
            d => {
                let per = 8 / usize::from(d);
                let byte = row[i / per];
                let shift = 8 - usize::from(d) * (i % per + 1);
                u32::from(byte >> shift) & ((1u32 << d) - 1)
            }
        }
    };
    let scale = |v: u32| -> u32 {
        match depth {
            1 => v * 255,
            2 => v * 85,
            4 => v * 17,
            _ => v,
        }
    };
    for &(x0, y0, dx, dy) in &passes {
        let (pw, ph) = ((w + dx - 1 - x0) / dx, (h + dy - 1 - y0) / dy);
        if pw == 0 || ph == 0 {
            continue;
        }
        let row_bytes = (pw * bits_pp).div_ceil(8);
        let need = ph * (row_bytes + 1);
        let sub = unfilter(
            raw.get(at..at + need)
                .ok_or("the picture data ends too soon")?,
            pw,
            ph,
            bpp,
            row_bytes,
        )?;
        at += need;
        for py in 0..ph {
            let row = &sub[py * row_bytes..(py + 1) * row_bytes];
            for pxl in 0..pw {
                let v = match ctype {
                    0 => {
                        let g = scale(sample(row, pxl));
                        0xFF00_0000 | g << 16 | g << 8 | g
                    }
                    2 => {
                        let (r, g, b) = (
                            sample(row, pxl * 3),
                            sample(row, pxl * 3 + 1),
                            sample(row, pxl * 3 + 2),
                        );
                        0xFF00_0000 | r << 16 | g << 8 | b
                    }
                    3 => *palette
                        .get(sample(row, pxl) as usize)
                        .unwrap_or(&0xFF00_0000),
                    4 => {
                        let (g, a) = (sample(row, pxl * 2), sample(row, pxl * 2 + 1));
                        a << 24 | g << 16 | g << 8 | g
                    }
                    _ => {
                        let (r, g, b, a) = (
                            sample(row, pxl * 4),
                            sample(row, pxl * 4 + 1),
                            sample(row, pxl * 4 + 2),
                            sample(row, pxl * 4 + 3),
                        );
                        a << 24 | r << 16 | g << 8 | b
                    }
                };
                px[(y0 + py * dy) * w + x0 + pxl * dx] = v;
            }
        }
    }
    Ok(Bitmap { w, h, px })
}

// ------------------------------------------------------------------ GIF

fn decode_gif(d: &[u8]) -> Res<Bitmap> {
    let rd16 = |at: usize| -> Res<usize> {
        let s = d.get(at..at + 2).ok_or("the file ends too soon")?;
        Ok(usize::from(s[0]) | usize::from(s[1]) << 8)
    };
    let (sw, sh) = (rd16(6)?, rd16(8)?);
    check_size(sw, sh)?;
    let flags = *d.get(10).ok_or("the file ends too soon")?;
    let mut pos = 13;
    let mut global: Vec<u32> = Vec::new();
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        let t = d.get(pos..pos + n * 3).ok_or("the file ends too soon")?;
        global = rgb_table(t);
        pos += n * 3;
    }
    let mut transparent: Option<u8> = None;
    loop {
        match *d.get(pos).ok_or("the file ends too soon")? {
            0x21 => {
                let label = *d.get(pos + 1).ok_or("the file ends too soon")?;
                pos += 2;
                if label == 0xF9 && d.get(pos) == Some(&4) {
                    let f = *d.get(pos + 1).ok_or("the file ends too soon")?;
                    if f & 1 != 0 {
                        transparent = Some(*d.get(pos + 4).ok_or("the file ends too soon")?);
                    }
                }
                loop {
                    let n = usize::from(*d.get(pos).ok_or("the file ends too soon")?);
                    pos += 1 + n;
                    if n == 0 {
                        break;
                    }
                }
            }
            0x2C => {
                let (ix, iy, iw, ih) = (
                    rd16(pos + 1)?,
                    rd16(pos + 3)?,
                    rd16(pos + 5)?,
                    rd16(pos + 7)?,
                );
                let f = *d.get(pos + 9).ok_or("the file ends too soon")?;
                pos += 10;
                check_size(iw.max(1), ih.max(1))?;
                let mut table = global.clone();
                if f & 0x80 != 0 {
                    let n = 2usize << (f & 7);
                    let t = d.get(pos..pos + n * 3).ok_or("the file ends too soon")?;
                    table = rgb_table(t);
                    pos += n * 3;
                }
                let min = *d.get(pos).ok_or("the file ends too soon")?;
                pos += 1;
                if !(2..=8).contains(&min) {
                    return Err("the picture data is damaged");
                }
                let mut data = Vec::new();
                loop {
                    let n = usize::from(*d.get(pos).ok_or("the file ends too soon")?);
                    pos += 1;
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(d.get(pos..pos + n).ok_or("the file ends too soon")?);
                    pos += n;
                }
                let idx = lzw(&data, min, iw * ih)?;
                let mut px = vec![0u32; sw * sh];
                let rows: Vec<usize> = if f & 0x40 != 0 {
                    let mut r = Vec::new();
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        r.extend((start..ih).step_by(step));
                    }
                    r
                } else {
                    (0..ih).collect()
                };
                for (n, &row) in rows.iter().enumerate() {
                    for x in 0..iw {
                        let i = idx.get(n * iw + x).copied().unwrap_or(0);
                        let (px_x, px_y) = (ix + x, iy + row);
                        if px_x < sw && px_y < sh && transparent != Some(i) {
                            px[px_y * sw + px_x] =
                                *table.get(usize::from(i)).unwrap_or(&0xFF00_0000);
                        }
                    }
                }
                return Ok(Bitmap { w: sw, h: sh, px });
            }
            _ => return Err("the picture has no image data"),
        }
    }
}

fn lzw(data: &[u8], min: u8, want: usize) -> Res<Vec<u8>> {
    let clear = 1usize << min;
    let end = clear + 1;
    let mut size = u32::from(min) + 1;
    let mut table: Vec<(u16, u8)> = Vec::new(); // (prefix, byte); entries start at clear + 2
    let reset = |t: &mut Vec<(u16, u8)>| {
        t.clear();
    };
    let mut out: Vec<u8> = Vec::with_capacity(want);
    let (mut bits, mut cnt, mut pos) = (0u32, 0u32, 0usize);
    let mut prev: Option<usize> = None;
    let expand = |t: &[(u16, u8)], mut code: usize, buf: &mut Vec<u8>| -> u8 {
        buf.clear();
        while code >= clear + 2 {
            let (p, b) = t[code - clear - 2];
            buf.push(b);
            code = usize::from(p);
        }
        buf.push(code as u8);
        buf.reverse();
        buf[0]
    };
    let mut buf: Vec<u8> = Vec::new();
    while out.len() < want {
        while cnt < size {
            let Some(&b) = data.get(pos) else {
                return Ok(pad(out, want));
            };
            pos += 1;
            bits |= u32::from(b) << cnt;
            cnt += 8;
        }
        let code = (bits & ((1 << size) - 1)) as usize;
        bits >>= size;
        cnt -= size;
        if code == clear {
            reset(&mut table);
            size = u32::from(min) + 1;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        let next = clear + 2 + table.len();
        match prev {
            None => {
                if code >= clear {
                    return Err("the picture data is damaged");
                }
                out.push(code as u8);
            }
            Some(p) => {
                let first;
                if code < next {
                    first = expand(&table, code, &mut buf);
                    out.extend_from_slice(&buf);
                } else if code == next {
                    let f = expand(&table, p, &mut buf);
                    out.extend_from_slice(&buf);
                    out.push(f);
                    first = f;
                } else {
                    return Err("the picture data is damaged");
                }
                if table.len() < 4096 - clear - 2 {
                    table.push((p as u16, first));
                }
            }
        }
        prev = Some(code);
        if clear + 2 + table.len() >= (1 << size) && size < 12 {
            size += 1;
        }
        if out.len() > want + 4096 {
            break;
        }
    }
    Ok(pad(out, want))
}

fn pad(mut v: Vec<u8>, want: usize) -> Vec<u8> {
    v.resize(want, 0);
    v
}

// ------------------------------------------------------------------ JPEG (baseline)

struct JHuff {
    mincode: [i32; 17],
    maxcode: [i32; 18],
    valptr: [usize; 17],
    vals: Vec<u8>,
}

impl JHuff {
    fn new(counts: &[u8], vals: &[u8]) -> JHuff {
        let mut h = JHuff {
            mincode: [0; 17],
            maxcode: [-1; 18],
            valptr: [0; 17],
            vals: vals.to_vec(),
        };
        let (mut code, mut k) = (0i32, 0usize);
        for l in 1..=16 {
            h.valptr[l] = k;
            h.mincode[l] = code;
            code += i32::from(counts[l - 1]);
            k += usize::from(counts[l - 1]);
            h.maxcode[l] = if counts[l - 1] == 0 { -1 } else { code - 1 };
            code <<= 1;
        }
        h
    }
}

struct JBits<'a> {
    d: &'a [u8],
    pos: usize,
    bits: u32,
    cnt: u32,
    marker: bool,
}

impl JBits<'_> {
    fn bit(&mut self) -> u32 {
        if self.cnt == 0 {
            let b = if self.marker {
                0
            } else {
                match self.d.get(self.pos) {
                    None => {
                        self.marker = true;
                        0
                    }
                    Some(&0xFF) => {
                        if self.d.get(self.pos + 1) == Some(&0) {
                            self.pos += 2;
                            0xFF
                        } else {
                            self.marker = true;
                            0
                        }
                    }
                    Some(&b) => {
                        self.pos += 1;
                        b
                    }
                }
            };
            self.bits = u32::from(b);
            self.cnt = 8;
        }
        self.cnt -= 1;
        (self.bits >> self.cnt) & 1
    }

    fn receive(&mut self, n: u32) -> i32 {
        let mut v = 0i32;
        for _ in 0..n {
            v = v << 1 | self.bit() as i32;
        }
        v
    }

    fn extend(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let v = self.receive(n);
        if v < 1 << (n - 1) {
            v - (1 << n) + 1
        } else {
            v
        }
    }

    fn decode(&mut self, h: &JHuff) -> Res<u8> {
        let mut code = 0i32;
        for l in 1..=16 {
            code = code << 1 | self.bit() as i32;
            if h.maxcode[l] >= 0 && code <= h.maxcode[l] && code >= h.mincode[l] {
                let idx = h.valptr[l] + (code - h.mincode[l]) as usize;
                return h
                    .vals
                    .get(idx)
                    .copied()
                    .ok_or("the picture data is damaged");
            }
        }
        Err("the picture data is damaged")
    }

    fn restart(&mut self) {
        self.cnt = 0;
        self.marker = false;
        // Skip to after the next RSTn marker.
        while self.pos + 1 < self.d.len() {
            if self.d[self.pos] == 0xFF && (0xD0..=0xD7).contains(&self.d[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }
}

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

fn idct(block: &[i32; 64], out: &mut [u8; 64]) {
    // A straightforward separable floating-point inverse DCT; exact enough for display.
    let mut tmp = [0f32; 64];
    let c = |k: usize| {
        if k == 0 {
            std::f32::consts::FRAC_1_SQRT_2
        } else {
            1.0
        }
    };
    let mut cos = [[0f32; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, v) in row.iter_mut().enumerate() {
            *v = (((2 * x + 1) * u) as f32 * std::f32::consts::PI / 16.0).cos();
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for u in 0..8 {
                s += c(u) * block[y * 8 + u] as f32 * cos[x][u];
            }
            tmp[y * 8 + x] = s / 2.0;
        }
    }
    for x in 0..8 {
        for y in 0..8 {
            let mut s = 0.0;
            for v in 0..8 {
                s += c(v) * tmp[v * 8 + x] * cos[y][v];
            }
            out[y * 8 + x] = (s / 2.0 + 128.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[derive(Clone, Default)]
struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    dc: usize,
    ac: usize,
}

fn decode_jpeg(d: &[u8]) -> Res<Bitmap> {
    let mut pos = 2;
    let mut qt = [[0i32; 64]; 4];
    let mut dc_tabs: [Option<JHuff>; 4] = [None, None, None, None];
    let mut ac_tabs: [Option<JHuff>; 4] = [None, None, None, None];
    let mut comps: Vec<Comp> = Vec::new();
    let (mut w, mut h) = (0usize, 0usize);
    let mut restart = 0usize;
    loop {
        // Find the next marker.
        while d.get(pos) != Some(&0xFF) {
            pos += 1;
            if pos >= d.len() {
                return Err("the file ends too soon");
            }
        }
        while d.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let m = *d.get(pos).ok_or("the file ends too soon")?;
        pos += 1;
        if m == 0xD9 {
            return Err("the picture has no image data");
        }
        if m == 0x01 || (0xD0..=0xD8).contains(&m) {
            continue;
        }
        let len = usize::from(*d.get(pos).ok_or("the file ends too soon")?) << 8
            | usize::from(*d.get(pos + 1).ok_or("the file ends too soon")?);
        let seg = d.get(pos + 2..pos + len).ok_or("the file ends too soon")?;
        match m {
            0xDB => {
                let mut s = seg;
                while !s.is_empty() {
                    let (pq, tq) = (s[0] >> 4, usize::from(s[0] & 15));
                    if tq > 3 {
                        return Err("the picture data is damaged");
                    }
                    let n = if pq == 0 { 64 } else { 128 };
                    let t = s.get(1..1 + n).ok_or("the picture data is damaged")?;
                    for i in 0..64 {
                        qt[tq][ZIGZAG[i]] = if pq == 0 {
                            i32::from(t[i])
                        } else {
                            i32::from(t[i * 2]) << 8 | i32::from(t[i * 2 + 1])
                        };
                    }
                    s = &s[1 + n..];
                }
            }
            0xC4 => {
                let mut s = seg;
                while s.len() >= 17 {
                    let (tc, th) = (s[0] >> 4, usize::from(s[0] & 15));
                    let counts = &s[1..17];
                    let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
                    let vals = s.get(17..17 + total).ok_or("the picture data is damaged")?;
                    if th > 3 {
                        return Err("the picture data is damaged");
                    }
                    let t = JHuff::new(counts, vals);
                    if tc == 0 {
                        dc_tabs[th] = Some(t);
                    } else {
                        ac_tabs[th] = Some(t);
                    }
                    s = &s[17 + total..];
                }
            }
            0xC0 | 0xC1 => {
                if seg.len() < 6 || seg[0] != 8 {
                    return Err("only 8-bit JPEG pictures are supported");
                }
                h = usize::from(seg[1]) << 8 | usize::from(seg[2]);
                w = usize::from(seg[3]) << 8 | usize::from(seg[4]);
                check_size(w, h)?;
                let n = usize::from(seg[5]);
                if !(n == 1 || n == 3) || seg.len() < 6 + n * 3 {
                    return Err("this JPEG colour layout is not supported");
                }
                comps = (0..n)
                    .map(|i| {
                        let c = &seg[6 + i * 3..9 + i * 3];
                        Comp {
                            id: c[0],
                            h: usize::from(c[1] >> 4),
                            v: usize::from(c[1] & 15),
                            tq: usize::from(c[2] & 3),
                            dc: 0,
                            ac: 0,
                        }
                    })
                    .collect();
                if comps
                    .iter()
                    .any(|c| c.h == 0 || c.v == 0 || c.h > 4 || c.v > 4)
                {
                    return Err("the picture data is damaged");
                }
            }
            0xC2 => return Err("progressive JPEG pictures are not supported yet"),
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err("this kind of JPEG is not supported")
            }
            0xDD => {
                if seg.len() >= 2 {
                    restart = usize::from(seg[0]) << 8 | usize::from(seg[1]);
                }
            }
            0xDA => {
                if comps.is_empty() {
                    return Err("the picture data is damaged");
                }
                let n = usize::from(*seg.first().ok_or("the picture data is damaged")?);
                if n != comps.len() || seg.len() < 1 + n * 2 {
                    return Err("this JPEG scan layout is not supported");
                }
                for i in 0..n {
                    let (cid, t) = (seg[1 + i * 2], seg[2 + i * 2]);
                    let c = comps
                        .iter_mut()
                        .find(|c| c.id == cid)
                        .ok_or("the picture data is damaged")?;
                    c.dc = usize::from(t >> 4) & 3;
                    c.ac = usize::from(t & 15) & 3;
                }
                return jpeg_scan(d, pos + len, &comps, &qt, &dc_tabs, &ac_tabs, w, h, restart);
            }
            _ => {}
        }
        pos += len;
    }
}

#[allow(clippy::too_many_arguments)]
fn jpeg_scan(
    d: &[u8],
    start: usize,
    comps: &[Comp],
    qt: &[[i32; 64]; 4],
    dc_tabs: &[Option<JHuff>; 4],
    ac_tabs: &[Option<JHuff>; 4],
    w: usize,
    h: usize,
    restart: usize,
) -> Res<Bitmap> {
    let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
    let (mcu_w, mcu_h) = (8 * hmax, 8 * vmax);
    let (mcus_x, mcus_y) = (w.div_ceil(mcu_w), h.div_ceil(mcu_h));
    // One plane per component, sized to whole MCUs.
    let mut planes: Vec<(usize, Vec<u8>)> = comps
        .iter()
        .map(|c| {
            (
                mcus_x * c.h * 8,
                vec![0u8; mcus_x * c.h * 8 * mcus_y * c.v * 8],
            )
        })
        .collect();
    let mut br = JBits {
        d,
        pos: start,
        bits: 0,
        cnt: 0,
        marker: false,
    };
    let mut pred = vec![0i32; comps.len()];
    let mut count = 0usize;
    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart > 0 && count > 0 && count.is_multiple_of(restart) {
                br.restart();
                pred.iter_mut().for_each(|p| *p = 0);
            }
            count += 1;
            for (ci, c) in comps.iter().enumerate() {
                let dct = dc_tabs[c.dc]
                    .as_ref()
                    .ok_or("the picture is missing a table")?;
                let act = ac_tabs[c.ac]
                    .as_ref()
                    .ok_or("the picture is missing a table")?;
                for by in 0..c.v {
                    for bx in 0..c.h {
                        let mut coef = [0i32; 64];
                        let t = br.decode(dct)?;
                        if t > 16 {
                            return Err("the picture data is damaged");
                        }
                        pred[ci] += br.extend(u32::from(t));
                        coef[0] = pred[ci] * qt[c.tq][0];
                        let mut k = 1;
                        while k < 64 {
                            let rs = br.decode(act)?;
                            let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
                            if s == 0 {
                                if r == 15 {
                                    k += 16;
                                    continue;
                                }
                                break;
                            }
                            k += r;
                            if k > 63 {
                                break;
                            }
                            coef[ZIGZAG[k]] = br.extend(s) * qt[c.tq][ZIGZAG[k]];
                            k += 1;
                        }
                        let mut out = [0u8; 64];
                        idct(&coef, &mut out);
                        let (stride, plane) = &mut planes[ci];
                        let (x0, y0) = ((mx * c.h + bx) * 8, (my * c.v + by) * 8);
                        for y in 0..8 {
                            let row = (y0 + y) * *stride + x0;
                            plane[row..row + 8].copy_from_slice(&out[y * 8..y * 8 + 8]);
                        }
                    }
                }
            }
        }
    }
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let sample = |ci: usize| -> i32 {
                let c = &comps[ci];
                let (stride, plane) = &planes[ci];
                let (sx, sy) = (x * c.h / hmax, y * c.v / vmax);
                i32::from(plane[sy * stride + sx])
            };
            let (r, g, b) = if comps.len() == 1 {
                let l = sample(0);
                (l, l, l)
            } else {
                let (yy, cb, cr) = (sample(0), sample(1) - 128, sample(2) - 128);
                let r = yy as f32 + 1.402 * cr as f32;
                let g = yy as f32 - 0.344_136 * cb as f32 - 0.714_136 * cr as f32;
                let b = yy as f32 + 1.772 * cb as f32;
                (r.round() as i32, g.round() as i32, b.round() as i32)
            };
            let cl = |v: i32| v.clamp(0, 255) as u32;
            px[y * w + x] = 0xFF00_0000 | cl(r) << 16 | cl(g) << 8 | cl(b);
        }
    }
    Ok(Bitmap { w, h, px })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_fixtures as fx;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// Compares a decoded picture with the pixels another decoder produced; `tol` is the largest
    /// allowed difference per colour channel.
    fn same(b: &Bitmap, size: (usize, usize), reference: &str, tol: i32) {
        assert_eq!((b.w, b.h), size);
        let r = unhex(reference);
        let mut worst = 0;
        for (i, p) in b.px.iter().enumerate() {
            let want = [r[i * 4], r[i * 4 + 1], r[i * 4 + 2], r[i * 4 + 3]];
            let got = [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8];
            for k in 0..4 {
                worst = worst.max((i32::from(want[k]) - i32::from(got[k])).abs());
            }
        }
        assert!(
            worst <= tol,
            "largest channel difference {worst} (allowed {tol})"
        );
    }

    #[test]
    fn inflate_reads_stored_fixed_and_dynamic_blocks() {
        let t = zlib_decompress(&unhex(fx::Z_TEXT), fx::Z_TEXT_LEN).unwrap();
        assert_eq!(t.len(), fx::Z_TEXT_LEN);
        assert!(t.starts_with(b"the quick brown fox") && t.ends_with(b"lazy dog. "));
        assert_eq!(
            zlib_decompress(&unhex(fx::Z_RAND), 3000).unwrap(),
            unhex(fx::Z_RAND_HEX),
            "incompressible data is stored"
        );
        assert_eq!(
            zlib_decompress(&unhex(fx::Z_FIXED), 12).unwrap(),
            b"abcabcabcabc"
        );
        // The size limit is enforced while decompressing, so a small file cannot fill memory.
        assert!(zlib_decompress(&unhex(fx::Z_TEXT), 1000).is_err());
        assert!(zlib_decompress(&unhex(fx::Z_TEXT), fx::Z_TEXT_LEN - 1).is_err());
    }

    #[test]
    fn png_pictures_match_an_independent_decoder_exactly() {
        same(
            &decode(&unhex(fx::PNG_RGB)).unwrap(),
            fx::PNG_RGB_SIZE,
            fx::PNG_RGB_REF,
            0,
        );
        same(
            &decode(&unhex(fx::PNG_RGBA)).unwrap(),
            fx::PNG_RGBA_SIZE,
            fx::PNG_RGBA_REF,
            0,
        );
        same(
            &decode(&unhex(fx::PNG_GRAY)).unwrap(),
            fx::PNG_GRAY_SIZE,
            fx::PNG_GRAY_REF,
            0,
        );
        same(
            &decode(&unhex(fx::PNG_PAL)).unwrap(),
            fx::PNG_PAL_SIZE,
            fx::PNG_PAL_REF,
            0,
        );
    }

    #[test]
    fn jpeg_pictures_are_close_to_an_independent_decoder() {
        same(
            &decode(&unhex(fx::JPG_RGB)).unwrap(),
            fx::JPG_RGB_SIZE,
            fx::JPG_RGB_REF,
            6,
        );
        same(
            &decode(&unhex(fx::JPG_GRAY)).unwrap(),
            fx::JPG_GRAY_SIZE,
            fx::JPG_GRAY_REF,
            4,
        );
        // Chroma subsampling is upsampled differently (we repeat samples, others interpolate).
        same(
            &decode(&unhex(fx::JPG_420)).unwrap(),
            fx::JPG_420_SIZE,
            fx::JPG_420_REF,
            40,
        );
        same(
            &decode(&unhex(fx::JPG_RST)).unwrap(),
            fx::JPG_RST_SIZE,
            fx::JPG_RST_REF,
            6,
        );
        assert_eq!(
            decode(&unhex(fx::JPG_PROG)).unwrap_err(),
            "progressive JPEG pictures are not supported yet"
        );
    }

    #[test]
    fn gif_first_frame_matches() {
        same(
            &decode(&unhex(fx::GIF)).unwrap(),
            fx::GIF_SIZE,
            fx::GIF_REF,
            0,
        );
    }

    #[test]
    fn interlaced_png_is_put_back_in_order() {
        // 3x3 grey picture, interlaced (Adam7), written by hand: pixel values 10*(y*3+x).
        let w = 3;
        let h = 3;
        let passes: [(usize, usize, usize, usize); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        let mut raw = Vec::new();
        for (x0, y0, dx, dy) in passes {
            let (pw, ph) = ((w + dx - 1 - x0) / dx, (h + dy - 1 - y0) / dy);
            if pw == 0 || ph == 0 {
                continue;
            }
            for py in 0..ph {
                raw.push(0u8);
                for px in 0..pw {
                    raw.push((10 * ((y0 + py * dy) * 3 + x0 + px * dx)) as u8);
                }
            }
        }
        // Stored deflate block inside a zlib wrapper.
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
        z.extend_from_slice(&raw);
        z.extend_from_slice(&[0, 0, 0, 0]);
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut chunk = |kind: &[u8; 4], body: &[u8]| {
            png.extend_from_slice(&(body.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(body);
            png.extend_from_slice(&[0, 0, 0, 0]); // checksums are not checked
        };
        chunk(b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 3, 8, 0, 0, 0, 1]);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        let b = decode(&png).unwrap();
        let grey: Vec<u32> = b.px.iter().map(|p| p & 0xFF).collect();
        assert_eq!(grey, (0..9).map(|i| i * 10).collect::<Vec<u32>>());
    }

    #[test]
    fn damaged_and_hostile_files_give_errors_not_panics() {
        for name in [fx::PNG_RGB, fx::PNG_PAL, fx::JPG_RGB, fx::JPG_420, fx::GIF] {
            let good = unhex(name);
            for cut in (0..good.len()).step_by(7) {
                let _ = decode(&good[..cut]);
            }
            for i in (0..good.len()).step_by(5) {
                let mut bad = good.clone();
                bad[i] ^= 0xA5;
                let _ = decode(&bad);
            }
        }
        assert!(decode(b"").is_err() && decode(b"not a picture").is_err());
        // A header that promises a huge picture is refused before anything is allocated.
        let mut png = unhex(fx::PNG_RGB);
        png[16..20].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
        png[20..24].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
        assert_eq!(decode(&png).unwrap_err(), "the picture is too large");
        // A tiny compressed file that claims to be small cannot expand beyond what the header says.
        assert!(inflate(&[0x03, 0x00], 0).is_ok());
        assert!(inflate(&[0xFF, 0xFF, 0xFF], 100).is_err());
        let mut gif = unhex(fx::GIF);
        gif[6] = 0xFF;
        gif[7] = 0xFF;
        gif[8] = 0xFF;
        gif[9] = 0xFF;
        assert!(decode(&gif).is_err());
    }
}
