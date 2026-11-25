//! The display list: what to draw, in what order, from a laid-out page.
//!
//! Painting itself (pixels, fonts) is done by the screen; this turns boxes into a flat list of
//! simple operations in *viewport* coordinates (the page scrolled by `scroll_y`), culled to what
//! can be seen. Order follows the usual rules: a box's background and border, then its children
//! in order, then its lines of text; positioned boxes are painted afterwards by `z-index`;
//! `overflow` clips become push and pop operations; opacity is folded into the colours.

use crate::layout::{BoxKind, FontSpec, Item, LBox, Layout, MarkerKind, Rect, Replaced};
use crate::style::{BorderStyle, Gradient, Position, Style};

/// One drawing operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// A filled rectangle (rounded when the radii are not zero: top-left, top-right,
    /// bottom-right, bottom-left).
    Rect {
        /// Where.
        r: Rect,
        /// Colour `0xAARRGGBB`.
        color: u32,
        /// Corner radii.
        radii: [f32; 4],
    },
    /// A linear gradient.
    Gradient {
        /// Where.
        r: Rect,
        /// The gradient.
        g: Gradient,
        /// Corner radii.
        radii: [f32; 4],
    },
    /// The borders of a box.
    Border {
        /// The border box.
        r: Rect,
        /// Widths: top, right, bottom, left.
        widths: [f32; 4],
        /// Styles.
        styles: [BorderStyle; 4],
        /// Colours.
        colors: [u32; 4],
        /// Corner radii.
        radii: [f32; 4],
    },
    /// A run of text.
    Text {
        /// Left edge.
        x: f32,
        /// Baseline.
        baseline: f32,
        /// The text.
        text: String,
        /// Its font.
        font: FontSpec,
        /// Colour.
        color: u32,
        /// Underline.
        underline: bool,
        /// Strike-through.
        strike: bool,
    },
    /// A list marker shape.
    Marker {
        /// Which shape or text.
        kind: MarkerKind,
        /// Left edge.
        x: f32,
        /// Baseline.
        baseline: f32,
        /// Font (for text markers and sizing).
        font: FontSpec,
        /// Colour.
        color: u32,
    },
    /// A picture; drawn from the decoded bitmap for `src` when there is one, otherwise as a
    /// labelled placeholder.
    Image {
        /// Where.
        r: Rect,
        /// The picture's `src` (the key of the bitmap).
        src: String,
        /// The alternative text, for the placeholder.
        label: String,
        /// The font for the label.
        font: FontSpec,
        /// Text colour.
        color: u32,
    },
    /// Other content we cannot show, drawn as a labelled placeholder.
    Placeholder {
        /// Where.
        r: Rect,
        /// What to write in it.
        label: String,
        /// The font for the label.
        font: FontSpec,
        /// Text colour.
        color: u32,
    },
    /// A form control.
    Control {
        /// Where.
        r: Rect,
        /// `text`, `button`, `checkbox`, `radio`, `select` or `textarea`.
        kind: String,
        /// The text shown.
        label: String,
        /// Checked.
        checked: bool,
        /// The font.
        font: FontSpec,
        /// Text colour.
        color: u32,
    },
    /// Start clipping to a rectangle.
    PushClip(Rect),
    /// Stop clipping.
    PopClip,
}

fn with_opacity(color: u32, o: f32) -> u32 {
    if o >= 1.0 {
        return color;
    }
    let a = ((color >> 24) as f32 * o.clamp(0.0, 1.0)).round() as u32;
    (a << 24) | (color & 0x00FF_FFFF)
}

fn font(s: &Style) -> FontSpec {
    FontSpec {
        size: s.font_size.max(1.0),
        bold: s.bold,
        italic: s.italic,
        mono: s.mono,
        letter_spacing: s.letter_spacing,
    }
}

struct Painter {
    ops: Vec<Op>,
    view: Rect,
    deferred: Vec<(i32, usize, Vec<Op>)>,
    count: usize,
}

