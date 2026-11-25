//! Drawing web pages: the display list from `mouli-web` turned into pixels with the embedded
//! fonts, and a [`Page`] that holds a loaded document for scrolling and clicking.
//!
//! Text is drawn from the 14-pixel Roboto face (the medium weight for bold, the monospace face
//! for code) scaled to any size with bilinear sampling; italics are sheared. Shapes are
//! anti-aliased with a signed-distance test, so rounded corners, circles and borders are smooth.

use crate::raster::Canvas;
use crate::typeface::{self, blend, Face};
use mouli_web::css::{State, Stylesheet};
use mouli_web::display::{paint, Op};
use mouli_web::dom::{Dom, NodeId};
use mouli_web::layout::{hit_test, layout, node_rect, FontSpec, Fonts, Layout, MarkerKind, Rect};
use mouli_web::style::{compute, BorderStyle, Gradient, Viewport};

/// Measures text with the embedded faces, for layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebFonts;

fn face_for(f: &FontSpec) -> &'static Face {
    if f.mono {
        typeface::mono()
    } else if f.bold {
        typeface::ui_medium()
    } else {
        typeface::ui()
    }
}

fn scale_of(f: &FontSpec, face: &Face) -> f32 {
    f.size.max(1.0) / face.size.max(1) as f32
}

impl Fonts for WebFonts {
    fn text_width(&self, text: &str, f: &FontSpec) -> f32 {
        let face = face_for(f);
        let k = scale_of(f, face);
        text.chars()
            .map(|c| face.glyph(c).advance as f32 * k + f.letter_spacing)
            .sum()
    }

    fn ascent(&self, f: &FontSpec) -> f32 {
        let face = face_for(f);
        face.ascent as f32 * scale_of(f, face)
    }

    fn descent(&self, f: &FontSpec) -> f32 {
        let face = face_for(f);
        (face.line_height - face.ascent).max(0) as f32 * scale_of(f, face)
    }

    fn normal_line_height(&self, f: &FontSpec) -> f32 {
        let face = face_for(f);
        face.line_height as f32 * scale_of(f, face)
    }
}

// ------------------------------------------------------------------ the surface

struct Surface<'a> {
    c: &'a mut Canvas,
    clips: Vec<(i32, i32, i32, i32)>,
    ox: i32,
    oy: i32,
}

fn alpha_of(color: u32) -> f32 {
    (color >> 24) as f32 / 255.0
}

