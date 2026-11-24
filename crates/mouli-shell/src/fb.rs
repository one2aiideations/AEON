//! Writing the screen to a Linux framebuffer device.

use mouli_screen::raster::Canvas;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// What can go wrong with the framebuffer.
#[derive(Debug)]
pub enum FbError {
    /// A sysfs file or the device could not be read or written.
    Io(String, io::Error),
    /// A sysfs file did not hold what was expected.
    Parse(String),
    /// The framebuffer uses a pixel depth this code does not handle.
    UnsupportedDepth(usize),
    /// The picture is larger than the screen.
    TooLarge {
        /// Picture size.
        canvas: (usize, usize),
        /// Screen size.
        screen: (usize, usize),
    },
}

impl fmt::Display for FbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FbError::Io(what, e) => write!(f, "{what}: {e}"),
            FbError::Parse(what) => write!(f, "unexpected content in {what}"),
            FbError::UnsupportedDepth(b) => {
                write!(f, "{b} bits per pixel is not supported (need 16 or 32)")
            }
            FbError::TooLarge { canvas, screen } => write!(
                f,
                "picture {}x{} does not fit the {}x{} screen",
                canvas.0, canvas.1, screen.0, screen.1
            ),
        }
    }
}

impl std::error::Error for FbError {}

/// The screen's geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FbInfo {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Bits per pixel.
    pub bpp: usize,
    /// Bytes from the start of one row to the start of the next.
    pub stride: usize,
}

fn read_trim(path: &Path) -> Result<String, FbError> {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| FbError::Io(path.display().to_string(), e))
}

impl FbInfo {
    /// Reads the geometry of the framebuffer described by the sysfs directory `sys`
    /// (normally `/sys/class/graphics/fb0`): `virtual_size` ("width,height"), `bits_per_pixel`
    /// and `stride`. A missing `stride` is taken to be `width * bits_per_pixel / 8`.
    pub fn read(sys: &Path) -> Result<Self, FbError> {
        let size_path = sys.join("virtual_size");
        let size = read_trim(&size_path)?;
        let (w, h) = size
            .split_once(',')
            .ok_or_else(|| FbError::Parse(size_path.display().to_string()))?;
        let parse = |s: &str, p: &Path| {
            s.trim()
                .parse::<usize>()
                .map_err(|_| FbError::Parse(p.display().to_string()))
        };
        let (width, height) = (parse(w, &size_path)?, parse(h, &size_path)?);
        let bpp_path = sys.join("bits_per_pixel");
        let bpp = parse(&read_trim(&bpp_path)?, &bpp_path)?;
        let stride_path = sys.join("stride");
        let stride = match read_trim(&stride_path) {
            Ok(s) => parse(&s, &stride_path)?,
            Err(_) => width * bpp / 8,
        };
        if width == 0 || height == 0 || stride < width * bpp / 8 {
            return Err(FbError::Parse(sys.display().to_string()));
        }
        Ok(Self {
            width,
            height,
            bpp,
            stride,
        })
    }
}

/// The order of colour bytes in a 32-bit pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelOrder {
    /// Blue, green, red, unused: what firmware (EFI/VESA) and most virtual GPUs use.
    #[default]
    Bgrx,
    /// Red, green, blue, unused.
    Rgbx,
}

/// An open framebuffer.
#[derive(Debug)]
pub struct Framebuffer {
    file: File,
    info: FbInfo,
    order: PixelOrder,
    /// The bytes last written for each row, so unchanged rows are skipped.
    shown: Vec<Option<Vec<u8>>>,
}

impl Framebuffer {
    /// Opens the device `dev` (normally `/dev/fb0`) whose geometry is described by `sys`.
    pub fn open(dev: &Path, sys: &Path, order: PixelOrder) -> Result<Self, FbError> {
        let info = FbInfo::read(sys)?;
        if info.bpp != 16 && info.bpp != 32 {
            return Err(FbError::UnsupportedDepth(info.bpp));
        }
        let file = OpenOptions::new()
            .write(true)
            .open(dev)
            .map_err(|e| FbError::Io(dev.display().to_string(), e))?;
        Ok(Self {
            file,
            info,
            order,
            shown: vec![None; info.height],
        })
    }

    /// The screen's geometry.
    pub fn info(&self) -> FbInfo {
        self.info
    }