/// Turns a layout into drawing operations for the part of the page that is visible.
///
/// `scroll_y` is how far the page is scrolled and `view` the size of the window (its `x` and `y`
/// are ignored). Fixed-position boxes do not scroll.
#[must_use]
pub fn paint(layout: &Layout, scroll_y: f32, view: Rect) -> Vec<Op> {
    let mut p = Painter {
        ops: Vec::new(),
        view: Rect {
            x: 0.0,
            y: 0.0,
            w: view.w,
            h: view.h,
        },
        deferred: Vec::new(),
        count: 0,
    };
    p.ops.push(Op::Rect {
        r: p.view,
        color: layout.canvas_color,
        radii: [0.0; 4],
    });
    p.box_ops(&layout.root, 0.0, -scroll_y, scroll_y, false);
    let mut deferred = std::mem::take(&mut p.deferred);
    deferred.sort_by_key(|(z, order, _)| (*z, *order));
    for (_, _, ops) in deferred {
        p.ops.extend(ops);
    }
    p.ops
}

impl Painter {
    fn visible(&self, r: Rect) -> bool {
        r.x < self.view.w && r.x + r.w > 0.0 && r.y < self.view.h && r.y + r.h > 0.0
    }

    fn radii(s: &Style, w: f32, h: f32) -> [f32; 4] {
        let mut r = [0.0; 4];
        for (i, d) in s.radius.iter().enumerate() {
            r[i] = d.resolve(w).clamp(0.0, w.min(h) / 2.0);
        }
        r
    }