impl Surface<'_> {
    fn clip(&self) -> (i32, i32, i32, i32) {
        self.clips
            .last()
            .copied()
            .unwrap_or((0, 0, self.c.width as i32, self.c.height as i32))
    }

    fn plot(&mut self, x: i32, y: i32, color: u32, cov: f32) {
        let (x0, y0, x1, y1) = self.clip();
        if x < x0
            || x >= x1
            || y < y0
            || y >= y1
            || x < 0
            || y < 0
            || x as usize >= self.c.width
            || y as usize >= self.c.height
        {
            return;
        }
        let a = (alpha_of(color) * cov.clamp(0.0, 1.0) * 255.0).round() as u32;
        if a == 0 {
            return;
        }
        let p = &mut self.c.pixels[y as usize * self.c.width + x as usize];
        *p = blend(*p, color & 0x00FF_FFFF, a.min(255) as u8);
    }

    fn rect_px(&mut self, r: Rect, color: u32) {
        let x0 = (r.x + self.ox as f32).round() as i32;
        let y0 = (r.y + self.oy as f32).round() as i32;
        let mut x1 = (r.x + r.w + self.ox as f32).round() as i32;
        let mut y1 = (r.y + r.h + self.oy as f32).round() as i32;
        if r.w > 0.0 && x1 == x0 {
            x1 += 1;
        }
        if r.h > 0.0 && y1 == y0 {
            y1 += 1;
        }
        let (cx0, cy0, cx1, cy1) = self.clip();
        for y in y0.max(cy0)..y1.min(cy1) {
            for x in x0.max(cx0)..x1.min(cx1) {
                self.plot(x, y, color, 1.0);
            }
        }
    }

    /// Coverage (0 to 1) of the pixel centred at (`px`, `py`) by a rounded rectangle.
    fn rr_cov(px: f32, py: f32, r: Rect, radii: [f32; 4]) -> f32 {
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (hx, hy) = (r.w / 2.0, r.h / 2.0);
        let rad = match (px >= cx, py >= cy) {
            (false, false) => radii[0],
            (true, false) => radii[1],
            (true, true) => radii[2],
            (false, true) => radii[3],
        }
        .min(hx.min(hy));
        let qx = (px - cx).abs() - hx + rad;
        let qy = (py - cy).abs() - hy + rad;
        let sdf = qx.max(qy).min(0.0) + qx.max(0.0).hypot(qy.max(0.0)) - rad;
        (0.5 - sdf).clamp(0.0, 1.0)
    }

    fn fill_rounded(
        &mut self,
        r: Rect,
        radii: [f32; 4],
        mut colour_at: impl FnMut(f32, f32) -> u32,
    ) {
        let r = Rect {
            x: r.x + self.ox as f32,
            y: r.y + self.oy as f32,
            w: r.w,
            h: r.h,
        };
        let (cx0, cy0, cx1, cy1) = self.clip();
        let x0 = (r.x.floor() as i32).max(cx0);
        let x1 = ((r.x + r.w).ceil() as i32).min(cx1);
        let y0 = (r.y.floor() as i32).max(cy0);
        let y1 = ((r.y + r.h).ceil() as i32).min(cy1);
        for y in y0..y1 {
            for x in x0..x1 {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let cov = Self::rr_cov(fx, fy, r, radii);
                if cov > 0.0 {
                    let col = colour_at(fx - self.ox as f32, fy - self.oy as f32);
                    self.plot(x, y, col, cov);
                }
            }
        }
    }

    fn fill(&mut self, r: Rect, color: u32, radii: [f32; 4]) {
        if color >> 24 == 0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        if radii.iter().all(|v| *v <= 0.0) {
            self.rect_px(r, color);
        } else {
            self.fill_rounded(r, radii, |_, _| color);
        }
    }

    fn gradient(&mut self, r: Rect, g: &Gradient, radii: [f32; 4]) {
        let theta = g.angle.to_radians();
        let (dx, dy) = (theta.sin(), -theta.cos());
        let len = (r.w * dx.abs() + r.h * dy.abs()).max(1.0);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let stops = g.stops.clone();
        self.fill_rounded(r, radii, move |px, py| {
            let t = (((px - cx) * dx + (py - cy) * dy) / len + 0.5).clamp(0.0, 1.0);
            gradient_colour(&stops, t)
        });
    }

    fn border(
        &mut self,
        r: Rect,
        widths: [f32; 4],
        styles: [BorderStyle; 4],
        colors: [u32; 4],
        radii: [f32; 4],
    ) {
        let w = |i: usize| {
            if styles[i] == BorderStyle::None {
                0.0
            } else {
                widths[i]
            }
        };
        let uniform = (0..4).all(|i| {
            (w(i) - w(0)).abs() < 0.01 && colors[i] == colors[0] && styles[i] == styles[0]
        });
        if uniform && radii.iter().any(|v| *v > 0.0) && w(0) > 0.0 {
            // A ring between the outer and the inner rounded rectangle.
            let bw = w(0);
            let inner = Rect {
                x: r.x + bw,
                y: r.y + bw,
                w: (r.w - 2.0 * bw).max(0.0),
                h: (r.h - 2.0 * bw).max(0.0),
            };
            let inner_r = [
                (radii[0] - bw).max(0.0),
                (radii[1] - bw).max(0.0),
                (radii[2] - bw).max(0.0),
                (radii[3] - bw).max(0.0),
            ];
            let (ox, oy) = (self.ox as f32, self.oy as f32);
            let (outer_abs, inner_abs) = (
                Rect {
                    x: r.x + ox,
                    y: r.y + oy,
                    ..r
                },
                Rect {
                    x: inner.x + ox,
                    y: inner.y + oy,
                    ..inner
                },
            );
            let color = colors[0];
            let shifted = Rect {
                x: r.x + ox,
                y: r.y + oy,
                w: r.w,
                h: r.h,
            };
            let (cx0, cy0, cx1, cy1) = self.clip();
            let (x0, x1) = (
                (shifted.x.floor() as i32).max(cx0),
                ((shifted.x + shifted.w).ceil() as i32).min(cx1),
            );
            let (y0, y1) = (
                (shifted.y.floor() as i32).max(cy0),
                ((shifted.y + shifted.h).ceil() as i32).min(cy1),
            );
            for y in y0..y1 {
                for x in x0..x1 {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let outer = Self::rr_cov(fx, fy, outer_abs, radii);
                    let inn = if inner.w > 0.0 && inner.h > 0.0 {
                        Self::rr_cov(fx, fy, inner_abs, inner_r)
                    } else {
                        0.0
                    };
                    let cov = (outer - inn).max(0.0);
                    if cov > 0.0 {
                        self.plot(x, y, color, cov);
                    }
                }
            }
            return;
        }
        let strips = [
            (
                Rect {
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: w(0),
                },
                0usize,
                true,
            ),
            (
                Rect {
                    x: r.x + r.w - w(1),
                    y: r.y + w(0),
                    w: w(1),
                    h: (r.h - w(0) - w(2)).max(0.0),
                },
                1,
                false,
            ),
            (
                Rect {
                    x: r.x,
                    y: r.y + r.h - w(2),
                    w: r.w,
                    h: w(2),
                },
                2,
                true,
            ),
            (
                Rect {
                    x: r.x,
                    y: r.y + w(0),
                    w: w(3),
                    h: (r.h - w(0) - w(2)).max(0.0),
                },
                3,
                false,
            ),
        ];
        for (strip, i, horizontal) in strips {
            if strip.w <= 0.0 || strip.h <= 0.0 {
                continue;
            }
            match styles[i] {
                BorderStyle::None => {}
                BorderStyle::Solid => self.rect_px(strip, colors[i]),
                BorderStyle::Dashed | BorderStyle::Dotted => {
                    let thick = if horizontal { strip.h } else { strip.w };
                    let (dash, gap) = if styles[i] == BorderStyle::Dashed {
                        (thick * 3.0, thick * 2.0)
                    } else {
                        (thick, thick)
                    };
                    let total = if horizontal { strip.w } else { strip.h };
                    let mut at = 0.0;
                    while at < total {
                        let len = dash.min(total - at);
                        let piece = if horizontal {
                            Rect {
                                x: strip.x + at,
                                w: len,
                                ..strip
                            }
                        } else {
                            Rect {
                                y: strip.y + at,
                                h: len,
                                ..strip
                            }
                        };
                        self.rect_px(piece, colors[i]);
                        at += dash + gap;
                    }
                }
                BorderStyle::Double => {
                    let third = (if horizontal { strip.h } else { strip.w }) / 3.0;
                    if horizontal {
                        self.rect_px(Rect { h: third, ..strip }, colors[i]);
                        self.rect_px(
                            Rect {
                                y: strip.y + 2.0 * third,
                                h: third,
                                ..strip
                            },
                            colors[i],
                        );
                    } else {
                        self.rect_px(Rect { w: third, ..strip }, colors[i]);
                        self.rect_px(
                            Rect {
                                x: strip.x + 2.0 * third,
                                w: third,
                                ..strip
                            },
                            colors[i],
                        );
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn glyph(
        &mut self,
        face: &Face,
        ch: char,
        pen_x: f32,
        baseline: f32,
        k: f32,
        color: u32,
        shear: f32,
    ) {
        let g = face.glyph(ch);
        if g.w == 0 || g.h == 0 {
            return;
        }
        let cov = face.coverage(g);
        let left = pen_x + self.ox as f32 + g.left as f32 * k;
        let top = baseline + self.oy as f32 - g.top as f32 * k;
        let (gw, gh) = (g.w as f32 * k, g.h as f32 * k);
        let slant = shear * gh;
        let (x0, x1) = (
            (left - 1.0).floor() as i32,
            (left + gw + slant.abs() + 1.0).ceil() as i32,
        );
        let (y0, y1) = (top.floor() as i32, (top + gh).ceil() as i32);
        let sample = |sx: f32, sy: f32| -> f32 {
            let (fx, fy) = (sx - 0.5, sy - 0.5);
            let (ix, iy) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - ix, fy - iy);
            let get = |x: i32, y: i32| -> f32 {
                if x < 0 || y < 0 || x >= g.w || y >= g.h {
                    0.0
                } else {
                    f32::from(cov[(y * g.w + x) as usize]) / 255.0
                }
            };
            let (ix, iy) = (ix as i32, iy as i32);
            get(ix, iy) * (1.0 - tx) * (1.0 - ty)
                + get(ix + 1, iy) * tx * (1.0 - ty)
                + get(ix, iy + 1) * (1.0 - tx) * ty
                + get(ix + 1, iy + 1) * tx * ty
        };
        for y in y0..y1 {
            // Italic: rows higher up shift right.
            let rise = 1.0 - ((y as f32 + 0.5 - top) / gh.max(1.0)).clamp(0.0, 1.0);
            let shift = slant * rise;
            for x in x0..x1 {
                let sx = (x as f32 + 0.5 - left - shift) / k;
                let sy = (y as f32 + 0.5 - top) / k;
                let a = sample(sx, sy);
                if a > 0.0 {
                    self.plot(x, y, color, a);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        x: f32,
        baseline: f32,
        text: &str,
        f: &FontSpec,
        color: u32,
        underline: bool,
        strike: bool,
    ) {
        let face = face_for(f);
        let k = scale_of(f, face);
        let shear = if f.italic { 0.2 } else { 0.0 };
        let mut pen = x;
        for ch in text.chars() {
            self.glyph(face, ch, pen, baseline, k, color, shear);
            if f.bold && !f.mono {
                self.glyph(face, ch, pen + 0.3 * k.max(0.7), baseline, k, color, shear);
            }
            pen += face.glyph(ch).advance as f32 * k + f.letter_spacing;
        }
        let thick = (f.size / 14.0).max(1.0);
        if underline {
            self.rect_px(
                Rect {
                    x,
                    y: baseline + f.size * 0.12,
                    w: pen - x,
                    h: thick,
                },
                color,
            );
        }
        if strike {
            self.rect_px(
                Rect {
                    x,
                    y: baseline - f.size * 0.3,
                    w: pen - x,
                    h: thick,
                },
                color,
            );
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, radius: f32, color: u32, ring: Option<f32>) {
        let r = Rect {
            x: cx - radius,
            y: cy - radius,
            w: radius * 2.0,
            h: radius * 2.0,
        };
        match ring {
            None => self.fill(r, color, [radius; 4]),
            Some(wd) => self.border(r, [wd; 4], [BorderStyle::Solid; 4], [color; 4], [radius; 4]),
        }
    }
}

fn gradient_colour(stops: &[(f32, u32)], t: f32) -> u32 {
    let Some(first) = stops.first() else { return 0 };
    if t <= first.0 {
        return first.1;
    }
    for w in stops.windows(2) {
        let ((p0, c0), (p1, c1)) = (w[0], w[1]);
        if t <= p1 {
            let u = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 };
            let mix = |shift: u32| {
                let (a, b) = (((c0 >> shift) & 0xFF) as f32, ((c1 >> shift) & 0xFF) as f32);
                ((a + (b - a) * u).round() as u32) << shift
            };
            return mix(24) | mix(16) | mix(8) | mix(0);
        }
    }
    stops.last().map_or(0, |s| s.1)
}

/// Draws a display list onto `canvas`, with the page's top-left at (`x`, `y`) and everything
/// clipped to `clip` (`x`, `y`, `w`, `h` in canvas pixels).
pub fn draw(canvas: &mut Canvas, ops: &[Op], x: i32, y: i32, clip: (i32, i32, i32, i32)) {
    let (cw, ch) = (canvas.width as i32, canvas.height as i32);
    let base = (
        clip.0.max(0),
        clip.1.max(0),
        (clip.0 + clip.2).min(cw),
        (clip.1 + clip.3).min(ch),
    );
    let mut s = Surface {
        c: canvas,
        clips: vec![base],
        ox: x,
        oy: y,
    };
    for op in ops {
        match op {
            Op::Rect { r, color, radii } => s.fill(*r, *color, *radii),
            Op::Gradient { r, g, radii } => s.gradient(*r, g, *radii),
            Op::Border {
                r,
                widths,
                styles,
                colors,
                radii,
            } => s.border(*r, *widths, *styles, *colors, *radii),
            Op::Text {
                x,
                baseline,
                text,
                font,
                color,
                underline,
                strike,
            } => s.text(*x, *baseline, text, font, *color, *underline, *strike),
            Op::Marker {
                kind,
                x,
                baseline,
                font,
                color,
            } => {
                let sz = font.size;
                match kind {
                    MarkerKind::Disc => s.circle(
                        *x + sz * 0.2,
                        *baseline - sz * 0.32,
                        sz * 0.17,
                        *color,
                        None,
                    ),
                    MarkerKind::Circle => s.circle(
                        *x + sz * 0.2,
                        *baseline - sz * 0.32,
                        sz * 0.17,
                        *color,
                        Some(1.0),
                    ),
                    MarkerKind::Square => {
                        s.fill(
                            Rect {
                                x: *x + sz * 0.05,
                                y: *baseline - sz * 0.5,
                                w: sz * 0.3,
                                h: sz * 0.3,
                            },
                            *color,
                            [0.0; 4],
                        );
                    }
                    MarkerKind::Text(t) => s.text(*x, *baseline, t, font, *color, false, false),
                }
            }
            Op::Placeholder {
                r,
                label,
                font,
                color,
            } => {
                s.fill(*r, 0xFFEE_EEEE, [0.0; 4]);
                s.border(
                    *r,
                    [1.0; 4],
                    [BorderStyle::Solid; 4],
                    [0xFFBB_BBBB; 4],
                    [0.0; 4],
                );
                let f = FontSpec {
                    size: font.size.clamp(9.0, 13.0),
                    bold: false,
                    italic: false,
                    mono: false,
                    letter_spacing: 0.0,
                };
                s.clips.push(clip_to(&s, *r));
                let line = WebFonts.normal_line_height(&f);
                s.text(
                    r.x + 4.0,
                    r.y + 3.0 + WebFonts.ascent(&f),
                    label,
                    &f,
                    scale_alpha(*color, 0.7),
                    false,
                    false,
                );
                let _ = line;
                s.clips.pop();
            }
            Op::Control {
                r,
                kind,
                label,
                checked,
                font,
                color,
            } => control(&mut s, *r, kind, label, *checked, font, *color),
            Op::PushClip(r) => {
                let c = clip_to(&s, *r);
                s.clips.push(c);
            }
            Op::PopClip => {
                if s.clips.len() > 1 {
                    s.clips.pop();
                }
            }
        }
    }
}

fn scale_alpha(color: u32, k: f32) -> u32 {
    (((color >> 24) as f32 * k).round() as u32) << 24 | (color & 0x00FF_FFFF)
}

fn clip_to(s: &Surface<'_>, r: Rect) -> (i32, i32, i32, i32) {
    let (cx0, cy0, cx1, cy1) = s.clip();
    let x0 = (r.x + s.ox as f32).round() as i32;
    let y0 = (r.y + s.oy as f32).round() as i32;
    let x1 = (r.x + r.w + s.ox as f32).round() as i32;
    let y1 = (r.y + r.h + s.oy as f32).round() as i32;
    (
        x0.max(cx0),
        y0.max(cy0),
        x1.min(cx1).max(x0.max(cx0)),
        y1.min(cy1).max(y0.max(cy0)),
    )
}

fn control(
    s: &mut Surface<'_>,
    r: Rect,
    kind: &str,
    label: &str,
    checked: bool,
    font: &FontSpec,
    color: u32,
) {
    let dark = 0xFF20_2020;
    match kind {
        "checkbox" => {
            s.fill(r, 0xFFFF_FFFF, [2.0; 4]);
            s.border(
                r,
                [1.0; 4],
                [BorderStyle::Solid; 4],
                [0xFF76_7676; 4],
                [2.0; 4],
            );
            if checked {
                s.fill(
                    Rect {
                        x: r.x + 3.0,
                        y: r.y + 3.0,
                        w: r.w - 6.0,
                        h: r.h - 6.0,
                    },
                    0xFF00_6CBE,
                    [1.0; 4],
                );
            }
        }
        "radio" => {
            let (cx, cy, rad) = (r.x + r.w / 2.0, r.y + r.h / 2.0, r.w.min(r.h) / 2.0);
            s.circle(cx, cy, rad, 0xFFFF_FFFF, None);
            s.circle(cx, cy, rad, 0xFF76_7676, Some(1.0));
            if checked {
                s.circle(cx, cy, rad * 0.5, 0xFF00_6CBE, None);
            }
        }
        _ => {
            let f = FontSpec {
                size: font.size,
                bold: false,
                italic: false,
                mono: font.mono,
                letter_spacing: 0.0,
            };
            let asc = WebFonts.ascent(&f);
            let lh = WebFonts.normal_line_height(&f);
            let clip = clip_to(s, r);
            s.clips.push(clip);
            let text_colour = if label.is_empty() {
                0xFF99_9999
            } else if color >> 24 == 0 {
                dark
            } else {
                color
            };
            match kind {
                "button" => {
                    let w = WebFonts.text_width(label, &f);
                    s.text(
                        r.x + (r.w - w) / 2.0,
                        r.y + (r.h - lh) / 2.0 + asc,
                        label,
                        &f,
                        text_colour,
                        false,
                        false,
                    );
                }
                "textarea" => {
                    for (i, line) in label.lines().take(20).enumerate() {
                        s.text(
                            r.x + 4.0,
                            r.y + 3.0 + asc + i as f32 * lh,
                            line,
                            &f,
                            text_colour,
                            false,
                            false,
                        );
                    }
                }
                _ => {
                    s.text(
                        r.x + 6.0,
                        r.y + (r.h - lh) / 2.0 + asc,
                        label,
                        &f,
                        text_colour,
                        false,
                        false,
                    );
                    if kind == "select" {
                        // A small down arrow at the right edge.
                        let (ax, ay) = (r.x + r.w - 14.0, r.y + r.h / 2.0 - 2.0);
                        for row in 0..4 {
                            let w = 8.0 - row as f32 * 2.0;
                            s.fill(
                                Rect {
                                    x: ax + row as f32,
                                    y: ay + row as f32,
                                    w,
                                    h: 1.0,
                                },
                                dark,
                                [0.0; 4],
                            );
                        }
                    }
                }
            }
            s.clips.pop();
        }
    }
}

// ------------------------------------------------------------------ pages

/// A loaded page: its document, layout and size, ready to be scrolled, drawn and clicked.
#[derive(Debug, Clone)]
pub struct Page {
    dom: Dom,
    layout: Layout,
    viewport: Viewport,
}

impl Page {
    /// Parses `html` and lays it out for a window `w` pixels wide and `h` tall.
    #[must_use]
    pub fn load(html: &str, w: f32, h: f32) -> Self {
        let dom = Dom::parse(html);
        let viewport = Viewport {
            w: w.max(50.0),
            h: h.max(50.0),
        };
        let layout = Self::lay(&dom, viewport, &State::default());
        Page {
            dom,
            layout,
            viewport,
        }
    }

    fn lay(dom: &Dom, vp: Viewport, st: &State) -> Layout {
        let sheets: Vec<Stylesheet> = dom
            .style_sheets()
            .iter()
            .map(|s| Stylesheet::parse(s))
            .collect();
        let styles = compute(dom, &sheets, vp, st);
        layout(dom, &styles, &WebFonts, vp)
    }

    /// Lays the page out again for a new window size (after the window is resized).
    pub fn resize(&mut self, w: f32, h: f32) {
        self.viewport = Viewport {
            w: w.max(50.0),
            h: h.max(50.0),
        };
        self.layout = Self::lay(&self.dom, self.viewport, &State::default());
    }

    /// Total height of the page in pixels.
    #[must_use]
    pub fn height(&self) -> f32 {
        self.layout.height
    }

    /// The document title.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        self.dom.title()
    }

    /// The document.
    #[must_use]
    pub fn dom(&self) -> &Dom {
        &self.dom
    }

    /// The largest useful scroll offset for the current window height.
    #[must_use]
    pub fn max_scroll(&self) -> f32 {
        (self.layout.height - self.viewport.h).max(0.0)
    }

    /// Draws the page, scrolled by `scroll_y`, into the rectangle (`x`, `y`, `w`, `h`) of `canvas`.
    pub fn draw(&self, canvas: &mut Canvas, x: i32, y: i32, w: i32, h: i32, scroll_y: f32) {
        let ops = paint(
            &self.layout,
            scroll_y.clamp(0.0, self.max_scroll()),
            Rect {
                x: 0.0,
                y: 0.0,
                w: w as f32,
                h: h as f32,
            },
        );
        draw(canvas, &ops, x, y, (x, y, w, h));
    }

    /// The element under a point of the window (`x`, `y` relative to the window's top-left).
    #[must_use]
    pub fn element_at(&self, x: f32, y: f32, scroll_y: f32) -> Option<NodeId> {
        hit_test(&self.layout, x, y + scroll_y)
    }

    /// The page position (top edge) of the element with this `id`, for `#anchor` links.
    #[must_use]
    pub fn anchor_y(&self, id: &str) -> Option<f32> {
        let n = self.dom.by_id(id).or_else(|| {
            self.dom
                .find_all("a")
                .into_iter()
                .find(|&a| self.dom.attr(a, "name") == Some(id))
        })?;
        node_rect(&self.layout, n).map(|r| r.y)
    }

    /// The `href` of the link under a point, if any.
    #[must_use]
    pub fn link_at(&self, x: f32, y: f32, scroll_y: f32) -> Option<String> {
        let n = self.element_at(x, y, scroll_y)?;
        self.dom
            .ancestry(n)
            .into_iter()
            .rev()
            .find(|&a| self.dom.tag(a) == Some("a"))
            .and_then(|a| self.dom.attr(a, "href"))
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: usize, h: usize) -> Canvas {
        Canvas {
            width: w,
            height: h,
            pixels: vec![0x00FF_FFFF; w * h],
        }
    }

    fn page_canvas(html: &str, w: usize, h: usize) -> (Page, Canvas) {
        let p = Page::load(html, w as f32, h as f32);
        let mut c = canvas(w, h);
        p.draw(&mut c, 0, 0, w as i32, h as i32, 0.0);
        (p, c)
    }

    fn px(c: &Canvas, x: usize, y: usize) -> u32 {
        c.get(x, y).unwrap()
    }

    fn ink(c: &Canvas, x0: usize, y0: usize, x1: usize, y1: usize) -> usize {
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| px(c, x, y) != 0x00FF_FFFF)
            .count()
    }

    #[test]
    fn backgrounds_borders_and_the_canvas_colour_are_drawn() {
        let html = r#"<body style="background:#102030;margin:0"><div style="margin:10px;width:60px;height:40px;background:#ff0000;border:3px solid #00ff00"></div></body>"#;
        let (_, c) = page_canvas(html, 100, 80);
        assert_eq!(
            px(&c, 2, 2),
            0x0010_2030,
            "the page background fills the window"
        );
        assert_eq!(px(&c, 40, 30), 0x00FF_0000, "the box");
        assert_eq!(px(&c, 11, 30), 0x0000_FF00, "the left border");
        assert_eq!(px(&c, 40, 11), 0x0000_FF00, "the top border");
        assert_eq!(px(&c, 80, 30), 0x0010_2030, "right of the box");
    }

    #[test]
    fn rounded_corners_are_smooth_and_gradients_blend() {
        let html = r#"<body style="margin:0"><div style="width:40px;height:40px;background:#000;border-radius:20px"></div></body>"#;
        let (_, c) = page_canvas(html, 60, 60);
        assert_eq!(px(&c, 0, 0), 0x00FF_FFFF, "the corner is cut away");
        assert_eq!(px(&c, 20, 20), 0x0000_0000, "the middle is filled");
        let edge = px(&c, 2, 10);
        assert!(
            edge != 0 && edge != 0x00FF_FFFF,
            "the edge is anti-aliased: {edge:06x}"
        );
        let html = r#"<body style="margin:0"><div style="width:100px;height:10px;background:linear-gradient(to right,#000000,#ffffff)"></div></body>"#;
        let (_, c) = page_canvas(html, 100, 20);
        let (l, m, r) = (
            px(&c, 2, 5) & 0xFF,
            px(&c, 50, 5) & 0xFF,
            px(&c, 97, 5) & 0xFF,
        );
        assert!(l < 20 && (115..140).contains(&m) && r > 235, "{l} {m} {r}");
    }

    #[test]
    fn text_is_drawn_scaled_bold_and_italic() {
        let size_ink = |css: &str| {
            let html =
                format!(r#"<body style="margin:0"><p style="margin:0;{css}">Hamburg</p></body>"#);
            let (_, c) = page_canvas(&html, 200, 80);
            ink(&c, 0, 0, 200, 80)
        };
        let small = size_ink("font-size:14px");
        let big = size_ink("font-size:28px");
        assert!(small > 100, "text appears: {small}");
        assert!(
            big > 3 * small,
            "double the size is much more ink: {small} {big}"
        );
        assert!(
            size_ink("font-size:14px;font-weight:bold") > small,
            "bold has more ink"
        );
        let html = r#"<body style="margin:0;color:#ff0000"><p style="margin:0;font-size:20px">Red</p></body>"#;
        let (_, c) = page_canvas(html, 100, 40);
        let reddish = (0..100)
            .flat_map(|x| (0..40).map(move |y| (x, y)))
            .any(|(x, y)| {
                let p = px(&c, x, y);
                (p >> 16) & 0xFF > 200 && (p & 0xFF) < 60
            });
        assert!(reddish, "text takes its colour");
    }

    #[test]
    fn underline_strike_and_lists_draw_extra_marks() {
        let plain = {
            let (_, c) = page_canvas(
                r#"<body style="margin:0"><p style="margin:0;font-size:20px">iiii</p></body>"#,
                80,
                40,
            );
            ink(&c, 0, 0, 80, 40)
        };
        let under = {
            let (_, c) = page_canvas(
                r#"<body style="margin:0"><p style="margin:0;font-size:20px;text-decoration:underline">iiii</p></body>"#,
                80,
                40,
            );
            ink(&c, 0, 0, 80, 40)
        };
        assert!(under > plain + 10, "{plain} {under}");
        let (_, c) = page_canvas(
            "<body style=\"margin:0\"><ul style=\"margin:0\"><li>x</li></ul></body>",
            80,
            30,
        );
        // The bullet sits left of the text, inside the list's padding.
        assert!(ink(&c, 15, 0, 40, 30) > 5);
    }

    #[test]
    fn overflow_hidden_really_clips() {
        let html = r#"<body style="margin:0"><div style="width:30px;height:30px;overflow:hidden"><div style="width:100px;height:100px;background:#000"></div></div></body>"#;
        let (_, c) = page_canvas(html, 100, 100);
        assert_eq!(px(&c, 10, 10), 0, "inside the clip");
        assert_eq!(px(&c, 50, 10), 0x00FF_FFFF, "outside the clip");
        assert_eq!(px(&c, 10, 50), 0x00FF_FFFF);
    }

    #[test]
    fn drawing_into_part_of_a_canvas_stays_inside_it() {
        let p = Page::load(
            r#"<body style="margin:0;background:#00f"><p>x</p></body>"#,
            40.0,
            40.0,
        );
        let mut c = canvas(100, 100);
        p.draw(&mut c, 30, 30, 40, 40, 0.0);
        assert_eq!(px(&c, 35, 35), 0x0000_00FF);
        assert_eq!(
            px(&c, 10, 10),
            0x00FF_FFFF,
            "outside the window nothing is drawn"
        );
        assert_eq!(px(&c, 75, 75), 0x00FF_FFFF);
        // A window partly off the canvas does not panic.
        p.draw(&mut c, 80, 80, 40, 40, 0.0);
        p.draw(&mut c, -20, -20, 40, 40, 0.0);
    }

    #[test]
    fn scrolling_shows_lower_content_and_is_limited() {
        let html = format!(
            r#"<body style="margin:0"><div style="height:200px"></div><div style="height:50px;background:#f00"></div>{}"#,
            ""
        );
        let p = Page::load(&html, 50.0, 100.0);
        assert!((p.height() - 250.0).abs() < 0.5);
        assert!((p.max_scroll() - 150.0).abs() < 0.5);
        let mut c = canvas(50, 100);
        p.draw(&mut c, 0, 0, 50, 100, 0.0);
        assert_eq!(px(&c, 10, 10), 0x00FF_FFFF);
        p.draw(&mut c, 0, 0, 50, 100, 150.0);
        assert_eq!(px(&c, 10, 60), 0x00FF_0000, "the red block is now in view");
        p.draw(&mut c, 0, 0, 50, 100, 9999.0);
        assert_eq!(
            px(&c, 10, 60),
            0x00FF_0000,
            "scrolling is clamped to the end"
        );
    }

    #[test]
    fn links_are_found_by_position() {
        let p = Page::load(
            r#"<body style="margin:0"><p style="margin:0;font-size:20px">go <a href="/next">here</a> now</p></body>"#,
            200.0,
            100.0,
        );
        assert_eq!(p.link_at(5.0, 10.0, 0.0), None, "plain text");
        let wid = |s: &str| {
            WebFonts.text_width(
                s,
                &FontSpec {
                    size: 20.0,
                    bold: false,
                    italic: false,
                    mono: false,
                    letter_spacing: 0.0,
                },
            )
        };
        let x = wid("go ") + 4.0;
        assert_eq!(p.link_at(x, 10.0, 0.0).as_deref(), Some("/next"));
        assert_eq!(p.link_at(wid("go here now") + 30.0, 10.0, 0.0), None);
        assert_eq!(p.title(), None);
    }

    #[test]
    fn a_page_resizes_and_fonts_measure_consistently() {
        let mut p = Page::load("<body style=\"margin:0\"><p style=\"margin:0\">aaaa bbbb cccc dddd eeee ffff gggg hhhh</p></body>", 600.0, 100.0);
        let tall_before = p.height();
        p.resize(80.0, 100.0);
        assert!(
            p.height() > tall_before,
            "narrower means taller: {tall_before} {}",
            p.height()
        );
        let f = FontSpec {
            size: 14.0,
            bold: false,
            italic: false,
            mono: false,
            letter_spacing: 0.0,
        };
        let w1 = WebFonts.text_width("hello", &f);
        let w2 = WebFonts.text_width("hello", &FontSpec { size: 28.0, ..f });
        assert!((w2 / w1 - 2.0).abs() < 0.01, "width scales with size");
        assert!(WebFonts.text_width("hello", &FontSpec { bold: true, ..f }) >= w1 * 0.95);
        assert!(WebFonts.ascent(&f) > 0.0 && WebFonts.descent(&f) > 0.0);
        assert!(
            WebFonts.text_width(
                "ab",
                &FontSpec {
                    letter_spacing: 2.0,
                    ..f
                }
            ) > WebFonts.text_width("ab", &f)
        );
        let mono = FontSpec { mono: true, ..f };
        assert!(
            (WebFonts.text_width("iiii", &mono) - WebFonts.text_width("mmmm", &mono)).abs() < 0.01,
            "monospace"
        );
    }

    #[test]
    fn controls_placeholders_and_odd_input_are_drawn_without_panicking() {
        let html = r#"<body><input value="hello"><input type=checkbox checked><input type=radio checked><button>Go</button><select><option>One</option></select><textarea>a
b</textarea><img alt="logo" width=60 height=30><div style="border:3px dashed red;border-radius:6px;padding:4px">x</div><div style="border:6px double blue">d</div><div style="border:2px dotted green;border-top-style:solid">e</div></body>"#;
        let (_, c) = page_canvas(html, 300, 300);
        assert!(ink(&c, 0, 0, 300, 300) > 500);
        for odd in ["", "<", "<div style=\"width:99999px;height:99999px;border-radius:99999px;background:red\"></div>", "<p style=\"font-size:0\">x</p><p style=\"font-size:5000px\">y</p>"] {
            let (_, c) = page_canvas(odd, 50, 50);
            assert_eq!(c.pixels.len(), 2500);
        }
    }
}