    fn encode_row(&self, canvas: &Canvas, y: usize) -> Vec<u8> {
        let row = canvas
            .pixels
            .get(y * canvas.width..(y + 1) * canvas.width)
            .unwrap_or(&[]);
        let mut out = Vec::with_capacity(row.len() * self.info.bpp / 8);
        for &p in row {
            let (r, g, b) = (
                ((p >> 16) & 0xFF) as u8,
                ((p >> 8) & 0xFF) as u8,
                (p & 0xFF) as u8,
            );
            if self.info.bpp == 32 {
                match self.order {
                    PixelOrder::Bgrx => out.extend_from_slice(&[b, g, r, 0]),
                    PixelOrder::Rgbx => out.extend_from_slice(&[r, g, b, 0]),
                }
            } else {
                let v =
                    ((u16::from(r) >> 3) << 11) | ((u16::from(g) >> 2) << 5) | (u16::from(b) >> 3);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    /// Shows `canvas` at the top left of the screen. Only rows that differ from what is already
    /// shown are written. Returns how many rows were written.
    pub fn present(&mut self, canvas: &Canvas) -> Result<usize, FbError> {
        if canvas.width > self.info.width || canvas.height > self.info.height {
            return Err(FbError::TooLarge {
                canvas: (canvas.width, canvas.height),
                screen: (self.info.width, self.info.height),
            });
        }
        let mut written = 0;
        for y in 0..canvas.height {
            let bytes = self.encode_row(canvas, y);
            if self.shown.get(y).and_then(Option::as_ref) == Some(&bytes) {
                continue;
            }
            self.file
                .write_all_at(&bytes, (y * self.info.stride) as u64)
                .map_err(|e| FbError::Io("framebuffer".into(), e))?;
            if let Some(slot) = self.shown.get_mut(y) {
                *slot = Some(bytes);
            }
            written += 1;
        }
        Ok(written)
    }

    /// Forgets what is believed to be on screen, so the next [`Framebuffer::present`] rewrites
    /// every row (use after something else may have drawn on the screen).
    pub fn invalidate(&mut self) {
        self.shown.iter_mut().for_each(|s| *s = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!("mouli-fb-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake sysfs directory and device file for a `w`x`h` screen with `bpp` bits and `stride`.
    fn fake(
        tag: &str,
        w: usize,
        h: usize,
        bpp: usize,
        stride: Option<usize>,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let root = tmp(tag);
        let sys = root.join("fb0");
        fs::create_dir_all(&sys).unwrap();
        fs::write(sys.join("virtual_size"), format!("{w},{h}\n")).unwrap();
        fs::write(sys.join("bits_per_pixel"), format!("{bpp}\n")).unwrap();
        let st = stride.unwrap_or(w * bpp / 8);
        if stride.is_some() {
            fs::write(sys.join("stride"), format!("{st}\n")).unwrap();
        }
        let dev = root.join("dev-fb0");
        fs::write(&dev, vec![0xEEu8; st * h]).unwrap();
        (root, sys, dev)
    }

    fn canvas(w: usize, h: usize, f: impl Fn(usize, usize) -> u32) -> Canvas {
        Canvas {
            width: w,
            height: h,
            pixels: (0..w * h).map(|i| f(i % w, i / w)).collect(),
        }
    }

    #[test]
    fn geometry_is_read_from_sysfs() {
        let (root, sys, _) = fake("info", 1280, 800, 32, Some(5120));
        assert_eq!(
            FbInfo::read(&sys).unwrap(),
            FbInfo {
                width: 1280,
                height: 800,
                bpp: 32,
                stride: 5120
            }
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_stride_is_computed() {
        let (root, sys, _) = fake("nostride", 640, 480, 16, None);
        assert_eq!(FbInfo::read(&sys).unwrap().stride, 1280);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nonsense_geometry_is_refused() {
        let (root, sys, _) = fake("bad", 100, 50, 32, Some(400));
        fs::write(sys.join("virtual_size"), "wide,tall").unwrap();
        assert!(matches!(FbInfo::read(&sys), Err(FbError::Parse(_))));
        fs::write(sys.join("virtual_size"), "100 50").unwrap();
        assert!(matches!(FbInfo::read(&sys), Err(FbError::Parse(_))));
        fs::write(sys.join("virtual_size"), "0,0").unwrap();
        assert!(matches!(FbInfo::read(&sys), Err(FbError::Parse(_))));
        fs::write(sys.join("virtual_size"), "100,50").unwrap();
        fs::write(sys.join("stride"), "10").unwrap();
        assert!(
            matches!(FbInfo::read(&sys), Err(FbError::Parse(_))),
            "a stride shorter than a row is nonsense"
        );
        assert!(matches!(
            FbInfo::read(&root.join("missing")),
            Err(FbError::Io(..))
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pixels_land_at_the_right_bytes_in_bgrx_order() {
        // Stride is wider than a row (as on real hardware), so row offsets must use it.
        let (root, sys, dev) = fake("bgrx", 4, 3, 32, Some(24));
        let mut fb = Framebuffer::open(&dev, &sys, PixelOrder::Bgrx).unwrap();
        let c = canvas(4, 3, |x, y| 0x10_20_30 + ((y * 4 + x) as u32));
        assert_eq!(fb.present(&c).unwrap(), 3);
        let bytes = fs::read(&dev).unwrap();
        assert_eq!(bytes.len(), 72);
        // Pixel (1, 1): value 0x102035, written as B, G, R, X at y*24 + x*4.
        assert_eq!(&bytes[24 + 4..24 + 8], &[0x35, 0x20, 0x10, 0]);
        // Row padding beyond the 16 bytes of pixels is left untouched.
        assert!(bytes[16..24].iter().all(|b| *b == 0xEE));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rgbx_order_and_rgb565_depth() {
        let (root, sys, dev) = fake("rgbx", 2, 1, 32, None);
        let mut fb = Framebuffer::open(&dev, &sys, PixelOrder::Rgbx).unwrap();
        fb.present(&canvas(2, 1, |_, _| 0x112233)).unwrap();
        assert_eq!(&fs::read(&dev).unwrap()[..4], &[0x11, 0x22, 0x33, 0]);
        let _ = fs::remove_dir_all(root);
        let (root, sys, dev) = fake("565", 2, 1, 16, None);
        let mut fb = Framebuffer::open(&dev, &sys, PixelOrder::Bgrx).unwrap();
        fb.present(&canvas(
            2,
            1,
            |x, _| if x == 0 { 0x112233 } else { 0xFFFFFF },
        ))
        .unwrap();
        assert_eq!(fs::read(&dev).unwrap(), vec![0x06, 0x11, 0xFF, 0xFF]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn only_changed_rows_are_written() {
        let (root, sys, dev) = fake("diff", 8, 10, 32, None);
        let mut fb = Framebuffer::open(&dev, &sys, PixelOrder::Bgrx).unwrap();
        let base = canvas(8, 10, |_, y| y as u32);
        assert_eq!(fb.present(&base).unwrap(), 10);
        assert_eq!(
            fb.present(&base).unwrap(),
            0,
            "an identical picture writes nothing"
        );
        let mut changed = base.clone();
        changed.pixels[3 * 8 + 2] = 0xFFFFFF;
        changed.pixels[7 * 8] = 0xFFFFFF;
        assert_eq!(fb.present(&changed).unwrap(), 2);
        fb.invalidate();
        assert_eq!(
            fb.present(&changed).unwrap(),
            10,
            "after invalidate everything is rewritten"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unsupported_depths_and_oversize_pictures_are_refused() {
        let (root, sys, dev) = fake("depth", 4, 4, 24, None);
        assert!(matches!(
            Framebuffer::open(&dev, &sys, PixelOrder::Bgrx),
            Err(FbError::UnsupportedDepth(24))
        ));
        let _ = fs::remove_dir_all(root);
        let (root, sys, dev) = fake("big", 4, 4, 32, None);
        let mut fb = Framebuffer::open(&dev, &sys, PixelOrder::Bgrx).unwrap();
        assert!(matches!(
            fb.present(&canvas(5, 4, |_, _| 0)),
            Err(FbError::TooLarge { .. })
        ));
        assert!(matches!(
            fb.present(&canvas(4, 5, |_, _| 0)),
            Err(FbError::TooLarge { .. })
        ));
        assert!(matches!(
            Framebuffer::open(&root.join("nope"), &sys, PixelOrder::Bgrx),
            Err(FbError::Io(..))
        ));
        assert!(
            fb.present(&canvas(2, 2, |_, _| 0)).is_ok(),
            "a smaller picture is fine"
        );
        let _ = fs::remove_dir_all(root);
    }
}