    fn box_ops(&mut self, b: &LBox, ox: f32, oy: f32, scroll: f32, in_deferred: bool) {
        self.count += 1;
        if self.count > 200_000 {
            return;
        }
        let (x, y) = (ox + b.rect.x, oy + b.rect.y);
        let border_rect = Rect {
            x,
            y,
            w: b.rect.w,
            h: b.rect.h,
        };
        // Positioned boxes are painted later, in z order.
        if !in_deferred && (b.style.position != Position::Static || b.out_of_flow) {
            let order = self.deferred.len();
            let z = b.style.z_index;
            let saved = std::mem::take(&mut self.ops);
            let fixed_shift = if b.style.position == Position::Fixed {
                scroll
            } else {
                0.0
            };
            self.box_ops(b, ox, oy + fixed_shift, scroll, true);
            let mine = std::mem::replace(&mut self.ops, saved);
            self.deferred.push((z, order, mine));
            return;
        }
        let s = &b.style;
        let o = s.opacity;
        let radii = Self::radii(s, b.rect.w, b.rect.h);
        if !s.hidden && self.visible(border_rect) {
            if s.background >> 24 != 0 && b.node.is_some() {
                self.ops.push(Op::Rect {
                    r: border_rect,
                    color: with_opacity(s.background, o),
                    radii,
                });
            }
            if let Some(g) = &s.gradient {
                if b.node.is_some() {
                    let mut g = g.clone();
                    for st in &mut g.stops {
                        st.1 = with_opacity(st.1, o);
                    }
                    self.ops.push(Op::Gradient {
                        r: border_rect,
                        g,
                        radii,
                    });
                }
            }
            if b.border.iter().any(|w| *w > 0.0) {
                let mut colors = s.border_color;
                for c in &mut colors {
                    *c = with_opacity(*c, o);
                }
                self.ops.push(Op::Border {
                    r: border_rect,
                    widths: b.border,
                    styles: s.border_style,
                    colors,
                    radii,
                });
            }
            if let BoxKind::Replaced(rep) = &b.kind {
                self.replaced_ops(b, rep, border_rect);
            }
        }
        let clip = b.clips();
        let (cx, cy) = (x + b.content_x(), y + b.content_y());
        if clip {
            let pad = Rect {
                x: x + b.border[3],
                y: y + b.border[0],
                w: (b.rect.w - b.border[1] - b.border[3]).max(0.0),
                h: (b.rect.h - b.border[0] - b.border[2]).max(0.0),
            };
            self.ops.push(Op::PushClip(pad));
        }
        if !s.hidden {
            if let Some(m) = &b.marker {
                let by = cy + m.baseline;
                if by > -50.0 && by < self.view.h + 50.0 {
                    self.ops.push(Op::Marker {
                        kind: m.kind.clone(),
                        x: cx + m.x,
                        baseline: by,
                        font: m.font,
                        color: with_opacity(m.color, o),
                    });
                }
            }
        }
        for c in &b.children {
            let cr = Rect {
                x: x + c.rect.x,
                y: y + c.rect.y,
                w: c.rect.w,
                h: c.rect.h,
            };
            // Skip boxes far outside the window (their positioned descendants stay reachable
            // because `visible` is checked again for each of them).
            if c.style.position == Position::Static
                && !c.out_of_flow
                && (cr.y > self.view.h + 2000.0 || cr.y + cr.h < -2000.0)
            {
                continue;
            }
            self.box_ops(c, x, y, scroll, in_deferred);
        }
        if !s.hidden {
            for l in &b.lines {
                let ly = cy + l.y;
                if ly > self.view.h || ly + l.h < 0.0 {
                    continue;
                }
                // Backgrounds and borders of inline elements go under the text of the line.
                for it in &l.items {
                    if let Item::Span {
                        x: sx,
                        w,
                        style,
                        left_edge,
                        right_edge,
                        ..
                    } = it
                    {
                        self.span_ops(style, cx + sx, ly, *w, l.h, *left_edge, *right_edge);
                    }
                }
                for it in &l.items {
                    match it {
                        Item::Text {
                            x: tx,
                            w: _,
                            text,
                            font: f,
                            color,
                            underline,
                            strike,
                            dy,
                            ..
                        } => {
                            self.ops.push(Op::Text {
                                x: cx + tx,
                                baseline: ly + l.baseline + dy,
                                text: text.clone(),
                                font: *f,
                                color: with_opacity(*color, o),
                                underline: *underline,
                                strike: *strike,
                            });
                        }
                        Item::Atomic {
                            x: ax,
                            y: ay,
                            b: ab,
                        } => {
                            self.box_ops(ab, cx + ax, ly + ay, scroll, in_deferred);
                        }
                        Item::Span { .. } => {}
                    }
                }
            }
        }
        if clip {
            self.ops.push(Op::PopClip);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn span_ops(
        &mut self,
        s: &Style,
        x: f32,
        line_y: f32,
        w: f32,
        line_h: f32,
        left: bool,
        right: bool,
    ) {
        // The box of an inline element is as tall as its font, centred on the line.
        let f = font(s);
        let h = (f.size * 1.2
            + s.padding[0].px
            + s.padding[2].px
            + s.border_width[0]
            + s.border_width[2])
            .min(line_h.max(f.size));
        let y = line_y + (line_h - h) / 2.0;
        let r = Rect { x, y, w, h };
        if !self.visible(r) {
            return;
        }
        let o = s.opacity;
        if s.background >> 24 != 0 {
            self.ops.push(Op::Rect {
                r,
                color: with_opacity(s.background, o),
                radii: [0.0; 4],
            });
        }
        let mut widths = [
            s.border_width[0],
            if right { s.border_width[1] } else { 0.0 },
            s.border_width[2],
            if left { s.border_width[3] } else { 0.0 },
        ];
        for (i, st) in s.border_style.iter().enumerate() {
            if *st == BorderStyle::None {
                widths[i] = 0.0;
            }
        }
        if widths.iter().any(|v| *v > 0.0) {
            let mut colors = s.border_color;
            for c in &mut colors {
                *c = with_opacity(*c, o);
            }
            self.ops.push(Op::Border {
                r,
                widths,
                styles: s.border_style,
                colors,
                radii: [0.0; 4],
            });
        }
    }

    fn replaced_ops(&mut self, b: &LBox, rep: &Replaced, r: Rect) {
        let s = &b.style;
        let f = font(s);
        let color = with_opacity(s.color, s.opacity);
        match rep {
            Replaced::Image { alt, src } => {
                let label = if alt.is_empty() {
                    "image".to_string()
                } else {
                    alt.clone()
                };
                self.ops.push(Op::Image {
                    r,
                    src: src.clone(),
                    label,
                    font: f,
                    color,
                });
            }
            Replaced::Placeholder { label } => self.ops.push(Op::Placeholder {
                r,
                label: label.clone(),
                font: f,
                color,
            }),
            Replaced::Control {
                label,
                kind,
                checked,
            } => {
                self.ops.push(Op::Control {
                    r,
                    kind: kind.clone(),
                    label: label.clone(),
                    checked: *checked,
                    font: f,
                    color,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{State, Stylesheet};
    use crate::dom::Dom;
    use crate::layout::{layout, Fonts};
    use crate::style::{compute, Viewport};

    struct F;
    impl Fonts for F {
        fn text_width(&self, t: &str, f: &FontSpec) -> f32 {
            t.chars().count() as f32 * f.size / 2.0
        }
        fn ascent(&self, f: &FontSpec) -> f32 {
            f.size * 0.8
        }
        fn descent(&self, f: &FontSpec) -> f32 {
            f.size * 0.2
        }
        fn normal_line_height(&self, f: &FontSpec) -> f32 {
            f.size * 1.25
        }
    }

    fn ops(html: &str, css: &str, scroll: f32) -> Vec<Op> {
        let dom = Dom::parse(html);
        let mut sheets: Vec<Stylesheet> = dom
            .style_sheets()
            .iter()
            .map(|s| Stylesheet::parse(s))
            .collect();
        sheets.push(Stylesheet::parse(css));
        let vp = Viewport { w: 400.0, h: 300.0 };
        let styles = compute(&dom, &sheets, vp, &State::default());
        let l = layout(&dom, &styles, &F, vp);
        paint(
            &l,
            scroll,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 300.0,
            },
        )
    }

    fn texts(o: &[Op]) -> Vec<String> {
        o.iter()
            .filter_map(|x| {
                if let Op::Text { text, .. } = x {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn the_canvas_comes_first_then_backgrounds_and_text_in_order() {
        let o = ops(
            r#"<div style="background:red;height:20px">hi</div>"#,
            "body{margin:0}",
            0.0,
        );
        assert!(
            matches!(
                o[0],
                Op::Rect {
                    color: 0xFFFF_FFFF,
                    ..
                }
            ),
            "{:?}",
            o[0]
        );
        assert!(matches!(
            o[1],
            Op::Rect {
                color: 0xFFFF_0000,
                r: Rect { x: 0.0, y: 0.0, .. },
                ..
            }
        ));
        assert_eq!(texts(&o), vec!["hi"]);
        let bg = o
            .iter()
            .position(|x| {
                matches!(
                    x,
                    Op::Rect {
                        color: 0xFFFF_0000,
                        ..
                    }
                )
            })
            .unwrap();
        let tx = o.iter().position(|x| matches!(x, Op::Text { .. })).unwrap();
        assert!(bg < tx, "text is drawn over its background");
    }

    #[test]
    fn scrolling_moves_things_and_hides_what_is_off_screen() {
        let html = "<p>first</p><div style=\"height:1000px\"></div><p>last</p>";
        assert_eq!(texts(&ops(html, "body{margin:0}", 0.0)), vec!["first"]);
        let o = ops(html, "body{margin:0}", 1000.0);
        assert_eq!(texts(&o), vec!["last"]);
        let y = o
            .iter()
            .find_map(|x| {
                if let Op::Text { baseline, .. } = x {
                    Some(*baseline)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(y > 0.0 && y < 300.0, "scrolled into view: {y}");
    }

    #[test]
    fn borders_radii_gradients_and_opacity_are_carried() {
        let o = ops(
            r#"<div style="border:2px solid blue;border-radius:5px;background:linear-gradient(red,blue);opacity:0.5;height:20px;width:100px">x</div>"#,
            "body{margin:0}",
            0.0,
        );
        assert!(o.iter().any(|x| matches!(x, Op::Border { widths, radii, colors, .. } if *widths == [2.0; 4] && radii[0] == 5.0 && colors[0] >> 24 == 0x80)));
        assert!(o
            .iter()
            .any(|x| matches!(x, Op::Gradient { g, .. } if g.stops[0].1 >> 24 == 0x80)));
        assert!(
            o.iter()
                .any(|x| matches!(x, Op::Text { color, .. } if color >> 24 == 0x80)),
            "text fades with its element"
        );
    }

    #[test]
    fn overflow_hidden_clips_its_contents() {
        let o = ops(
            r#"<div style="overflow:hidden;height:10px;width:50px"><p>text</p></div>"#,
            "body{margin:0}",
            0.0,
        );
        let push = o.iter().position(|x| matches!(x, Op::PushClip(_))).unwrap();
        let pop = o.iter().position(|x| matches!(x, Op::PopClip)).unwrap();
        let tx = o.iter().position(|x| matches!(x, Op::Text { .. })).unwrap();
        assert!(push < tx && tx < pop);
        assert!(matches!(
            o[push],
            Op::PushClip(Rect {
                w: 50.0,
                h: 10.0,
                ..
            })
        ));
    }

    #[test]
    fn positioned_boxes_are_painted_after_normal_flow_in_z_order() {
        let css = "body{margin:0} .p{position:absolute;top:0;left:0;width:50px;height:50px}";
        let html = r#"<div class="p" style="z-index:2;background:#00f"></div><div class="p" style="z-index:1;background:#0f0"></div><div style="background:#f00;height:30px">flow</div>"#;
        let o = ops(html, css, 0.0);
        let at = |c: u32| {
            o.iter()
                .position(|x| matches!(x, Op::Rect { color, .. } if *color == c))
                .unwrap()
        };
        assert!(at(0xFFFF_0000) < at(0xFF00_FF00), "normal flow first");
        assert!(
            at(0xFF00_FF00) < at(0xFF00_00FF),
            "then z-index 1, then z-index 2"
        );
    }

    #[test]
    fn hidden_elements_and_list_markers_and_controls() {
        let o = ops(
            r#"<p style="visibility:hidden">secret</p><ul><li>item</li></ul><input value="v"><img alt="logo" width=20 height=10>"#,
            "body{margin:0}",
            0.0,
        );
        assert!(!texts(&o).contains(&"secret".to_string()));
        assert!(o.iter().any(|x| matches!(
            x,
            Op::Marker {
                kind: MarkerKind::Disc,
                ..
            }
        )));
        assert!(o.iter().any(
            |x| matches!(x, Op::Control { kind, label, .. } if kind == "text" && label == "v")
        ));
        assert!(o
            .iter()
            .any(|x| matches!(x, Op::Image { label, .. } if label == "logo")));
    }

    #[test]
    fn inline_backgrounds_are_drawn_under_the_text() {
        let o = ops(
            r#"<p>a <b style="background:yellow;padding:0 3px">b</b></p>"#,
            "body{margin:0}",
            0.0,
        );
        let bg = o
            .iter()
            .position(|x| {
                matches!(
                    x,
                    Op::Rect {
                        color: 0xFFFF_FF00,
                        ..
                    }
                )
            })
            .unwrap();
        let tx = o
            .iter()
            .rposition(|x| matches!(x, Op::Text { text, .. } if text == "b"))
            .unwrap();
        assert!(bg < tx);
    }

    #[test]
    fn a_huge_page_paints_only_what_is_visible_and_quickly() {
        let html = "<div><p>line of text</p></div>".repeat(5000);
        let t = std::time::Instant::now();
        let o = ops(&html, "", 0.0);
        assert!(o.len() < 2000, "culled: {}", o.len());
        assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    }
}
