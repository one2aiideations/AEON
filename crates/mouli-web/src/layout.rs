//! Layout: from the DOM and computed styles to a tree of positioned boxes.
//!
//! What is laid out: block flow with margin collapsing, inline text with line breaking,
//! alignment and mixed font sizes, inline-blocks and replaced elements, flexbox (rows, columns,
//! wrapping, grow and shrink, alignment), grid (fixed, `fr`, `auto` and `minmax` tracks,
//! `repeat` with counts and `auto-fill`, gaps, spans and explicit placement, alignment), tables
//! (auto widths, colspan), lists with markers, relative, absolute and fixed positioning,
//! percentage and `calc()` sizes, min and max sizes, `box-sizing`, and `overflow: hidden`.
//!
//! What is not: floats (a floated box is laid out as a normal block), grid template areas and
//! dense packing, `rowspan`, multi-column layout, `writing-mode`, transforms, and sticky
//! positioning (treated as relative). Layout
//! approximates a browser's; it is deterministic and uses only the [`Fonts`] it is given.

use crate::dom::{Dom, NodeId};
use crate::style::{
    Align, BorderStyle, Dim, Display, FlexDir, FlexWrap, Float, GridLine, GridTemplate, Justify,
    LineHeight, ListStyle, Overflow, Position, Size, Style, Styles, TextAlign, TextTransform,
    Track, TrackSize, VAlign, Viewport, WhiteSpace,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Most boxes laid out; beyond this the rest of the page is left out.
pub const MAX_BOXES: usize = 60_000;
/// Widest a box may be, in pixels.
const MAX_W: f32 = 100_000.0;

/// A font at a size, for measuring and drawing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontSpec {
    /// Size in pixels.
    pub size: f32,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Monospace face.
    pub mono: bool,
    /// Extra pixels after every character.
    pub letter_spacing: f32,
}

/// What layout needs to know about the fonts. The painter's real fonts and the test fonts both
/// implement it.
pub trait Fonts {
    /// The width of `text` (including letter spacing).
    fn text_width(&self, text: &str, f: &FontSpec) -> f32;
    /// Distance from the baseline up to the top of the text.
    fn ascent(&self, f: &FontSpec) -> f32;
    /// Distance from the baseline down to the bottom of the text.
    fn descent(&self, f: &FontSpec) -> f32;
    /// The line height `normal` stands for.
    fn normal_line_height(&self, f: &FontSpec) -> f32 {
        self.ascent(f) + self.descent(f)
    }
    /// The natural size of the picture at `src`, when it is known (decoded pictures).
    fn image_size(&self, _src: &str) -> Option<(f32, f32)> {
        None
    }
}

/// A rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// Whether the point is inside.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// What kind of box this is.
#[derive(Debug, Clone, PartialEq)]
pub enum BoxKind {
    /// A block-level box.
    Block,
    /// A box made for a run of inline content between blocks.
    Anonymous,
    /// An inline-level box laid out like a block (inline-block, inline-flex).
    InlineBlock,
    /// A replaced element drawn as a placeholder or control.
    Replaced(Replaced),
}

/// A replaced element or control.
#[derive(Debug, Clone, PartialEq)]
pub enum Replaced {
    /// An image (no decoder: drawn as a placeholder with its alternative text).
    Image {
        /// The alternative text.
        alt: String,
        /// The `src` attribute (the key of the decoded picture).
        src: String,
    },
    /// A text field, button, checkbox and so on.
    Control {
        /// What the control shows (value, placeholder or label).
        label: String,
        /// The control kind: `text`, `button`, `checkbox`, `radio`, `select`, `textarea`.
        kind: String,
        /// Checked or selected state.
        checked: bool,
    },
    /// Anything else with no content we can show (`video`, `canvas`, `iframe`).
    Placeholder {
        /// What to call it.
        label: String,
    },
}

/// A list marker.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    /// What to draw.
    pub kind: MarkerKind,
    /// Left edge, relative to the content box of the list item.
    pub x: f32,
    /// Baseline, relative to the top of the list item's content box.
    pub baseline: f32,
    /// The size of the marker's font.
    pub font: FontSpec,
    /// Its colour.
    pub color: u32,
}

/// The look of a list marker.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkerKind {
    /// A filled circle.
    Disc,
    /// An open circle.
    Circle,
    /// A filled square.
    Square,
    /// Text such as `1.`.
    Text(String),
}

/// One piece of a line.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A run of text on the baseline.
    Text {
        /// Left edge, relative to the content box.
        x: f32,
        /// Width.
        w: f32,
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
        /// The element the text belongs to (for links and hit testing).
        node: Option<NodeId>,
        /// Vertical shift from the baseline (positive is down), for sub- and superscript.
        dy: f32,
    },
    /// An inline-block or replaced box on the line.
    Atomic {
        /// Left edge of the margin box, relative to the content box.
        x: f32,
        /// Top of the margin box, relative to the top of the line.
        y: f32,
        /// The box (its own `rect` is relative to the margin box position).
        b: Box<LBox>,
    },
    /// Where an inline element's background and border go on this line.
    Span {
        /// Left edge, relative to the content box.
        x: f32,
        /// Width.
        w: f32,
        /// The element.
        node: NodeId,
        /// Its style.
        style: Rc<Style>,
        /// This fragment starts the element (draw the left border).
        left_edge: bool,
        /// This fragment ends the element (draw the right border).
        right_edge: bool,
    },
}

/// A line of inline content.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Top, relative to the content box.
    pub y: f32,
    /// Height.
    pub h: f32,
    /// Baseline, measured from the top of the line.
    pub baseline: f32,
    /// What is on the line.
    pub items: Vec<Item>,
}

/// A laid-out box.
#[derive(Debug, Clone, PartialEq)]
pub struct LBox {
    /// The element (`None` for anonymous boxes).
    pub node: Option<NodeId>,
    /// Its style.
    pub style: Rc<Style>,
    /// What kind of box.
    pub kind: BoxKind,
    /// The border box, relative to the parent's border box origin.
    pub rect: Rect,
    /// Margins in pixels: top, right, bottom, left.
    pub margin: [f32; 4],
    /// Border widths.
    pub border: [f32; 4],
    /// Paddings.
    pub padding: [f32; 4],
    /// Child boxes, in paint order within normal flow.
    pub children: Vec<LBox>,
    /// Lines of inline content directly in this box.
    pub lines: Vec<Line>,
    /// A list marker.
    pub marker: Option<Marker>,
    /// Baseline of the first line, from the top of the border box.
    pub first_baseline: Option<f32>,
    /// Out of normal flow (absolute or fixed).
    pub out_of_flow: bool,
    /// Width the content actually used (for shrink-to-fit).
    pub used_content_w: f32,
    eff_top: f32,
    eff_bottom: f32,
    pending: Vec<Pending>,
    empty_flow: bool,
}

impl LBox {
    fn new(node: Option<NodeId>, style: Rc<Style>, kind: BoxKind) -> Self {
        LBox {
            node,
            style,
            kind,
            rect: Rect::default(),
            margin: [0.0; 4],
            border: [0.0; 4],
            padding: [0.0; 4],
            children: Vec::new(),
            lines: Vec::new(),
            marker: None,
            first_baseline: None,
            out_of_flow: false,
            used_content_w: 0.0,
            eff_top: 0.0,
            eff_bottom: 0.0,
            pending: Vec::new(),
            empty_flow: false,
        }
    }

    /// Left edge of the content box inside this box.
    #[must_use]
    pub fn content_x(&self) -> f32 {
        self.border[3] + self.padding[3]
    }

    /// Top edge of the content box inside this box.
    #[must_use]
    pub fn content_y(&self) -> f32 {
        self.border[0] + self.padding[0]
    }

    /// Width of the content box.
    #[must_use]
    pub fn content_w(&self) -> f32 {
        (self.rect.w - self.border[1] - self.border[3] - self.padding[1] - self.padding[3]).max(0.0)
    }

    /// Height of the content box.
    #[must_use]
    pub fn content_h(&self) -> f32 {
        (self.rect.h - self.border[0] - self.border[2] - self.padding[0] - self.padding[2]).max(0.0)
    }

    /// Whether this box clips what overflows it.
    #[must_use]
    pub fn clips(&self) -> bool {
        self.style.overflow_x != Overflow::Visible || self.style.overflow_y != Overflow::Visible
    }
}

/// An absolutely positioned box waiting for its containing block.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    node: NodeId,
    /// Where it would have been in normal flow, relative to the box that holds this list.
    static_x: f32,
    static_y: f32,
    fixed: bool,
}

/// The result of laying out a document.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The root box (the `html` element).
    pub root: LBox,
    /// Total height of the page.
    pub height: f32,
    /// Total width of the page.
    pub width: f32,
    /// The colour behind everything (from `html`, or `body` when `html` has none).
    pub canvas_color: u32,
    /// Number of boxes made (layout stops growing at [`MAX_BOXES`]).
    pub boxes: usize,
}

// ------------------------------------------------------------------ helpers

fn collapse(a: f32, b: f32) -> f32 {
    if a >= 0.0 && b >= 0.0 {
        a.max(b)
    } else if a <= 0.0 && b <= 0.0 {
        a.min(b)
    } else {
        a + b
    }
}

fn dim_px(d: Dim, base: f32) -> f32 {
    let v = d.resolve(base);
    if v.is_finite() {
        v.clamp(-MAX_W, MAX_W)
    } else {
        0.0
    }
}

fn size_px(s: Size, base: f32) -> Option<f32> {
    s.dim().map(|d| dim_px(d, base))
}

fn font_of(s: &Style) -> FontSpec {
    FontSpec {
        size: s.font_size.max(1.0),
        bold: s.bold,
        italic: s.italic,
        mono: s.mono,
        letter_spacing: s.letter_spacing,
    }
}

fn transform(text: &str, t: TextTransform) -> String {
    match t {
        TextTransform::None => text.to_string(),
        TextTransform::Upper => text.to_uppercase(),
        TextTransform::Lower => text.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
                start = c.is_whitespace();
            }
            out
        }
    }
}

/// An inline atom: the pieces text and inline elements are cut into before line breaking.
#[derive(Debug, Clone)]
enum Atom {
    Text {
        text: String,
        style: Rc<Style>,
        node: Option<NodeId>,
        w: f32,
        nowrap: bool,
    },
    Space {
        style: Rc<Style>,
        node: Option<NodeId>,
        w: f32,
        keep: bool,
    },
    Break,
    Open {
        node: NodeId,
        style: Rc<Style>,
        w: f32,
    },
    Close {
        node: NodeId,
        w: f32,
    },
    Atomic {
        b: Box<LBox>,
        w: f32,
        min_w: f32,
    },
}

struct Ctx<'a> {
    dom: &'a Dom,
    styles: &'a Styles,
    fonts: &'a dyn Fonts,
    intrinsic: RefCell<HashMap<NodeId, (f32, f32)>>,
    count: RefCell<usize>,
}

/// Lays out a document.
#[must_use]
pub fn layout(dom: &Dom, styles: &Styles, fonts: &dyn Fonts, vp: Viewport) -> Layout {
    let ctx = Ctx {
        dom,
        styles,
        fonts,
        intrinsic: RefCell::new(HashMap::new()),
        count: RefCell::new(0),
    };
    let html = dom.children(dom.root()).first().copied().unwrap_or(0);
    let Some(style) = styles.rc(html) else {
        return Layout {
            root: LBox::new(None, Rc::new(Style::initial()), BoxKind::Block),
            height: 0.0,
            width: vp.w,
            canvas_color: 0xFFFF_FFFF,
            boxes: 0,
        };
    };
    let mut root = ctx.layout_block(
        html,
        &style,
        vp.w,
        Some(vp.h),
        Some(Forced {
            w: None,
            h: None,
            bfc: true,
        }),
    );
    root.rect.x = 0.0;
    root.rect.y = 0.0;
    // Absolutely positioned boxes whose containing block is the viewport.
    let pend = std::mem::take(&mut root.pending);
    let (w, h) = (vp.w, vp.h);
    for p in pend {
        let b = ctx.layout_abs(&p, w, h, 0.0, 0.0);
        root.children.push(b);
    }
    let canvas = if style.background >> 24 != 0 {
        style.background
    } else {
        dom.body()
            .and_then(|b| styles.get(b))
            .map_or(0, |s| s.background)
    };
    let boxes = *ctx.count.borrow();
    let height = root.rect.h.max(content_bottom(&root));
    let width = root.rect.w.max(vp.w);
    Layout {
        root,
        height,
        width,
        canvas_color: if canvas >> 24 == 0 {
            0xFFFF_FFFF
        } else {
            canvas
        },
        boxes,
    }
}

/// How far down in-flow content (and overflowing boxes) reaches.
fn content_bottom(b: &LBox) -> f32 {
    let mut bottom = b.rect.y + b.rect.h;
    for c in &b.children {
        if !c.out_of_flow {
            bottom = bottom.max(b.rect.y + content_bottom_rel(c));
        }
    }
    bottom
}

fn content_bottom_rel(b: &LBox) -> f32 {
    let mut bottom = b.rect.y + b.rect.h;
    for c in &b.children {
        bottom = bottom.max(b.rect.y + content_bottom_rel(c));
    }
    bottom
}

impl Ctx<'_> {
    fn style_of(&self, n: NodeId) -> Option<Rc<Style>> {
        self.styles.rc(n)
    }

    fn room(&self) -> bool {
        *self.count.borrow() < MAX_BOXES
    }

    fn tick(&self) {
        *self.count.borrow_mut() += 1;
    }

    /// Child nodes that take part in layout (elements that are not `display: none`, and text).
    fn visible_children(&self, n: NodeId) -> Vec<NodeId> {
        self.dom
            .children(n)
            .iter()
            .copied()
            .filter(|&c| match self.dom.tag(c) {
                Some(_) => self.style_of(c).is_some_and(|s| s.visible()),
                None => self.dom.text(c).is_some(),
            })
            .collect()
    }

    fn is_inline_level(&self, c: NodeId) -> bool {
        match self.style_of(c) {
            None => true,
            Some(s) => {
                matches!(s.position, Position::Static | Position::Relative)
                    && matches!(
                        s.display,
                        Display::Inline
                            | Display::InlineBlock
                            | Display::InlineFlex
                            | Display::InlineGrid
                    )
                    && s.float == Float::None
            }
        }
    }

    fn is_abs(&self, c: NodeId) -> bool {
        self.style_of(c)
            .is_some_and(|s| matches!(s.position, Position::Absolute | Position::Fixed))
    }

    // -------------------------------------------------------------- replaced elements

    fn attr_px(&self, n: NodeId, name: &str) -> Option<f32> {
        self.dom
            .attr(n, name)
            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
    }

    /// The kind of replaced element or control, if `n` is one.
    fn replaced(&self, n: NodeId, style: &Style) -> Option<Replaced> {
        let tag = self.dom.tag(n)?;
        match tag {
            "img" => Some(Replaced::Image {
                alt: self.dom.attr(n, "alt").unwrap_or("").to_string(),
                src: self.dom.attr(n, "src").unwrap_or("").to_string(),
            }),
            "input" => {
                let ty = self
                    .dom
                    .attr(n, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase();
                let value = self.dom.attr(n, "value").unwrap_or("");
                let label = match ty.as_str() {
                    "submit" => if value.is_empty() { "Submit" } else { value }.to_string(),
                    "reset" => if value.is_empty() { "Reset" } else { value }.to_string(),
                    "button" => value.to_string(),
                    "checkbox" | "radio" => String::new(),
                    "password" => "*".repeat(value.chars().count().min(64)),
                    _ => if value.is_empty() {
                        self.dom.attr(n, "placeholder").unwrap_or("")
                    } else {
                        value
                    }
                    .to_string(),
                };
                let kind = match ty.as_str() {
                    "submit" | "reset" | "button" | "image" => "button",
                    "checkbox" => "checkbox",
                    "radio" => "radio",
                    _ => "text",
                };
                Some(Replaced::Control {
                    label,
                    kind: kind.into(),
                    checked: self.dom.attr(n, "checked").is_some(),
                })
            }
            "button" => Some(Replaced::Control {
                label: self
                    .dom
                    .text_content(n)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                kind: "button".into(),
                checked: false,
            }),
            "select" => {
                let opts = self.dom.find_all("option");
                let mine: Vec<NodeId> = opts
                    .into_iter()
                    .filter(|&o| self.dom.ancestry(o).contains(&n))
                    .collect();
                let chosen = mine
                    .iter()
                    .copied()
                    .find(|&o| self.dom.attr(o, "selected").is_some())
                    .or_else(|| mine.first().copied());
                Some(Replaced::Control {
                    label: chosen
                        .map(|o| self.dom.text_content(o).trim().to_string())
                        .unwrap_or_default(),
                    kind: "select".into(),
                    checked: false,
                })
            }
            "textarea" => Some(Replaced::Control {
                label: self.dom.text_content(n),
                kind: "textarea".into(),
                checked: false,
            }),
            "video" | "canvas" | "iframe" | "object" | "embed" | "svg" | "audio" => {
                Some(Replaced::Placeholder {
                    label: tag.to_string(),
                })
            }
            _ => {
                let _ = style;
                None
            }
        }
    }

    /// The content-box size of a replaced element when CSS gives none.
    fn replaced_size(&self, n: NodeId, r: &Replaced, style: &Style) -> (f32, f32) {
        let f = font_of(style);
        let line = self.line_height(style).max(1.0);
        match r {
            Replaced::Image { src, .. } => {
                let w = self.attr_px(n, "width");
                let h = self.attr_px(n, "height");
                let natural = self
                    .fonts
                    .image_size(src)
                    .filter(|(a, b)| *a > 0.0 && *b > 0.0);
                match (w, h, natural) {
                    (Some(w), Some(h), _) => (w, h),
                    (Some(w), None, Some((a, b))) => (w, w * b / a),
                    (Some(w), None, None) => (w, w * 0.75),
                    (None, Some(h), Some((a, b))) => (h * a / b, h),
                    (None, Some(h), None) => (h * 4.0 / 3.0, h),
                    (None, None, Some((a, b))) => (a, b),
                    _ => (100.0, 75.0),
                }
            }
            Replaced::Control { label, kind, .. } => match kind.as_str() {
                "checkbox" | "radio" => (13.0, 13.0),
                "button" => (self.fonts.text_width(label, &f), line),
                "select" => ((self.fonts.text_width(label, &f) + 24.0).max(60.0), line),
                "textarea" => {
                    let cols = self.attr_px(n, "cols").unwrap_or(20.0);
                    let rows = self.attr_px(n, "rows").unwrap_or(2.0);
                    (cols * self.fonts.text_width("0", &f), rows * line)
                }
                _ => {
                    let size = self.attr_px(n, "size").unwrap_or(20.0);
                    (size * self.fonts.text_width("0", &f), line)
                }
            },
            Replaced::Placeholder { .. } => (
                self.attr_px(n, "width").unwrap_or(300.0),
                self.attr_px(n, "height").unwrap_or(150.0),
            ),
        }
    }

    fn line_height(&self, s: &Style) -> f32 {
        let f = font_of(s);
        match s.line_height {
            LineHeight::Normal => self.fonts.normal_line_height(&f),
            LineHeight::Number(x) => x * f.size,
            LineHeight::Px(p) => p,
        }
    }

    // -------------------------------------------------------------- intrinsic widths

    /// Minimum and maximum content widths of an element (no padding, border or margin).
    fn intrinsic(&self, n: NodeId) -> (f32, f32) {
        if let Some(v) = self.intrinsic.borrow().get(&n) {
            return *v;
        }
        let v = self.compute_intrinsic(n);
        self.intrinsic.borrow_mut().insert(n, v);
        v
    }

    fn extras_w(&self, s: &Style) -> f32 {
        let pad = s.padding[1].px + s.padding[3].px;
        let bor = s.border_width[1] + s.border_width[3];
        let mar = s.margin[1].dim().map_or(0.0, |d| d.px) + s.margin[3].dim().map_or(0.0, |d| d.px);
        pad + bor + mar
    }

    fn compute_intrinsic(&self, n: NodeId) -> (f32, f32) {
        let Some(style) = self.style_of(n) else {
            return (0.0, 0.0);
        };
        if let Some(r) = self.replaced(n, &style) {
            let (w, _) = self.replaced_size(n, &r, &style);
            let w = size_px(style.width, 0.0)
                .filter(|_| style.width.dim().is_some_and(|d| d.pct == 0.0))
                .unwrap_or(w);
            let w = w
                + style.padding[1].px
                + style.padding[3].px
                + style.border_width[1]
                + style.border_width[3];
            return (w, w);
        }
        let (mut min, mut max) = match style.display {
            Display::Flex | Display::InlineFlex => self.flex_intrinsic(n, &style),
            Display::Grid | Display::InlineGrid => self.grid_intrinsic(n),
            Display::Table => self.table_intrinsic(n),
            _ => self.flow_intrinsic(n, &style),
        };
        if let Some(d) = style.width.dim().filter(|d| d.pct == 0.0) {
            let mut w = d.px;
            if !style.border_box {
                // content width as given
            } else {
                w = (w
                    - style.padding[1].px
                    - style.padding[3].px
                    - style.border_width[1]
                    - style.border_width[3])
                    .max(0.0);
            }
            min = w;
            max = w;
        }
        if let Some(d) = style.max_width.dim().filter(|d| d.pct == 0.0) {
            max = max.min(d.px);
            min = min.min(d.px);
        }
        (min.max(style.min_width.px), max.max(style.min_width.px))
    }

    /// Intrinsic widths of the contents of a block container.
    fn flow_intrinsic(&self, n: NodeId, style: &Rc<Style>) -> (f32, f32) {
        let kids = self.visible_children(n);
        let (mut min, mut max) = (0.0f32, 0.0f32);
        let mut run: Vec<NodeId> = Vec::new();
        let flush = |run: &mut Vec<NodeId>, min: &mut f32, max: &mut f32| {
            if run.is_empty() {
                return;
            }
            let atoms = self.collect_atoms(run, style, None);
            let (a, b) = atoms_intrinsic(&atoms);
            *min = min.max(a);
            *max = max.max(b);
            run.clear();
        };
        for c in kids {
            if self.is_abs(c) {
                continue;
            }
            if self.is_inline_level(c) {
                run.push(c);
            } else {
                flush(&mut run, &mut min, &mut max);
                let cs = self.style_of(c);
                let extra = cs.as_ref().map_or(0.0, |s| self.extras_w(s));
                let (a, b) = self.intrinsic(c);
                min = min.max(a + extra);
                max = max.max(b + extra);
            }
        }
        flush(&mut run, &mut min, &mut max);
        (min, max)
    }

    // -------------------------------------------------------------- inline content

    /// Cuts inline content into atoms. With `avail` the atomic boxes are laid out; without it
    /// (for intrinsic widths) they are measured only.
    fn collect_atoms(&self, run: &[NodeId], parent: &Rc<Style>, avail: Option<f32>) -> Vec<Atom> {
        let mut atoms = Vec::new();
        let mut last_space = true;
        for &n in run {
            self.collect_one(n, parent, avail, &mut atoms, &mut last_space);
        }
        atoms
    }

    fn collect_one(
        &self,
        n: NodeId,
        parent: &Rc<Style>,
        avail: Option<f32>,
        atoms: &mut Vec<Atom>,
        last_space: &mut bool,
    ) {
        if atoms.len() > 200_000 || !self.room() {
            return;
        }
        if let Some(text) = self.dom.text(n) {
            self.collect_text(text, parent, self.dom.parent(n), atoms, last_space);
            return;
        }
        let Some(style) = self.style_of(n) else {
            return;
        };
        if !style.visible() {
            return;
        }
        if self.dom.tag(n) == Some("br") {
            atoms.push(Atom::Break);
            *last_space = true;
            return;
        }
        let replaced = self.replaced(n, &style);
        let atomic = replaced.is_some()
            || matches!(
                style.display,
                Display::InlineBlock | Display::InlineFlex | Display::InlineGrid
            );
        if atomic {
            let (min_w, w, b) = match avail {
                Some(a) => {
                    let b = self.layout_atomic(n, &style, replaced, a);
                    let mw = b.rect.w + b.margin[1] + b.margin[3];
                    (mw.min(b.rect.w), mw, Some(b))
                }
                None => {
                    let (lo, hi) = self.intrinsic(n);
                    let extra = if replaced.is_some() {
                        0.0
                    } else {
                        self.extras_w(&style)
                    };
                    let m = style.margin[1].dim().map_or(0.0, |d| d.px)
                        + style.margin[3].dim().map_or(0.0, |d| d.px);
                    let extra = if replaced.is_some() { extra + m } else { extra };
                    (lo + extra, hi + extra, None)
                }
            };
            let b = b.unwrap_or_else(|| LBox::new(Some(n), style.clone(), BoxKind::InlineBlock));
            atoms.push(Atom::Atomic {
                b: Box::new(b),
                w,
                min_w,
            });
            *last_space = false;
            return;
        }
        // An ordinary inline element.
        let left = style.margin[3].dim().map_or(0.0, |d| d.px)
            + style.border_width[3]
            + style.padding[3].px;
        let right = style.margin[1].dim().map_or(0.0, |d| d.px)
            + style.border_width[1]
            + style.padding[1].px;
        atoms.push(Atom::Open {
            node: n,
            style: style.clone(),
            w: left,
        });
        for &c in self.dom.children(n) {
            if self.dom.tag(c).is_some() && self.is_abs(c) {
                continue;
            }
            self.collect_one(c, &style, avail, atoms, last_space);
        }
        atoms.push(Atom::Close { node: n, w: right });
    }

    fn collect_text(
        &self,
        text: &str,
        style: &Rc<Style>,
        owner: Option<NodeId>,
        atoms: &mut Vec<Atom>,
        last_space: &mut bool,
    ) {
        let text = transform(text, style.text_transform);
        let f = font_of(style);
        let keep = matches!(style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap);
        let nowrap = matches!(style.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre);
        let newline_breaks = matches!(
            style.white_space,
            WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine
        );
        let sp = self.fonts.text_width(" ", &f);
        let mut word = String::new();
        let flush_word = |word: &mut String, atoms: &mut Vec<Atom>, last_space: &mut bool| {
            if !word.is_empty() {
                let w = self.fonts.text_width(word, &f);
                atoms.push(Atom::Text {
                    text: std::mem::take(word),
                    style: style.clone(),
                    node: owner,
                    w,
                    nowrap,
                });
                *last_space = false;
            }
        };
        for ch in text.chars() {
            if ch == '\n' && newline_breaks {
                flush_word(&mut word, atoms, last_space);
                atoms.push(Atom::Break);
                *last_space = true;
            } else if ch.is_whitespace() && !matches!(ch, '\u{a0}') {
                flush_word(&mut word, atoms, last_space);
                if keep {
                    atoms.push(Atom::Space {
                        style: style.clone(),
                        node: owner,
                        w: if ch == '\t' { sp * 4.0 } else { sp },
                        keep: true,
                    });
                    *last_space = false;
                } else if !*last_space {
                    atoms.push(Atom::Space {
                        style: style.clone(),
                        node: owner,
                        w: sp,
                        keep: false,
                    });
                    *last_space = true;
                }
            } else {
                word.push(ch);
            }
        }
        flush_word(&mut word, atoms, last_space);
    }

    /// Lays out one inline run into lines no wider than `avail`.
    ///
    /// Returns the lines, their total height and the widest line.
    fn layout_inline(
        &self,
        run: &[NodeId],
        parent: &Rc<Style>,
        avail: f32,
        text_indent: f32,
    ) -> InlineOut {
        let atoms = self.collect_atoms(run, parent, Some(avail));
        self.break_lines(atoms, parent, avail, text_indent)
    }

    fn strut(&self, s: &Style) -> (f32, f32) {
        let f = font_of(s);
        let lh = self.line_height(s);
        let (a, d) = (self.fonts.ascent(&f), self.fonts.descent(&f));
        let half = (lh - (a + d)) / 2.0;
        (a + half, d + half)
    }

    fn break_lines(
        &self,
        atoms: Vec<Atom>,
        parent: &Rc<Style>,
        avail: f32,
        _text_indent: f32,
    ) -> InlineOut {
        let avail = avail.max(0.0);
        let mut lines: Vec<Line> = Vec::new();
        let mut y = 0.0f32;
        let mut max_w = 0.0f32;
        let (strut_a, strut_d) = self.strut(parent);
        // The line being built.
        let mut cur: Vec<Placed> = Vec::new();
        let mut cur_w = 0.0f32;
        let mut can_break = false;
        let mut open: Vec<(NodeId, Rc<Style>)> = Vec::new();
        let mut line_has_content = false;

        let mut start_open: Vec<(NodeId, Rc<Style>)> = Vec::new();
        let finish = |cur: &mut Vec<Placed>,
                      open: &mut Vec<(NodeId, Rc<Style>)>,
                      start_open: &mut Vec<(NodeId, Rc<Style>)>,
                      lines: &mut Vec<Line>,
                      y: &mut f32,
                      max_w: &mut f32,
                      last: bool| {
            // Trailing spaces hang off the end and do not count.
            while matches!(
                cur.last(),
                Some(Placed {
                    atom: Atom::Space { keep: false, .. },
                    ..
                })
            ) {
                cur.pop();
            }
            let width: f32 = cur.iter().map(|p| p.w).sum();
            *max_w = max_w.max(width);
            if cur.is_empty() && !last {
                // A blank line made by a break still takes up space.
            }
            let (items, asc, desc) = self.build_line_items(
                cur, start_open, parent, strut_a, strut_d, avail, width, last,
            );
            start_open.clone_from(open);
            let h = asc + desc;
            lines.push(Line {
                y: *y,
                h,
                baseline: asc,
                items,
            });
            *y += h;
            cur.clear();
        };

        let mut i = 0;
        let atoms_len = atoms.len();
        let mut atoms = atoms;
        while i < atoms_len {
            let atom = std::mem::replace(&mut atoms[i], Atom::Break);
            i += 1;
            match atom {
                Atom::Break => {
                    let ghost = cur.is_empty() && !line_has_content;
                    let _ = ghost;
                    finish(
                        &mut cur,
                        &mut open,
                        &mut start_open,
                        &mut lines,
                        &mut y,
                        &mut max_w,
                        false,
                    );
                    cur_w = 0.0;
                    can_break = false;
                    line_has_content = false;
                }
                Atom::Space {
                    ref style, w, keep, ..
                } => {
                    let _ = style;
                    if cur.is_empty() && !keep {
                        continue;
                    }
                    cur_w += w;
                    cur.push(Placed { atom, w, x: 0.0 });
                    can_break = true;
                }
                Atom::Open {
                    ref node,
                    ref style,
                    w,
                } => {
                    open.push((*node, style.clone()));
                    cur_w += w;
                    cur.push(Placed { atom, w, x: 0.0 });
                }
                Atom::Close { w, .. } => {
                    open.pop();
                    cur_w += w;
                    cur.push(Placed { atom, w, x: 0.0 });
                }
                Atom::Text {
                    ref text,
                    ref style,
                    w,
                    nowrap,
                    ..
                } => {
                    let fits = cur_w + w <= avail + 0.01;
                    if !fits && can_break && line_has_content && !nowrap {
                        finish(
                            &mut cur,
                            &mut open,
                            &mut start_open,
                            &mut lines,
                            &mut y,
                            &mut max_w,
                            false,
                        );
                        cur_w = 0.0;
                    }
                    if w > avail + 0.01 && style.break_words && !nowrap {
                        // Split an overlong word into pieces that fit.
                        let f = font_of(style);
                        let mut piece = String::new();
                        let owner = if let Atom::Text { node, .. } = &atom {
                            *node
                        } else {
                            None
                        };
                        for ch in text.chars() {
                            let mut t = piece.clone();
                            t.push(ch);
                            if self.fonts.text_width(&t, &f) + cur_w > avail + 0.01
                                && !piece.is_empty()
                            {
                                let pw = self.fonts.text_width(&piece, &f);
                                cur.push(Placed {
                                    atom: Atom::Text {
                                        text: std::mem::take(&mut piece),
                                        style: style.clone(),
                                        node: owner,
                                        w: pw,
                                        nowrap: false,
                                    },
                                    w: pw,
                                    x: 0.0,
                                });
                                finish(
                                    &mut cur,
                                    &mut open,
                                    &mut start_open,
                                    &mut lines,
                                    &mut y,
                                    &mut max_w,
                                    false,
                                );
                                cur_w = 0.0;
                            }
                            piece.push(ch);
                        }
                        if !piece.is_empty() {
                            let pw = self.fonts.text_width(&piece, &f);
                            cur_w += pw;
                            cur.push(Placed {
                                atom: Atom::Text {
                                    text: piece,
                                    style: style.clone(),
                                    node: owner,
                                    w: pw,
                                    nowrap: false,
                                },
                                w: pw,
                                x: 0.0,
                            });
                        }
                    } else {
                        cur_w += w;
                        cur.push(Placed { atom, w, x: 0.0 });
                    }
                    line_has_content = true;
                    can_break = false;
                }
                Atom::Atomic { w, .. } => {
                    if cur_w + w > avail + 0.01 && line_has_content {
                        finish(
                            &mut cur,
                            &mut open,
                            &mut start_open,
                            &mut lines,
                            &mut y,
                            &mut max_w,
                            false,
                        );
                        cur_w = 0.0;
                    }
                    cur_w += w;
                    cur.push(Placed { atom, w, x: 0.0 });
                    line_has_content = true;
                    can_break = true;
                }
            }
        }
        let has_anything = cur.iter().any(|p| !matches!(p.atom, Atom::Space { .. }));
        if has_anything
            || (lines.is_empty() && cur.iter().any(|p| matches!(p.atom, Atom::Open { .. })))
        {
            finish(
                &mut cur,
                &mut open,
                &mut start_open,
                &mut lines,
                &mut y,
                &mut max_w,
                true,
            );
        }
        let first_baseline = lines.first().map(|l| l.y + l.baseline);
        InlineOut {
            height: y,
            lines,
            used_w: max_w,
            first_baseline,
        }
    }

    /// Positions the atoms of one line and works out its height and baseline.
    #[allow(clippy::too_many_arguments)]
    fn build_line_items(
        &self,
        cur: &[Placed],
        open_at_end: &[(NodeId, Rc<Style>)],
        parent: &Rc<Style>,
        strut_a: f32,
        strut_d: f32,
        avail: f32,
        width: f32,
        _last: bool,
    ) -> (Vec<Item>, f32, f32) {
        // Vertical metrics.
        let mut asc = strut_a;
        let mut desc = strut_d;
        let mut top_h = 0.0f32;
        let mut bottom_h = 0.0f32;
        for p in cur {
            match &p.atom {
                Atom::Text { style, .. } | Atom::Space { style, .. } => {
                    let (a, d) = self.strut(style);
                    let shift = match style.vertical_align {
                        VAlign::Sub => style.font_size * 0.25,
                        VAlign::Super => -style.font_size * 0.35,
                        _ => 0.0,
                    };
                    asc = asc.max(a - shift);
                    desc = desc.max(d + shift);
                }
                Atom::Atomic { b, .. } => {
                    let h = b.rect.h + b.margin[0] + b.margin[2];
                    match b.style.vertical_align {
                        VAlign::Top | VAlign::Bottom => {
                            top_h = top_h.max(h);
                            bottom_h = bottom_h.max(0.0);
                        }
                        VAlign::Middle => {
                            let half = h / 2.0;
                            let x_mid = parent.font_size * 0.25;
                            asc = asc.max(half + x_mid);
                            desc = desc.max(half - x_mid);
                        }
                        _ => {
                            asc = asc.max(h);
                        }
                    }
                }
                _ => {}
            }
        }
        if asc + desc < top_h {
            desc = top_h - asc;
        }
        let _ = bottom_h;
        // Horizontal alignment.
        let free = (avail - width).max(0.0);
        let mut x = match parent.text_align {
            TextAlign::Center => free / 2.0,
            TextAlign::Right => free,
            _ => 0.0,
        };
        let line_h = asc + desc;
        let mut items: Vec<Item> = Vec::new();
        let mut spans: Vec<(NodeId, Rc<Style>, f32, bool)> = Vec::new();
        // Elements already open when the line starts continue from its left edge.
        for (node, style) in open_at_end.iter().cloned() {
            spans.push((node, style, x, false));
        }
        let push_text = |items: &mut Vec<Item>,
                         x: f32,
                         w: f32,
                         text: &str,
                         style: &Style,
                         node: Option<NodeId>| {
            let font = font_of(style);
            let dy = match style.vertical_align {
                VAlign::Sub => style.font_size * 0.25,
                VAlign::Super => -style.font_size * 0.35,
                _ => 0.0,
            };
            if let Some(Item::Text {
                x: px,
                w: pw,
                text: pt,
                font: pf,
                color,
                underline,
                strike,
                node: pn,
                dy: pdy,
            }) = items.last_mut()
            {
                if *pf == font
                    && *color == style.color
                    && *underline == style.underline
                    && *strike == style.strike
                    && *pn == node
                    && *pdy == dy
                    && (*px + *pw - x).abs() < 0.01
                {
                    pt.push_str(text);
                    *pw += w;
                    return;
                }
            }
            items.push(Item::Text {
                x,
                w,
                text: text.to_string(),
                font,
                color: style.color,
                underline: style.underline,
                strike: style.strike,
                node,
                dy,
            });
        };
        for p in cur {
            match &p.atom {
                Atom::Text {
                    text,
                    style,
                    node,
                    w,
                    ..
                } => {
                    push_text(&mut items, x, *w, text, style, *node);
                    x += w;
                }
                Atom::Space { style, node, w, .. } => {
                    push_text(&mut items, x, *w, " ", style, *node);
                    x += w;
                }
                Atom::Open { node, style, w } => {
                    // The fragment starts after the margin and before the border.
                    let margin = style.margin[3].dim().map_or(0.0, |d| d.px);
                    spans.push((*node, style.clone(), x + margin, true));
                    x += w;
                }
                Atom::Close { node, w } => {
                    if let Some(pos) = spans.iter().rposition(|s| s.0 == *node) {
                        let (n, st, sx, left) = spans.remove(pos);
                        let margin = st.margin[1].dim().map_or(0.0, |d| d.px);
                        let end = x + w - margin;
                        items.insert(
                            0,
                            Item::Span {
                                x: sx,
                                w: (end - sx).max(0.0),
                                node: n,
                                style: st,
                                left_edge: left,
                                right_edge: true,
                            },
                        );
                    }
                    x += w;
                }
                Atom::Atomic { b, w, .. } => {
                    let mut b2 = b.clone();
                    let mh = b2.rect.h + b2.margin[0] + b2.margin[2];
                    let y = match b2.style.vertical_align {
                        VAlign::Top => 0.0,
                        VAlign::Bottom => line_h - mh,
                        VAlign::Middle => asc - mh / 2.0 - parent.font_size * 0.25,
                        _ => asc - mh,
                    };
                    b2.rect.x = b2.margin[3];
                    b2.rect.y = b2.margin[0];
                    items.push(Item::Atomic { x, y, b: b2 });
                    x += w;
                }
                Atom::Break => {}
            }
        }
        // Fragments still open at the end of the line.
        for (n, st, sx, left) in spans {
            items.insert(
                0,
                Item::Span {
                    x: sx,
                    w: (x - sx).max(0.0),
                    node: n,
                    style: st,
                    left_edge: left,
                    right_edge: false,
                },
            );
        }
        (items, asc, desc)
    }
}

struct Placed {
    atom: Atom,
    w: f32,
    #[allow(dead_code)]
    x: f32,
}

struct InlineOut {
    lines: Vec<Line>,
    height: f32,
    used_w: f32,
    first_baseline: Option<f32>,
}

/// Minimum and maximum widths of a run of atoms.
fn atoms_intrinsic(atoms: &[Atom]) -> (f32, f32) {
    let (mut min, mut max) = (0.0f32, 0.0f32);
    let (mut seg, mut chunk) = (0.0f32, 0.0f32);
    for a in atoms {
        match a {
            Atom::Text { w, nowrap, .. } => {
                seg += w;
                chunk += w;
                if *nowrap {
                    // An unbreakable run keeps growing the chunk.
                }
            }
            Atom::Space { w, keep, .. } => {
                seg += w;
                if *keep {
                    chunk += w;
                } else {
                    min = min.max(chunk);
                    chunk = 0.0;
                }
            }
            Atom::Open { w, .. } | Atom::Close { w, .. } => {
                seg += w;
                chunk += w;
            }
            Atom::Atomic { w, min_w, .. } => {
                seg += w;
                min = min.max(chunk);
                chunk = *min_w;
                min = min.max(chunk);
                chunk = 0.0;
            }
            Atom::Break => {
                max = max.max(seg);
                min = min.max(chunk);
                seg = 0.0;
                chunk = 0.0;
            }
        }
    }
    (min.max(chunk), max.max(seg))
}

// ------------------------------------------------------------------ block layout

/// Sizes imposed from outside (flex items, inline-blocks, absolutely positioned boxes).
#[derive(Debug, Clone, Copy, Default)]
struct Forced {
    /// Content-box width.
    w: Option<f32>,
    /// Content-box height.
    h: Option<f32>,
    /// The box establishes its own block formatting context.
    bfc: bool,
}

/// What a container's contents came to.
#[derive(Debug, Default)]
struct Flow {
    children: Vec<LBox>,
    lines: Vec<Line>,
    height: f32,
    first_baseline: Option<f32>,
    top_escape: f32,
    bottom_escape: f32,
    used_w: f32,
    pending: Vec<Pending>,
    empty: bool,
}

struct FlowState {
    y: f32,
    margin_acc: f32,
    at_start: bool,
    top_escape: f32,
    children: Vec<LBox>,
    first_baseline: Option<f32>,
    used_w: f32,
    pending: Vec<Pending>,
}

fn effective_border(s: &Style) -> [f32; 4] {
    let mut b = s.border_width;
    for (i, w) in b.iter_mut().enumerate() {
        if s.border_style[i] == BorderStyle::None {
            *w = 0.0;
        }
    }
    b
}

impl Ctx<'_> {
    fn pad_border(&self, s: &Style, cb_w: f32) -> ([f32; 4], [f32; 4]) {
        let border = effective_border(s);
        let mut padding = [0.0; 4];
        for (i, p) in padding.iter_mut().enumerate() {
            *p = dim_px(s.padding[i], cb_w).max(0.0);
        }
        (border, padding)
    }

    /// Lays out a block-level element (also used for inline-blocks, flex items and cells).
    fn layout_block(
        &self,
        node: NodeId,
        style: &Rc<Style>,
        cb_w: f32,
        cb_h: Option<f32>,
        forced: Option<Forced>,
    ) -> LBox {
        self.tick();
        let forced = forced.unwrap_or_default();
        let mut b = LBox::new(Some(node), style.clone(), BoxKind::Block);
        let cb_w = cb_w.clamp(0.0, MAX_W);
        let (border, padding) = self.pad_border(style, cb_w);
        b.border = border;
        b.padding = padding;
        let extras_h = border[1] + border[3] + padding[1] + padding[3];
        let extras_v = border[0] + border[2] + padding[0] + padding[2];
        let m_top = size_px(style.margin[0], cb_w).unwrap_or(0.0);
        let m_bottom = size_px(style.margin[2], cb_w).unwrap_or(0.0);
        let m_left = size_px(style.margin[3], cb_w);
        let m_right = size_px(style.margin[1], cb_w);
        let replaced = self.replaced(node, style);
        // Content width.
        let width_given = forced.w.is_some() || style.width.dim().is_some();
        let mut content_w = if let Some(w) = forced.w {
            w
        } else {
            match style.width {
                Size::Dim(d) => {
                    let v = dim_px(d, cb_w);
                    if style.border_box {
                        v - extras_h
                    } else {
                        v
                    }
                }
                Size::Auto => {
                    if let Some(r) = &replaced {
                        self.replaced_size(node, r, style).0
                    } else if style.display == Display::Table {
                        let (lo, hi) = self.intrinsic(node);
                        let avail =
                            cb_w - m_left.unwrap_or(0.0) - m_right.unwrap_or(0.0) - extras_h;
                        hi.min(avail.max(lo)).max(lo.min(avail))
                    } else {
                        cb_w - m_left.unwrap_or(0.0) - m_right.unwrap_or(0.0) - extras_h
                    }
                }
            }
        };
        if forced.w.is_none() {
            let adj = |d: Dim| {
                let v = dim_px(d, cb_w);
                if style.border_box {
                    v - extras_h
                } else {
                    v
                }
            };
            if let Some(d) = style.max_width.dim() {
                content_w = content_w.min(adj(d));
            }
            content_w = content_w.max(adj(style.min_width));
        }
        let content_w = content_w.clamp(0.0, MAX_W);
        // Horizontal margins (auto margins centre a box with a definite width).
        let used = content_w + extras_h + m_left.unwrap_or(0.0) + m_right.unwrap_or(0.0);
        let free = cb_w - used;
        let (ml, mr) = match (m_left, m_right) {
            (None, None) if width_given || replaced.is_some() => {
                (free.max(0.0) / 2.0, free.max(0.0) / 2.0)
            }
            (None, Some(r)) if width_given => (free.max(0.0), r),
            (Some(l), None) if width_given => (l, free.max(0.0)),
            (l, r) => (l.unwrap_or(0.0), r.unwrap_or(0.0)),
        };
        b.margin = [m_top, mr, m_bottom, ml];
        // Block formatting context and margin collapsing between parent and children.
        let bfc = forced.bfc
            || style.overflow_x != Overflow::Visible
            || style.overflow_y != Overflow::Visible
            || matches!(
                style.display,
                Display::Flex
                    | Display::InlineFlex
                    | Display::Grid
                    | Display::InlineGrid
                    | Display::Table
                    | Display::TableCell
                    | Display::InlineBlock
            )
            || matches!(style.position, Position::Absolute | Position::Fixed)
            || style.float != Float::None;
        let top_c = !bfc && border[0] == 0.0 && padding[0] == 0.0;
        let height_given = forced.h.is_some()
            || style
                .height
                .dim()
                .is_some_and(|d| d.pct == 0.0 || cb_h.is_some());
        let bot_c = !bfc && border[2] == 0.0 && padding[2] == 0.0 && !height_given;
        // Definite height for percentage children.
        let own_h = forced.h.or_else(|| {
            style.height.dim().and_then(|d| {
                if d.pct != 0.0 && cb_h.is_none() {
                    None
                } else {
                    let v = dim_px(d, cb_h.unwrap_or(0.0));
                    Some(if style.border_box { v - extras_v } else { v })
                }
            })
        });
        let flow = if let Some(r) = replaced {
            let (rw, rh) = self.replaced_size(node, &r, style);
            let _ = rw;
            // A picture whose width was set by style keeps its proportions.
            let ratio_h = match &r {
                Replaced::Image { src, .. } if self.attr_px(node, "height").is_none() => self
                    .fonts
                    .image_size(src)
                    .filter(|(a, b)| *a > 0.0 && *b > 0.0)
                    .map(|(a, b)| content_w * b / a),
                _ => None,
            };
            b.kind = BoxKind::Replaced(r);
            Flow {
                height: if style.height.dim().is_some() || forced.h.is_some() {
                    0.0
                } else {
                    ratio_h.unwrap_or(rh)
                },
                ..Flow::default()
            }
        } else {
            match style.display {
                Display::Flex | Display::InlineFlex => {
                    self.layout_flex(node, style, content_w, own_h)
                }
                Display::Grid | Display::InlineGrid => {
                    self.layout_grid(node, style, content_w, own_h)
                }
                Display::Table => self.layout_table(node, style, content_w),
                _ => self.layout_flow(node, style, content_w, own_h, top_c, bot_c),
            }
        };
        // Height.
        let mut content_h = own_h.unwrap_or(flow.height);
        if forced.h.is_none() {
            let adj = |d: Dim, base: f32| {
                let v = dim_px(d, base);
                if style.border_box {
                    v - extras_v
                } else {
                    v
                }
            };
            if let Some(d) = style.max_height.dim() {
                if d.pct == 0.0 || cb_h.is_some() {
                    content_h = content_h.min(adj(d, cb_h.unwrap_or(0.0)));
                }
            }
            if style.min_height.pct == 0.0 || cb_h.is_some() {
                content_h = content_h.max(adj(style.min_height, cb_h.unwrap_or(0.0)));
            }
        }
        let content_h = content_h.clamp(0.0, MAX_W);
        b.rect = Rect {
            x: ml,
            y: 0.0,
            w: content_w + extras_h,
            h: content_h + extras_v,
        };
        let (cx, cy) = (b.content_x(), b.content_y());
        b.children = flow.children;
        for c in &mut b.children {
            c.rect.x += cx;
            c.rect.y += cy;
        }
        b.lines = flow.lines;
        b.used_content_w = flow.used_w;
        b.first_baseline = flow.first_baseline.map(|f| f + cy);
        b.pending = flow
            .pending
            .into_iter()
            .map(|mut p| {
                p.static_x += cx;
                p.static_y += cy;
                p
            })
            .collect();
        // Effective margins for the parent.
        let escaped_top = if top_c { flow.top_escape } else { 0.0 };
        let escaped_bottom = if bot_c { flow.bottom_escape } else { 0.0 };
        let empty = flow.empty
            && !height_given
            && extras_v == 0.0
            && b.children.iter().all(|c| c.out_of_flow);
        if empty {
            let m = collapse(
                collapse(m_top, escaped_top),
                collapse(m_bottom, escaped_bottom),
            );
            b.eff_top = m;
            b.eff_bottom = m;
            b.empty_flow = true;
        } else {
            b.eff_top = if top_c {
                collapse(m_top, escaped_top)
            } else {
                m_top
            };
            b.eff_bottom = if bot_c {
                collapse(m_bottom, escaped_bottom)
            } else {
                m_bottom
            };
        }
        // List marker.
        if style.display == Display::ListItem && style.list_style != ListStyle::None {
            b.marker = self.marker_for(
                node,
                style,
                flow.first_baseline.unwrap_or_else(|| self.strut(style).0),
            );
        }
        // Absolutely positioned descendants whose containing block is this box.
        if style.position != Position::Static {
            let pend = std::mem::take(&mut b.pending);
            let (keep, mine): (Vec<_>, Vec<_>) = pend.into_iter().partition(|p| p.fixed);
            b.pending = keep;
            let (w, h) = (
                b.rect.w - b.border[1] - b.border[3],
                b.rect.h - b.border[0] - b.border[2],
            );
            for p in mine {
                let ab = self.layout_abs(&p, w, h, b.border[3], b.border[0]);
                b.children.push(ab);
            }
        }
        b
    }

    fn marker_for(&self, node: NodeId, style: &Rc<Style>, baseline: f32) -> Option<Marker> {
        let font = font_of(style);
        let kind = match style.list_style {
            ListStyle::None => return None,
            ListStyle::Disc => MarkerKind::Disc,
            ListStyle::Circle => MarkerKind::Circle,
            ListStyle::Square => MarkerKind::Square,
            other => {
                // Count this item among the list items before it.
                let parent = self.dom.parent(node)?;
                let start: i32 = self
                    .dom
                    .attr(parent, "start")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(1);
                let mut index = start;
                for &s in self.dom.children(parent) {
                    if s == node {
                        break;
                    }
                    if self
                        .style_of(s)
                        .is_some_and(|x| x.display == Display::ListItem)
                    {
                        index += 1;
                    }
                }
                let text = match other {
                    ListStyle::Decimal => format!("{index}."),
                    ListStyle::LowerAlpha => format!("{}.", alpha(index, false)),
                    ListStyle::UpperAlpha => format!("{}.", alpha(index, true)),
                    ListStyle::LowerRoman => format!("{}.", roman(index).to_lowercase()),
                    _ => format!("{}.", roman(index)),
                };
                MarkerKind::Text(text)
            }
        };
        let w = match &kind {
            MarkerKind::Text(t) => self.fonts.text_width(t, &font),
            _ => font.size * 0.35,
        };
        Some(Marker {
            kind,
            x: -(w + font.size * 0.4),
            baseline,
            font,
            color: style.color,
        })
    }

    /// An inline-block, inline-flex or replaced element on a line.
    fn layout_atomic(
        &self,
        node: NodeId,
        style: &Rc<Style>,
        replaced: Option<Replaced>,
        avail: f32,
    ) -> LBox {
        let (lo, hi) = self.intrinsic(node);
        let (border, padding) = self.pad_border(style, avail);
        let extras = border[1] + border[3] + padding[1] + padding[3];
        let content_w = match style.width {
            Size::Dim(d) => {
                let v = dim_px(d, avail);
                if style.border_box {
                    v - extras
                } else {
                    v
                }
            }
            Size::Auto => {
                if replaced.is_some() {
                    hi
                } else {
                    // Shrink to fit.
                    let m = size_px(style.margin[1], avail).unwrap_or(0.0)
                        + size_px(style.margin[3], avail).unwrap_or(0.0);
                    let room = (avail - m - extras).max(0.0);
                    hi.min(room.max(lo)).max(lo.min(room))
                }
            }
        };
        let mut b = self.layout_block(
            node,
            style,
            avail,
            None,
            Some(Forced {
                w: Some(content_w.max(0.0)),
                h: None,
                bfc: true,
            }),
        );
        if matches!(b.kind, BoxKind::Block) {
            b.kind = BoxKind::InlineBlock;
        }
        // Atomic boxes use their own margins as numbers (auto is 0).
        b
    }

    /// Lays out the children of a block container: block children stacked with collapsing
    /// margins, runs of inline children as lines.
    fn layout_flow(
        &self,
        node: NodeId,
        style: &Rc<Style>,
        content_w: f32,
        def_h: Option<f32>,
        top_c: bool,
        bot_c: bool,
    ) -> Flow {
        let kids = self.visible_children(node);
        let has_block = kids
            .iter()
            .any(|&c| !self.is_inline_level(c) && !self.is_abs(c));
        let mut flow = Flow::default();
        if !self.room() {
            return flow;
        }
        if !has_block {
            let run: Vec<NodeId> = kids.iter().copied().filter(|&c| !self.is_abs(c)).collect();
            let out = self.layout_inline(&run, style, content_w, 0.0);
            flow.height = out.height;
            flow.used_w = out.used_w;
            flow.first_baseline = out.first_baseline;
            flow.empty = out.lines.is_empty() || out.height == 0.0;
            flow.lines = out.lines;
            for &c in &kids {
                if self.is_abs(c) {
                    let fixed = self
                        .style_of(c)
                        .is_some_and(|s| s.position == Position::Fixed);
                    flow.pending.push(Pending {
                        node: c,
                        static_x: 0.0,
                        static_y: 0.0,
                        fixed,
                    });
                }
            }
            return flow;
        }
        let mut st = FlowState {
            y: 0.0,
            margin_acc: 0.0,
            at_start: true,
            top_escape: 0.0,
            children: Vec::new(),
            first_baseline: None,
            used_w: 0.0,
            pending: Vec::new(),
        };
        let mut run: Vec<NodeId> = Vec::new();
        for c in kids {
            if self.is_abs(c) {
                let fixed = self
                    .style_of(c)
                    .is_some_and(|s| s.position == Position::Fixed);
                st.pending.push(Pending {
                    node: c,
                    static_x: 0.0,
                    static_y: st.y + st.margin_acc,
                    fixed,
                });
                continue;
            }
            if self.is_inline_level(c) {
                run.push(c);
                continue;
            }
            self.flush_run(&mut st, &mut run, style, content_w, top_c);
            let Some(cs) = self.style_of(c) else { continue };
            if !self.room() {
                break;
            }
            let cb = self.layout_block(c, &cs, content_w, def_h, None);
            self.push_block(&mut st, cb, top_c, content_w);
        }
        self.flush_run(&mut st, &mut run, style, content_w, top_c);
        flow.empty = st.at_start;
        if st.at_start {
            flow.top_escape = st.margin_acc;
            flow.bottom_escape = 0.0;
            if !top_c {
                st.y += st.margin_acc;
            }
        } else if bot_c {
            flow.bottom_escape = st.margin_acc;
        } else {
            st.y += st.margin_acc;
        }
        if !st.at_start {
            flow.top_escape = st.top_escape;
        }
        flow.height = st.y;
        flow.children = st.children;
        flow.first_baseline = st.first_baseline;
        flow.used_w = st.used_w;
        flow.pending = st.pending;
        flow
    }

    fn flush_run(
        &self,
        st: &mut FlowState,
        run: &mut Vec<NodeId>,
        style: &Rc<Style>,
        content_w: f32,
        top_c: bool,
    ) {
        if run.is_empty() {
            return;
        }
        let nodes = std::mem::take(run);
        let out = self.layout_inline(&nodes, style, content_w, 0.0);
        if out.lines.is_empty() || out.height == 0.0 {
            return;
        }
        let mut b = LBox::new(None, style.clone(), BoxKind::Anonymous);
        b.rect = Rect {
            x: 0.0,
            y: 0.0,
            w: content_w,
            h: out.height,
        };
        b.lines = out.lines;
        b.used_content_w = out.used_w;
        b.first_baseline = out.first_baseline;
        self.push_block(st, b, top_c, content_w);
    }

    /// Places a finished child box in the flow, collapsing margins.
    fn push_block(&self, st: &mut FlowState, mut b: LBox, top_c: bool, content_w: f32) {
        st.margin_acc = collapse(st.margin_acc, b.eff_top);
        st.used_w = st
            .used_w
            .max(b.rect.w + b.margin[1] + b.margin[3])
            .min(content_w.max(b.rect.w + b.margin[1] + b.margin[3]));
        if b.empty_flow {
            st.margin_acc = collapse(st.margin_acc, b.eff_bottom);
            b.rect.y = st.y + st.margin_acc.max(0.0);
            self.finish_child(&mut b, content_w);
            st.pending
                .extend(std::mem::take(&mut b.pending).into_iter().map(|mut p| {
                    p.static_x += b.rect.x;
                    p.static_y += b.rect.y;
                    p
                }));
            st.children.push(b);
            return;
        }
        if st.at_start && top_c {
            st.top_escape = st.margin_acc;
        } else {
            st.y += st.margin_acc;
        }
        st.margin_acc = 0.0;
        b.rect.y = st.y;
        self.finish_child(&mut b, content_w);
        if st.first_baseline.is_none() {
            st.first_baseline = b.first_baseline.map(|f| f + b.rect.y);
        }
        st.y += b.rect.h;
        st.margin_acc = b.eff_bottom;
        st.at_start = false;
        st.pending
            .extend(std::mem::take(&mut b.pending).into_iter().map(|mut p| {
                p.static_x += b.rect.x;
                p.static_y += b.rect.y;
                p
            }));
        st.children.push(b);
    }

    fn finish_child(&self, b: &mut LBox, cb_w: f32) {
        // Relative positioning shifts the painted box but not its neighbours.
        if b.style.position == Position::Relative {
            let dx = match (b.style.inset[3].dim(), b.style.inset[1].dim()) {
                (Some(l), _) => dim_px(l, cb_w),
                (None, Some(r)) => -dim_px(r, cb_w),
                _ => 0.0,
            };
            let dy = match (b.style.inset[0].dim(), b.style.inset[2].dim()) {
                (Some(t), _) => dim_px(t, 0.0),
                (None, Some(bm)) => -dim_px(bm, 0.0),
                _ => 0.0,
            };
            b.rect.x += dx;
            b.rect.y += dy;
        }
    }

    /// Lays out an absolutely positioned box against its containing block.
    ///
    /// `cb_w` and `cb_h` are the padding box of the containing block; `off_x` and `off_y` are
    /// where that padding box starts inside the containing box (its border widths).
    fn layout_abs(&self, p: &Pending, cb_w: f32, cb_h: f32, off_x: f32, off_y: f32) -> LBox {
        let Some(style) = self.style_of(p.node) else {
            return LBox::new(None, Rc::new(Style::initial()), BoxKind::Block);
        };
        let (border, padding) = self.pad_border(&style, cb_w);
        let extras_h = border[1] + border[3] + padding[1] + padding[3];
        let extras_v = border[0] + border[2] + padding[0] + padding[2];
        let left = size_px(style.inset[3], cb_w);
        let right = size_px(style.inset[1], cb_w);
        let top = size_px(style.inset[0], cb_h);
        let bottom = size_px(style.inset[2], cb_h);
        let ml = size_px(style.margin[3], cb_w).unwrap_or(0.0);
        let mr = size_px(style.margin[1], cb_w).unwrap_or(0.0);
        let mt = size_px(style.margin[0], cb_w).unwrap_or(0.0);
        let mb = size_px(style.margin[2], cb_w).unwrap_or(0.0);
        let (lo, hi) = self.intrinsic(p.node);
        let content_w = match style.width {
            Size::Dim(d) => {
                let v = dim_px(d, cb_w);
                if style.border_box {
                    v - extras_h
                } else {
                    v
                }
            }
            Size::Auto => match (left, right) {
                (Some(l), Some(r)) => cb_w - l - r - ml - mr - extras_h,
                _ => {
                    let room =
                        cb_w - left.unwrap_or(0.0) - right.unwrap_or(0.0) - ml - mr - extras_h;
                    hi.min(room.max(lo)).max(lo.min(room))
                }
            },
        }
        .max(0.0);
        let forced_h = match (style.height, top, bottom) {
            (Size::Dim(d), _, _) => {
                let v = dim_px(d, cb_h);
                Some(if style.border_box { v - extras_v } else { v })
            }
            (Size::Auto, Some(t), Some(bm)) => Some((cb_h - t - bm - mt - mb - extras_v).max(0.0)),
            _ => None,
        };
        let mut b = self.layout_block(
            p.node,
            &style,
            cb_w,
            Some(cb_h),
            Some(Forced {
                w: Some(content_w),
                h: forced_h,
                bfc: true,
            }),
        );
        let bw = b.rect.w;
        let bh = b.rect.h;
        let x = match (left, right) {
            (Some(l), _) => off_x + l + ml,
            (None, Some(r)) => off_x + cb_w - r - mr - bw,
            _ => p.static_x + ml,
        };
        let y = match (top, bottom) {
            (Some(t), _) => off_y + t + mt,
            (None, Some(bm)) => off_y + cb_h - bm - mb - bh,
            _ => p.static_y + mt,
        };
        b.rect.x = x;
        b.rect.y = y;
        b.out_of_flow = true;
        b
    }
}

fn alpha(n: i32, upper: bool) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let mut n = n;
    let mut out = String::new();
    while n > 0 {
        n -= 1;
        let c = char::from(b'a' + u8::try_from(n % 26).unwrap_or(0));
        out.insert(0, if upper { c.to_ascii_uppercase() } else { c });
        n /= 26;
    }
    out
}

fn roman(n: i32) -> String {
    if !(1..4000).contains(&n) {
        return n.to_string();
    }
    let table = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut n = n;
    let mut out = String::new();
    for (v, s) in table {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

// ------------------------------------------------------------------ flexbox

struct FItem {
    elem: Option<NodeId>,
    text: Option<NodeId>,
    style: Rc<Style>,
    order: i32,
    grow: f32,
    shrink: f32,
    margin: [f32; 4],
    auto_m: [bool; 4],
    border: [f32; 4],
    padding: [f32; 4],
    base: f32,
    min: f32,
    max: f32,
    target: f32,
    frozen: bool,
    lbox: Option<LBox>,
}

impl FItem {
    fn extras(&self, row: bool) -> f32 {
        if row {
            self.border[1] + self.border[3] + self.padding[1] + self.padding[3]
        } else {
            self.border[0] + self.border[2] + self.padding[0] + self.padding[2]
        }
    }

    fn margin_main(&self, row: bool) -> f32 {
        if row {
            self.margin[3] + self.margin[1]
        } else {
            self.margin[0] + self.margin[2]
        }
    }

    fn outer_main(&self, size: f32, row: bool) -> f32 {
        size + self.extras(row) + self.margin_main(row)
    }
}

fn shift_content(b: &mut LBox, dy: f32) {
    for c in &mut b.children {
        c.rect.y += dy;
    }
    for l in &mut b.lines {
        l.y += dy;
    }
    if let Some(m) = &mut b.marker {
        m.baseline += dy;
    }
    if let Some(f) = &mut b.first_baseline {
        *f += dy;
    }
}

impl Ctx<'_> {
    fn flex_items(
        &self,
        node: NodeId,
        style: &Rc<Style>,
        content_w: f32,
        row: bool,
        def_h: Option<f32>,
    ) -> (Vec<FItem>, Vec<Pending>) {
        let anon = {
            let mut s = Style::inherit_from(style);
            s.display = Display::Block;
            Rc::new(s)
        };
        let mut items: Vec<FItem> = Vec::new();
        let mut pending = Vec::new();
        let main_cb = if row { content_w } else { def_h.unwrap_or(0.0) };
        for c in self.visible_children(node) {
            let (elem, text, st) = if self.dom.tag(c).is_some() {
                let Some(s) = self.style_of(c) else { continue };
                if matches!(s.position, Position::Absolute | Position::Fixed) {
                    pending.push(Pending {
                        node: c,
                        static_x: 0.0,
                        static_y: 0.0,
                        fixed: s.position == Position::Fixed,
                    });
                    continue;
                }
                (Some(c), None, s)
            } else {
                if self.dom.text(c).is_none_or(|t| t.trim().is_empty()) {
                    continue;
                }
                (None, Some(c), anon.clone())
            };
            let (border, padding) = self.pad_border(&st, content_w);
            let mut margin = [0.0; 4];
            let mut auto_m = [false; 4];
            for i in 0..4 {
                match st.margin[i] {
                    Size::Auto => auto_m[i] = true,
                    Size::Dim(d) => margin[i] = dim_px(d, content_w),
                }
            }
            let mut it = FItem {
                elem,
                text,
                order: st.order,
                grow: st.flex_grow,
                shrink: st.flex_shrink,
                style: st.clone(),
                margin,
                auto_m,
                border,
                padding,
                base: 0.0,
                min: 0.0,
                max: f32::INFINITY,
                target: 0.0,
                frozen: false,
                lbox: None,
            };
            let extras = it.extras(row);
            let adj = |d: Dim, base: f32| {
                let v = dim_px(d, base);
                if st.border_box {
                    (v - extras).max(0.0)
                } else {
                    v.max(0.0)
                }
            };
            let (size_prop, min_prop, max_prop) = if row {
                (st.width, st.min_width, st.max_width)
            } else {
                (st.height, st.min_height, st.max_height)
            };
            if let Some(d) = max_prop
                .dim()
                .filter(|d| row || d.pct == 0.0 || def_h.is_some())
            {
                it.max = adj(d, main_cb);
            }
            if min_prop.pct == 0.0 || row || def_h.is_some() {
                it.min = adj(min_prop, main_cb);
            }
            let basis = match (st.flex_basis, size_prop) {
                (Size::Dim(d), _) => Some(adj(d, main_cb)),
                (Size::Auto, Size::Dim(d)) if d.pct == 0.0 || row || def_h.is_some() => {
                    Some(adj(d, main_cb))
                }
                _ => None,
            };
            it.base = match basis {
                Some(b) => b,
                None => {
                    if row {
                        match elem {
                            Some(e) => {
                                let (_, hi) = self.intrinsic(e);
                                hi
                            }
                            None => {
                                let atoms = self.collect_atoms(&[c], &anon, None);
                                atoms_intrinsic(&atoms).1
                            }
                        }
                    } else {
                        f32::NAN // measured after the cross size is known
                    }
                }
            };
            // The automatic minimum size: content that cannot be squeezed below its longest word.
            if row
                && basis.is_none_or(|_| size_prop.dim().is_none())
                && it.min == 0.0
                && st.overflow_x == Overflow::Visible
            {
                let lo = match elem {
                    Some(e) => self.intrinsic(e).0,
                    None => atoms_intrinsic(&self.collect_atoms(&[c], &anon, None)).0,
                };
                it.min = lo.min(if it.base.is_nan() {
                    lo
                } else {
                    it.base.max(0.0)
                });
            }
            items.push(it);
        }
        items.sort_by_key(|i| i.order);
        (items, pending)
    }

    /// Lays out an item at a content width (and optionally a content height).
    fn layout_item(&self, it: &FItem, content_w: f32, w: f32, h: Option<f32>) -> LBox {
        match it.elem {
            Some(e) => self.layout_block(
                e,
                &it.style,
                content_w,
                None,
                Some(Forced {
                    w: Some(w.max(0.0)),
                    h,
                    bfc: true,
                }),
            ),
            None => {
                let mut b = LBox::new(None, it.style.clone(), BoxKind::Anonymous);
                let out = self.layout_inline(&[it.text.unwrap_or(0)], &it.style, w.max(0.0), 0.0);
                b.rect = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: w.max(0.0),
                    h: h.unwrap_or(out.height),
                };
                b.lines = out.lines;
                b.first_baseline = out.first_baseline;
                b.used_content_w = out.used_w;
                b
            }
        }
    }

    fn layout_flex(
        &self,
        node: NodeId,
        style: &Rc<Style>,
        content_w: f32,
        def_h: Option<f32>,
    ) -> Flow {
        let row = matches!(style.flex_direction, FlexDir::Row | FlexDir::RowReverse);
        let reverse = matches!(
            style.flex_direction,
            FlexDir::RowReverse | FlexDir::ColumnReverse
        );
        let (mut items, pending) = self.flex_items(node, style, content_w, row, def_h);
        let mut flow = Flow {
            pending,
            ..Flow::default()
        };
        if items.is_empty() || !self.room() {
            flow.empty = true;
            if let Some(h) = def_h {
                flow.height = h;
            }
            return flow;
        }
        let gap_main = if row {
            dim_px(style.column_gap, content_w)
        } else {
            dim_px(style.row_gap, def_h.unwrap_or(0.0))
        }
        .max(0.0);
        let gap_cross = if row {
            dim_px(style.row_gap, def_h.unwrap_or(0.0))
        } else {
            dim_px(style.column_gap, content_w)
        }
        .max(0.0);
        let main_avail = if row {
            content_w
        } else {
            def_h.unwrap_or(f32::INFINITY)
        };
        let wrap = style.flex_wrap != FlexWrap::NoWrap;
        // Column items are measured at their cross (width) size first.
        let cross_w = |it: &FItem| -> f32 {
            let free = content_w - it.margin[1] - it.margin[3] - it.extras(true);
            let align = it.style.align_self.unwrap_or(style.align_items);
            match it.style.width {
                Size::Dim(d) => {
                    let v = dim_px(d, content_w);
                    if it.style.border_box {
                        v - it.extras(true)
                    } else {
                        v
                    }
                }
                Size::Auto => {
                    if align == Align::Stretch && !it.auto_m[1] && !it.auto_m[3] {
                        free
                    } else {
                        let (lo, hi) = match it.elem {
                            Some(e) => self.intrinsic(e),
                            None => (
                                0.0,
                                atoms_intrinsic(&self.collect_atoms(
                                    &[it.text.unwrap_or(0)],
                                    &it.style,
                                    None,
                                ))
                                .1,
                            ),
                        };
                        hi.min(free.max(lo)).max(lo.min(free))
                    }
                }
            }
            .max(0.0)
        };
        if !row {
            for it in &mut items {
                if it.base.is_nan() {
                    let w = cross_w(it);
                    let b = self.layout_item(it, content_w, w, None);
                    it.base = (b.rect.h - it.extras(false)).max(0.0);
                }
            }
        }
        for it in &mut items {
            it.base = it.base.max(0.0);
            it.target = it.base.clamp(it.min, it.max.max(it.min));
        }
        // Split into lines.
        let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
        let mut used = 0.0f32;
        for (i, it) in items.iter().enumerate() {
            let outer = it.outer_main(it.target, row);
            let cur = lines.last_mut().unwrap_or_else(|| unreachable!());
            if wrap && !cur.is_empty() && used + gap_main + outer > main_avail + 0.01 {
                lines.push(vec![i]);
                used = outer;
            } else {
                used += if cur.is_empty() {
                    outer
                } else {
                    gap_main + outer
                };
                cur.push(i);
            }
        }
        // Resolve flexible lengths line by line.
        for line in &lines {
            let gaps = gap_main * (line.len().saturating_sub(1)) as f32;
            for &i in line {
                items[i].frozen = false;
                items[i].target = items[i]
                    .base
                    .clamp(items[i].min, items[i].max.max(items[i].min));
            }
            if main_avail.is_finite() {
                let hyp: f32 = line
                    .iter()
                    .map(|&i| items[i].outer_main(items[i].target, row))
                    .sum::<f32>()
                    + gaps;
                let growing = main_avail - hyp > 0.0;
                // Items that cannot flex in this direction are frozen at their hypothetical size.
                for &i in line {
                    let factor = if growing {
                        items[i].grow
                    } else {
                        items[i].shrink * items[i].base
                    };
                    if factor <= 0.0 {
                        items[i].frozen = true;
                    }
                }
                for _ in 0..=line.len() {
                    let frozen_sum: f32 = line
                        .iter()
                        .filter(|&&i| items[i].frozen)
                        .map(|&i| items[i].outer_main(items[i].target, row))
                        .sum();
                    let unfrozen: Vec<usize> =
                        line.iter().copied().filter(|&i| !items[i].frozen).collect();
                    if unfrozen.is_empty() {
                        break;
                    }
                    let base_sum: f32 = unfrozen
                        .iter()
                        .map(|&i| items[i].outer_main(items[i].base, row))
                        .sum();
                    let free = main_avail - frozen_sum - base_sum - gaps;
                    let weights: Vec<f32> = unfrozen
                        .iter()
                        .map(|&i| {
                            if free >= 0.0 {
                                items[i].grow
                            } else {
                                items[i].shrink * items[i].base
                            }
                        })
                        .collect();
                    let total: f32 = weights.iter().sum();
                    let mut violation = 0.0f32;
                    let mut clamped: Vec<(usize, f32)> = Vec::new();
                    for (k, &i) in unfrozen.iter().enumerate() {
                        let raw = items[i].base
                            + if total > 0.0 {
                                free * weights[k] / total
                            } else {
                                0.0
                            };
                        let c = raw
                            .clamp(items[i].min, items[i].max.max(items[i].min))
                            .max(0.0);
                        violation += c - raw;
                        clamped.push((i, c));
                    }
                    for (i, c) in &clamped {
                        items[*i].target = *c;
                    }
                    let freeze_kind = |i: usize, items: &Vec<FItem>| -> bool {
                        let t = items[i].target;
                        if violation > 0.0 {
                            t <= items[i].min + 0.001
                        } else if violation < 0.0 {
                            t >= items[i].max - 0.001
                        } else {
                            true
                        }
                    };
                    let to_freeze: Vec<usize> = unfrozen
                        .iter()
                        .copied()
                        .filter(|&i| freeze_kind(i, &items))
                        .collect();
                    if violation == 0.0 || to_freeze.is_empty() {
                        for &i in &unfrozen {
                            items[i].frozen = true;
                        }
                        break;
                    }
                    for i in to_freeze {
                        items[i].frozen = true;
                    }
                }
            }
        }
        // Lay every item out at its final main size and find the cross sizes.
        let mut line_cross: Vec<f32> = Vec::new();
        for line in &lines {
            let mut cross = 0.0f32;
            for &i in line {
                let b = if row {
                    self.layout_item(&items[i], content_w, items[i].target, None)
                } else {
                    let w = cross_w(&items[i]);
                    self.layout_item(
                        &items[i],
                        content_w,
                        w,
                        if def_h.is_some() {
                            Some(items[i].target)
                        } else {
                            None
                        },
                    )
                };
                let outer_cross = if row {
                    b.rect.h + items[i].margin[0] + items[i].margin[2]
                } else {
                    b.rect.w + items[i].margin[1] + items[i].margin[3]
                };
                cross = cross.max(outer_cross);
                items[i].lbox = Some(b);
            }
            line_cross.push(cross);
        }
        let cross_def = if row { def_h } else { Some(content_w) };
        let total_cross_content: f32 =
            line_cross.iter().sum::<f32>() + gap_cross * (lines.len().saturating_sub(1)) as f32;
        if lines.len() == 1 {
            if let Some(c) = cross_def {
                line_cross[0] = c;
            }
        } else if let Some(c) = cross_def {
            let extra = (c - total_cross_content).max(0.0) / lines.len() as f32;
            for l in &mut line_cross {
                *l += extra;
            }
        }
        // Position.
        let mut cross_pos = 0.0f32;
        let mut order: Vec<usize> = (0..lines.len()).collect();
        if style.flex_wrap == FlexWrap::WrapReverse {
            order.reverse();
        }
        let mut max_main_used = 0.0f32;
        for li in order {
            let line = &lines[li];
            let cross_size = line_cross[li];
            let gaps = gap_main * (line.len().saturating_sub(1)) as f32;
            let outer_sum: f32 = line
                .iter()
                .map(|&i| items[i].outer_main(items[i].target, row))
                .sum::<f32>()
                + gaps;
            let leftover = if main_avail.is_finite() {
                (main_avail - outer_sum).max(0.0)
            } else {
                0.0
            };
            let auto_count = line
                .iter()
                .map(|&i| {
                    let (a, b) = if row {
                        (items[i].auto_m[3], items[i].auto_m[1])
                    } else {
                        (items[i].auto_m[0], items[i].auto_m[2])
                    };
                    usize::from(a) + usize::from(b)
                })
                .sum::<usize>();
            let (mut start, between) = if auto_count > 0 {
                (0.0, 0.0)
            } else {
                let justify = match (reverse, style.justify) {
                    (true, Justify::Start) => Justify::End,
                    (true, Justify::End) => Justify::Start,
                    (_, j) => j,
                };
                match justify {
                    Justify::Start => (0.0, 0.0),
                    Justify::End => (leftover, 0.0),
                    Justify::Center => (leftover / 2.0, 0.0),
                    Justify::SpaceBetween if line.len() > 1 => {
                        (0.0, leftover / (line.len() - 1) as f32)
                    }
                    Justify::SpaceBetween => (0.0, 0.0),
                    Justify::SpaceAround => (
                        leftover / line.len() as f32 / 2.0,
                        leftover / line.len() as f32,
                    ),
                    Justify::SpaceEvenly => (
                        leftover / (line.len() + 1) as f32,
                        leftover / (line.len() + 1) as f32,
                    ),
                }
            };
            let seq: Vec<usize> = if reverse {
                line.iter().rev().copied().collect()
            } else {
                line.clone()
            };
            let mut pos = start;
            for (k, &i) in seq.iter().enumerate() {
                let share = if auto_count > 0 {
                    leftover / auto_count as f32
                } else {
                    0.0
                };
                let (ms, me) = if row { (3, 1) } else { (0, 2) };
                let m_start = items[i].margin[ms] + if items[i].auto_m[ms] { share } else { 0.0 };
                let m_end = items[i].margin[me] + if items[i].auto_m[me] { share } else { 0.0 };
                let Some(mut b) = items[i].lbox.take() else {
                    continue;
                };
                let (cs, ce) = if row { (0, 2) } else { (3, 1) };
                let c_start = items[i].margin[cs];
                let c_end = items[i].margin[ce];
                let outer_cross = (if row { b.rect.h } else { b.rect.w }) + c_start + c_end;
                let align = items[i].style.align_self.unwrap_or(style.align_items);
                let cross_auto = if row {
                    items[i].style.height == Size::Auto
                } else {
                    items[i].style.width == Size::Auto
                };
                let auto_c = (items[i].auto_m[cs], items[i].auto_m[ce]);
                if align == Align::Stretch && cross_auto && !auto_c.0 && !auto_c.1 {
                    let target_cross = (cross_size - c_start - c_end).max(0.0);
                    if row {
                        let delta = target_cross - b.rect.h;
                        if delta > 0.0 {
                            b.rect.h = target_cross;
                        }
                    } else {
                        b.rect.w = target_cross.max(b.rect.w);
                    }
                }
                let free_c = (cross_size - outer_cross).max(0.0);
                let cross_off = match (auto_c.0, auto_c.1, align) {
                    (true, true, _) => free_c / 2.0,
                    (true, false, _) => free_c,
                    (false, true, _) => 0.0,
                    (_, _, Align::End) => free_c,
                    (_, _, Align::Center) => free_c / 2.0,
                    _ => 0.0,
                };
                let main_pos = pos + m_start;
                if row {
                    b.rect.x = main_pos;
                    b.rect.y = cross_pos + cross_off + c_start;
                } else {
                    b.rect.y = main_pos;
                    b.rect.x = cross_pos + cross_off + c_start;
                }
                let main_size = if row { b.rect.w } else { b.rect.h };
                pos = main_pos + main_size + m_end + gap_main + between;
                let _ = k;
                max_main_used = max_main_used.max(main_pos + main_size + m_end);
                self.finish_child(&mut b, content_w);
                flow.pending
                    .extend(std::mem::take(&mut b.pending).into_iter().map(|mut p| {
                        p.static_x += b.rect.x;
                        p.static_y += b.rect.y;
                        p
                    }));
                flow.children.push(b);
            }
            start = 0.0;
            let _ = start;
            cross_pos += cross_size + gap_cross;
        }
        let cross_total = cross_pos - gap_cross.min(cross_pos);
        flow.height = if row { cross_total } else { max_main_used };
        if !row {
            flow.height = def_h.unwrap_or(max_main_used);
        }
        flow.used_w = if row { max_main_used } else { content_w };
        flow.first_baseline = flow
            .children
            .first()
            .and_then(|c| c.first_baseline.map(|f| f + c.rect.y));
        flow
    }

    fn grid_intrinsic(&self, n: NodeId) -> (f32, f32) {
        // Widest minimum of any child, and every child side by side as the most it could want.
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for c in self.visible_children(n) {
            if self.dom.tag(c).is_some() {
                let (a, b) = self.intrinsic(c);
                lo = lo.max(a);
                hi += b;
            }
        }
        (lo, hi)
    }

    // -------------------------------------------------------------- grid

    /// Expands a template into its tracks for a container `avail` pixels long.
    fn grid_tracks(&self, t: &GridTemplate, avail: f32, gap: f32) -> Vec<Track> {
        let mut tracks = t.tracks.clone();
        if let Some((at, tr, _)) = t.repeat {
            let base = match (tr.min, tr.max) {
                (TrackSize::Px(p), _) | (_, TrackSize::Px(p)) => p,
                (TrackSize::Pct(f), _) | (_, TrackSize::Pct(f)) => f * avail,
                _ => 0.0,
            };
            let fixed: f32 = t
                .tracks
                .iter()
                .map(|k| match k.min {
                    TrackSize::Px(p) => p,
                    TrackSize::Pct(f) => f * avail,
                    _ => 0.0,
                })
                .sum();
            let n = if base > 0.0 {
                (((avail - fixed + gap) / (base + gap)).floor().max(1.0) as usize).min(200)
            } else {
                1
            };
            let at = at.min(tracks.len());
            for _ in 0..n {
                tracks.insert(at, tr);
            }
        }
        tracks
    }

    /// Reads where an item sits along one axis: the first track (if given) and how many it spans.
    fn grid_axis(line: GridLine, explicit: usize) -> (Option<usize>, usize) {
        let idx = |v: i32| -> usize {
            if v > 0 {
                (v - 1) as usize
            } else {
                (explicit as i32 + 1 + v).max(0) as usize
            }
        };
        match (line.start, line.end, line.span) {
            (Some(a), Some(b), _) => {
                let (a, b) = (idx(a), idx(b));
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                (Some(lo), (hi - lo).max(1))
            }
            (Some(a), None, sp) => (Some(idx(a)), sp.map_or(1, |n| n as usize)),
            (None, Some(b), sp) => {
                let end = idx(b);
                let span = sp.map_or(1, |n| n as usize);
                (Some(end.saturating_sub(span)), span)
            }
            (None, None, sp) => (None, sp.map_or(1, |n| n as usize)),
        }
    }

    fn layout_grid(
        &self,
        node: NodeId,
        style: &Rc<Style>,
        content_w: f32,
        def_h: Option<f32>,
    ) -> Flow {
        let (mut items, pending) = self.flex_items(node, style, content_w, true, def_h);
        items.sort_by_key(|i| i.order);
        let mut flow = Flow {
            pending,
            ..Flow::default()
        };
        if items.is_empty() || !self.room() {
            flow.empty = true;
            if let Some(h) = def_h {
                flow.height = h;
            }
            return flow;
        }
        let gap_c = dim_px(style.column_gap, content_w).max(0.0);
        let gap_r = dim_px(style.row_gap, def_h.unwrap_or(0.0)).max(0.0);
        let mut col_tracks = self.grid_tracks(&style.grid_columns, content_w, gap_c);
        let explicit_cols = col_tracks.len();
        let row_template = self.grid_tracks(&style.grid_rows, def_h.unwrap_or(0.0), gap_r);
        let explicit_rows = row_template.len();
        // Place every item in a cell range.
        struct Cell {
            c: usize,
            r: usize,
            cs: usize,
            rs: usize,
        }
        let mut ncols = explicit_cols.max(1);
        type Axis = (Option<usize>, usize);
        let axes: Vec<(Axis, Axis)> = items
            .iter()
            .map(|it| {
                (
                    Self::grid_axis(it.style.grid_column, explicit_cols.max(1)),
                    Self::grid_axis(it.style.grid_row, explicit_rows),
                )
            })
            .collect();
        for ((cs, csp), _) in &axes {
            ncols = ncols.max(cs.map_or(0, |c| c + csp)).max(*csp);
        }
        ncols = ncols.min(1000);
        while col_tracks.len() < ncols {
            col_tracks.push(style.grid_auto_columns);
        }
        let mut occ: Vec<Vec<bool>> = Vec::new();
        let free = |occ: &Vec<Vec<bool>>, r: usize, c: usize, rs: usize, cs: usize| -> bool {
            (r..r + rs).all(|y| {
                occ.get(y)
                    .is_none_or(|row| (c..c + cs).all(|x| !row.get(x).copied().unwrap_or(false)))
            })
        };
        let mark = |occ: &mut Vec<Vec<bool>>, r: usize, c: usize, rs: usize, cs: usize| {
            while occ.len() < r + rs {
                occ.push(vec![false; ncols]);
            }
            for row in occ.iter_mut().skip(r).take(rs) {
                for cell in row.iter_mut().take((c + cs).min(ncols)).skip(c) {
                    *cell = true;
                }
            }
        };
        let mut cells: Vec<Option<Cell>> = Vec::new();
        let (mut cur_r, mut cur_c) = (0usize, 0usize);
        for ((cstart, cspan), (rstart, rspan)) in &axes {
            let cs = (*cspan).clamp(1, ncols);
            let rs = (*rspan).clamp(1, 1000);
            let placed = match (cstart, rstart) {
                (Some(c), Some(r)) => Some((*r, (*c).min(ncols - cs))),
                (Some(c), None) => {
                    let c = (*c).min(ncols - cs);
                    let mut r = cur_r;
                    while r < 1000 && !free(&occ, r, c, rs, cs) {
                        r += 1;
                    }
                    (r < 1000).then_some((r, c))
                }
                (None, Some(r)) => {
                    let mut c = 0;
                    while c + cs <= ncols && !free(&occ, *r, c, rs, cs) {
                        c += 1;
                    }
                    (c + cs <= ncols).then_some((*r, c))
                }
                (None, None) => {
                    let (mut r, mut c) = (cur_r, cur_c);
                    loop {
                        if c + cs > ncols {
                            r += 1;
                            c = 0;
                        }
                        if r >= 1000 {
                            break None;
                        }
                        if free(&occ, r, c, rs, cs) {
                            break Some((r, c));
                        }
                        c += 1;
                    }
                }
            };
            match placed {
                Some((r, c)) => {
                    mark(&mut occ, r, c, rs, cs);
                    if cstart.is_none() || rstart.is_none() {
                        cur_r = r;
                        cur_c = c + cs;
                    }
                    cells.push(Some(Cell { c, r, cs, rs }));
                }
                None => cells.push(None),
            }
        }
        let nrows = occ.len().max(explicit_rows).max(1);
        // Column widths.
        let content_extra = |it: &FItem| it.extras(true) + it.margin[1] + it.margin[3];
        let item_range = |it: &FItem| -> (f32, f32) {
            let (lo, hi) = match it.elem {
                Some(e) => self.intrinsic(e),
                None => {
                    let (lo, hi) = atoms_intrinsic(&self.collect_atoms(
                        &[it.text.unwrap_or(0)],
                        &it.style,
                        None,
                    ));
                    (lo, hi)
                }
            };
            let extra = content_extra(it);
            (lo + extra, hi + extra)
        };
        let total_gap_c = gap_c * (ncols - 1) as f32;
        let avail_c = (content_w - total_gap_c).max(0.0);
        let mut width = vec![0.0f32; ncols];
        let mut floor = vec![0.0f32; ncols];
        let mut fr = vec![0.0f32; ncols];
        let mut auto = vec![false; ncols];
        for (k, t) in col_tracks.iter().enumerate().take(ncols) {
            let px = |s: TrackSize| match s {
                TrackSize::Px(p) => Some(p),
                TrackSize::Pct(f) => Some(f * content_w),
                _ => None,
            };
            let content_fit = |want_hi: bool| -> f32 {
                items
                    .iter()
                    .zip(&cells)
                    .filter_map(|(it, c)| c.as_ref().filter(|c| c.c == k && c.cs == 1).map(|_| it))
                    .map(|it| {
                        let (lo, hi) = item_range(it);
                        if want_hi {
                            hi
                        } else {
                            lo
                        }
                    })
                    .fold(0.0f32, f32::max)
            };
            match t.max {
                TrackSize::Fr(f) => {
                    fr[k] = f.max(0.0);
                    floor[k] = px(t.min).unwrap_or_else(|| content_fit(false));
                }
                TrackSize::Auto => {
                    auto[k] = true;
                    width[k] = px(t.min).unwrap_or(0.0).max(content_fit(true).min(avail_c));
                }
                other => {
                    let max = px(other).unwrap_or(0.0);
                    let min = px(t.min).unwrap_or(max);
                    width[k] = max.max(min);
                }
            }
        }
        let fixed_sum: f32 = width.iter().sum();
        let total_fr: f32 = fr.iter().sum();
        if total_fr > 0.0 {
            let mut frozen = vec![false; ncols];
            let mut leftover = (avail_c - fixed_sum).max(0.0);
            for _ in 0..ncols {
                let denom: f32 = fr
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| !frozen[*k])
                    .map(|(_, f)| *f)
                    .sum::<f32>()
                    .max(1.0);
                let size = leftover / denom;
                let mut changed = false;
                for k in 0..ncols {
                    if fr[k] > 0.0 && !frozen[k] && fr[k] * size < floor[k] {
                        frozen[k] = true;
                        width[k] = floor[k];
                        leftover = (leftover - floor[k]).max(0.0);
                        changed = true;
                    }
                }
                if !changed {
                    for k in 0..ncols {
                        if fr[k] > 0.0 && !frozen[k] {
                            width[k] = fr[k] * size;
                        }
                    }
                    break;
                }
            }
        } else {
            // No flexible tracks: automatic tracks share what is left.
            let n_auto = auto.iter().filter(|a| **a).count();
            let leftover = avail_c - fixed_sum;
            if n_auto > 0 && leftover > 0.0 {
                for k in 0..ncols {
                    if auto[k] {
                        width[k] += leftover / n_auto as f32;
                    }
                }
            }
        }
        let mut xs = vec![0.0f32; ncols];
        for k in 1..ncols {
            xs[k] = xs[k - 1] + width[k - 1] + gap_c;
        }
        // Lay every item out at the width of its cells and read its height.
        let span_w = |c: &Cell| -> f32 {
            width[c.c..c.c + c.cs].iter().sum::<f32>() + gap_c * (c.cs - 1) as f32
        };
        let mut boxes: Vec<Option<LBox>> = Vec::new();
        for (it, cell) in items.iter().zip(&cells) {
            let Some(c) = cell else {
                boxes.push(None);
                continue;
            };
            let cell_w = span_w(c);
            let w = match it.style.width {
                Size::Dim(d) => {
                    let v = dim_px(d, content_w);
                    if it.style.border_box {
                        v - it.extras(true)
                    } else {
                        v
                    }
                }
                Size::Auto => cell_w - content_extra(it),
            };
            boxes.push(Some(self.layout_item(it, content_w, w.max(0.0), None)));
        }
        // Row heights.
        let mut heights = vec![0.0f32; nrows];
        let mut row_auto = vec![true; nrows];
        let mut row_fr = vec![0.0f32; nrows];
        for r in 0..nrows {
            let t = row_template.get(r).copied().unwrap_or(style.grid_auto_rows);
            let px = |s: TrackSize| match s {
                TrackSize::Px(p) => Some(p),
                TrackSize::Pct(f) => def_h.map(|h| f * h),
                _ => None,
            };
            match t.max {
                TrackSize::Fr(f) if def_h.is_some() => {
                    row_fr[r] = f;
                    heights[r] = px(t.min).unwrap_or(0.0);
                }
                TrackSize::Px(_) | TrackSize::Pct(_) if px(t.max).is_some() => {
                    heights[r] = px(t.max).unwrap_or(0.0).max(px(t.min).unwrap_or(0.0));
                    row_auto[r] = false;
                }
                _ => heights[r] = px(t.min).unwrap_or(0.0),
            }
        }
        let outer_h = |it: &FItem, b: &LBox| b.rect.h + it.margin[0] + it.margin[2];
        for ((it, cell), b) in items.iter().zip(&cells).zip(&boxes) {
            if let (Some(c), Some(b)) = (cell, b) {
                if c.rs == 1 && row_auto[c.r] {
                    heights[c.r] = heights[c.r].max(outer_h(it, b));
                }
            }
        }
        for ((it, cell), b) in items.iter().zip(&cells).zip(&boxes) {
            if let (Some(c), Some(b)) = (cell, b) {
                if c.rs > 1 {
                    let have: f32 =
                        heights[c.r..c.r + c.rs].iter().sum::<f32>() + gap_r * (c.rs - 1) as f32;
                    let need = outer_h(it, b);
                    if need > have {
                        heights[c.r + c.rs - 1] += need - have;
                    }
                }
            }
        }
        let total_gap_r = gap_r * (nrows - 1) as f32;
        if let Some(h) = def_h {
            let used: f32 = heights.iter().sum::<f32>() + total_gap_r;
            let fr_total: f32 = row_fr.iter().sum();
            if fr_total > 0.0 && h > used {
                let share = (h - used) / fr_total;
                for r in 0..nrows {
                    heights[r] += row_fr[r] * share;
                }
            }
        }
        let mut ys = vec![0.0f32; nrows];
        for r in 1..nrows {
            ys[r] = ys[r - 1] + heights[r - 1] + gap_r;
        }
        // Position the items in their cells.
        for ((it, cell), b) in items.iter().zip(&cells).zip(boxes) {
            let (Some(c), Some(mut b)) = (cell, b) else {
                continue;
            };
            let cell_h = heights[c.r..c.r + c.rs].iter().sum::<f32>() + gap_r * (c.rs - 1) as f32;
            let align = it.style.align_self.unwrap_or(style.align_items);
            let avail_h = (cell_h - it.margin[0] - it.margin[2]).max(0.0);
            let mut dy = 0.0;
            if align == Align::Stretch && it.style.height == Size::Auto {
                if avail_h > b.rect.h {
                    b.rect.h = avail_h;
                }
            } else {
                let spare = (avail_h - b.rect.h).max(0.0);
                dy = match align {
                    Align::End => spare,
                    Align::Center => spare / 2.0,
                    _ => 0.0,
                };
            }
            b.rect.x = xs[c.c] + it.margin[3];
            b.rect.y = ys[c.r] + it.margin[0] + dy;
            self.finish_child(&mut b, content_w);
            flow.pending
                .extend(std::mem::take(&mut b.pending).into_iter().map(|mut p| {
                    p.static_x += b.rect.x;
                    p.static_y += b.rect.y;
                    p
                }));
            flow.children.push(b);
        }
        flow.height = heights.iter().sum::<f32>() + total_gap_r;
        flow.used_w = xs.last().copied().unwrap_or(0.0) + width.last().copied().unwrap_or(0.0);
        flow.first_baseline = flow
            .children
            .first()
            .and_then(|c| c.first_baseline.map(|f| f + c.rect.y));
        flow
    }

    // -------------------------------------------------------------- flexbox helpers

    fn flex_intrinsic(&self, n: NodeId, style: &Rc<Style>) -> (f32, f32) {
        let row = matches!(style.flex_direction, FlexDir::Row | FlexDir::RowReverse);
        let gap = if row { style.column_gap.px } else { 0.0 };
        let kids: Vec<(f32, f32)> = self
            .visible_children(n)
            .into_iter()
            .filter(|&c| !self.is_abs(c))
            .filter_map(|c| {
                if self.dom.tag(c).is_some() {
                    let s = self.style_of(c)?;
                    let (a, b) = self.intrinsic(c);
                    let e = self.extras_w(&s);
                    Some((a + e, b + e))
                } else if self.dom.text(c).is_some_and(|t| !t.trim().is_empty()) {
                    let atoms = self.collect_atoms(&[c], style, None);
                    Some(atoms_intrinsic(&atoms))
                } else {
                    None
                }
            })
            .collect();
        let n_gaps = gap * kids.len().saturating_sub(1) as f32;
        if row {
            let wrap = style.flex_wrap != FlexWrap::NoWrap;
            let min = if wrap {
                kids.iter().map(|k| k.0).fold(0.0, f32::max)
            } else {
                kids.iter().map(|k| k.0).sum::<f32>() + n_gaps
            };
            (min, kids.iter().map(|k| k.1).sum::<f32>() + n_gaps)
        } else {
            (
                kids.iter().map(|k| k.0).fold(0.0, f32::max),
                kids.iter().map(|k| k.1).fold(0.0, f32::max),
            )
        }
    }
}

// ------------------------------------------------------------------ tables

struct CellInfo {
    node: NodeId,
    colspan: usize,
    style: Rc<Style>,
}

struct RowInfo {
    node: NodeId,
    style: Rc<Style>,
    cells: Vec<CellInfo>,
}

impl Ctx<'_> {
    fn table_rows(&self, table: NodeId) -> Vec<RowInfo> {
        let mut rows = Vec::new();
        let take_row = |me: &Self, r: NodeId, rows: &mut Vec<RowInfo>| {
            let Some(rs) = me.style_of(r) else { return };
            let cells: Vec<CellInfo> = me
                .dom
                .element_children(r)
                .into_iter()
                .filter_map(|c| {
                    let s = me.style_of(c)?;
                    (s.display == Display::TableCell).then(|| CellInfo {
                        node: c,
                        colspan: me
                            .dom
                            .attr(c, "colspan")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 100),
                        style: s,
                    })
                })
                .collect();
            rows.push(RowInfo {
                node: r,
                style: rs,
                cells,
            });
        };
        for c in self.dom.element_children(table) {
            let Some(s) = self.style_of(c) else { continue };
            match s.display {
                Display::TableRow => take_row(self, c, &mut rows),
                Display::TableRowGroup => {
                    for r in self.dom.element_children(c) {
                        if self
                            .style_of(r)
                            .is_some_and(|x| x.display == Display::TableRow)
                        {
                            take_row(self, r, &mut rows);
                        }
                    }
                }
                _ => {}
            }
        }
        rows
    }

    /// Minimum and maximum width of each column, from the cells that span one column.
    fn column_extents(&self, rows: &[RowInfo]) -> (Vec<f32>, Vec<f32>) {
        let cols = rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.colspan).sum::<usize>())
            .max()
            .unwrap_or(0);
        let mut min = vec![0.0f32; cols];
        let mut max = vec![0.0f32; cols];
        for r in rows {
            let mut col = 0;
            for c in &r.cells {
                let (lo, hi) = self.intrinsic(c.node);
                let e = self.extras_w(&c.style);
                let (lo, hi) = (lo + e, hi + e);
                if c.colspan == 1 {
                    min[col] = min[col].max(lo);
                    max[col] = max[col].max(hi);
                }
                col += c.colspan;
            }
        }
        // Cells that span columns widen the columns they cover when they need more room.
        for r in rows {
            let mut col = 0;
            for c in &r.cells {
                if c.colspan > 1 && col + c.colspan <= cols {
                    let (lo, hi) = self.intrinsic(c.node);
                    let e = self.extras_w(&c.style);
                    let have_min: f32 = min[col..col + c.colspan].iter().sum();
                    let have_max: f32 = max[col..col + c.colspan].iter().sum();
                    for k in col..col + c.colspan {
                        if lo + e > have_min {
                            min[k] += (lo + e - have_min) / c.colspan as f32;
                        }
                        if hi + e > have_max {
                            max[k] += (hi + e - have_max) / c.colspan as f32;
                        }
                    }
                }
                col += c.colspan;
            }
        }
        (min, max)
    }

    fn table_intrinsic(&self, n: NodeId) -> (f32, f32) {
        let Some(style) = self.style_of(n) else {
            return (0.0, 0.0);
        };
        let rows = self.table_rows(n);
        let (min, max) = self.column_extents(&rows);
        let spacing = if style.border_collapse {
            0.0
        } else {
            style.border_spacing
        };
        let extra = spacing * (min.len() + 1) as f32;
        (
            min.iter().sum::<f32>() + extra,
            max.iter().sum::<f32>() + extra,
        )
    }

    fn layout_table(&self, node: NodeId, style: &Rc<Style>, content_w: f32) -> Flow {
        let rows = self.table_rows(node);
        let mut flow = Flow::default();
        let (min, max) = self.column_extents(&rows);
        let cols = min.len();
        if cols == 0 || !self.room() {
            flow.empty = true;
            return flow;
        }
        let spacing = if style.border_collapse {
            0.0
        } else {
            style.border_spacing
        };
        let avail = (content_w - spacing * (cols + 1) as f32).max(0.0);
        let (sum_min, sum_max) = (min.iter().sum::<f32>(), max.iter().sum::<f32>());
        // Column widths: between the minimum and maximum, shared out by how much each wants.
        let mut widths: Vec<f32> = if sum_max <= avail {
            let extra = if style.width.dim().is_some() && sum_max > 0.0 {
                avail - sum_max
            } else {
                0.0
            };
            max.iter()
                .map(|m| {
                    m + if sum_max > 0.0 {
                        extra * m / sum_max
                    } else {
                        0.0
                    }
                })
                .collect()
        } else if sum_min >= avail {
            min.clone()
        } else {
            let t = (avail - sum_min) / (sum_max - sum_min);
            min.iter()
                .zip(&max)
                .map(|(lo, hi)| lo + (hi - lo) * t)
                .collect()
        };
        // Cells with a stated width ask for it.
        for r in &rows {
            let mut col = 0;
            for c in &r.cells {
                if c.colspan == 1 {
                    if let Some(d) = c.style.width.dim() {
                        let want = dim_px(d, avail)
                            + if c.style.border_box {
                                0.0
                            } else {
                                self.extras_w(&c.style)
                            };
                        if want.is_finite() && want > widths[col] && want <= avail {
                            widths[col] = want.max(min[col]);
                        }
                    }
                }
                col += c.colspan;
            }
        }
        let table_w = widths.iter().sum::<f32>() + spacing * (cols + 1) as f32;
        let mut y = spacing;
        let mut first_baseline = None;
        // Captions go above the rows.
        for c in self.dom.element_children(node) {
            if self.dom.tag(c) == Some("caption") {
                if let Some(cs) = self.style_of(c) {
                    let mut cb = self.layout_block(c, &cs, table_w, None, None);
                    cb.rect.y = y;
                    y += cb.rect.h;
                    flow.children.push(cb);
                }
            }
        }
        for r in &rows {
            let mut cells: Vec<(LBox, f32, usize)> = Vec::new();
            let mut col = 0;
            let mut row_h = 0.0f32;
            let mut x = spacing;
            for c in &r.cells {
                let w: f32 = widths[col..(col + c.colspan).min(cols)].iter().sum::<f32>()
                    + spacing * (c.colspan.saturating_sub(1)) as f32;
                let (border, padding) = self.pad_border(&c.style, w);
                let extras = border[1] + border[3] + padding[1] + padding[3];
                let b = self.layout_block(
                    c.node,
                    &c.style,
                    w,
                    None,
                    Some(Forced {
                        w: Some((w - extras).max(0.0)),
                        h: None,
                        bfc: true,
                    }),
                );
                row_h = row_h.max(b.rect.h);
                cells.push((b, x, col));
                x += w + spacing;
                col += c.colspan;
            }
            let mut rb = LBox::new(Some(r.node), r.style.clone(), BoxKind::Block);
            rb.rect = Rect {
                x: 0.0,
                y,
                w: table_w,
                h: row_h,
            };
            for (mut b, cx, _) in cells {
                let delta = row_h - b.rect.h;
                if delta > 0.0 {
                    let shift = match b.style.vertical_align {
                        VAlign::Middle => delta / 2.0,
                        VAlign::Bottom => delta,
                        _ => 0.0,
                    };
                    b.rect.h = row_h;
                    if shift > 0.0 {
                        shift_content(&mut b, shift);
                    }
                }
                b.rect.x = cx;
                b.rect.y = 0.0;
                rb.children.push(b);
            }
            if first_baseline.is_none() {
                first_baseline = rb
                    .children
                    .first()
                    .and_then(|c| c.first_baseline)
                    .map(|f| f + y);
            }
            y += row_h + spacing;
            flow.children.push(rb);
        }
        flow.height = y;
        flow.used_w = table_w;
        flow.first_baseline = first_baseline;
        flow
    }
}

/// The border box of an element in page coordinates, if it has a box.
#[must_use]
pub fn node_rect(layout: &Layout, node: NodeId) -> Option<Rect> {
    fn walk(b: &LBox, ox: f32, oy: f32, node: NodeId) -> Option<Rect> {
        let (x, y) = (ox + b.rect.x, oy + b.rect.y);
        if b.node == Some(node) {
            return Some(Rect {
                x,
                y,
                w: b.rect.w,
                h: b.rect.h,
            });
        }
        for c in &b.children {
            if let Some(r) = walk(c, x, y, node) {
                return Some(r);
            }
        }
        let (cx, cy) = (x + b.content_x(), y + b.content_y());
        for l in &b.lines {
            for it in &l.items {
                if let Item::Atomic {
                    x: ix,
                    y: iy,
                    b: ab,
                } = it
                {
                    if let Some(r) = walk(ab, cx + ix, cy + l.y + iy, node) {
                        return Some(r);
                    }
                }
            }
        }
        None
    }
    walk(&layout.root, 0.0, 0.0, node)
}

/// Finds the element (innermost first) under a point, in page coordinates.
#[must_use]
pub fn hit_test(layout: &Layout, x: f32, y: f32) -> Option<NodeId> {
    fn walk(b: &LBox, ox: f32, oy: f32, x: f32, y: f32) -> Option<NodeId> {
        let (bx, by) = (ox + b.rect.x, oy + b.rect.y);
        let inside = Rect {
            x: bx,
            y: by,
            w: b.rect.w,
            h: b.rect.h,
        }
        .contains(x, y);
        if b.clips() && !inside {
            return None;
        }
        let (cx, cy) = (bx + b.content_x(), by + b.content_y());
        for c in b.children.iter().rev() {
            if let Some(n) = walk(c, bx, by, x, y) {
                return Some(n);
            }
        }
        for l in b.lines.iter().rev() {
            for it in l.items.iter().rev() {
                match it {
                    Item::Atomic {
                        x: ix,
                        y: iy,
                        b: ab,
                    } => {
                        if let Some(n) = walk(ab, cx + ix, cy + l.y + iy, x, y) {
                            return Some(n);
                        }
                    }
                    Item::Text { x: ix, w, node, .. } => {
                        if x >= cx + ix && x < cx + ix + w && y >= cy + l.y && y < cy + l.y + l.h {
                            if let Some(n) = node {
                                return Some(*n);
                            }
                        }
                    }
                    Item::Span { .. } => {}
                }
            }
        }
        if inside {
            return b.node;
        }
        None
    }
    // Empty page below the content belongs to the root element (the canvas).
    walk(&layout.root, 0.0, 0.0, x, y).or(layout.root.node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{State, Stylesheet};
    use crate::style::compute;

    /// Every character is half the font size wide; ascent 0.8 and descent 0.2 of the size, and
    /// a normal line is 1.25 times the size.
    struct TestFonts;

    impl Fonts for TestFonts {
        fn text_width(&self, text: &str, f: &FontSpec) -> f32 {
            text.chars().count() as f32 * (f.size / 2.0 + f.letter_spacing)
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

    fn page(html: &str, css: &str, w: f32) -> (Dom, Layout) {
        let dom = Dom::parse(html);
        let mut sheets = dom
            .style_sheets()
            .iter()
            .map(|s| Stylesheet::parse(s))
            .collect::<Vec<_>>();
        if !css.is_empty() {
            sheets.push(Stylesheet::parse(css));
        }
        let vp = Viewport { w, h: 600.0 };
        let styles = compute(&dom, &sheets, vp, &State::default());
        let l = layout(&dom, &styles, &TestFonts, vp);
        (dom, l)
    }

    /// The border box of an element in page coordinates.
    fn rect_of(layout: &Layout, node: NodeId) -> Option<Rect> {
        fn walk(b: &LBox, ox: f32, oy: f32, node: NodeId) -> Option<Rect> {
            let (x, y) = (ox + b.rect.x, oy + b.rect.y);
            if b.node == Some(node) {
                return Some(Rect {
                    x,
                    y,
                    w: b.rect.w,
                    h: b.rect.h,
                });
            }
            for c in &b.children {
                if let Some(r) = walk(c, x, y, node) {
                    return Some(r);
                }
            }
            let (cx, cy) = (x + b.content_x(), y + b.content_y());
            for l in &b.lines {
                for it in &l.items {
                    if let Item::Atomic {
                        x: ix,
                        y: iy,
                        b: ab,
                    } = it
                    {
                        if let Some(r) = walk(ab, cx + ix, cy + l.y + iy, node) {
                            return Some(r);
                        }
                    }
                }
            }
            None
        }
        walk(&layout.root, 0.0, 0.0, node)
    }

    fn r(dom: &Dom, l: &Layout, id: &str) -> Rect {
        rect_of(l, dom.by_id(id).unwrap_or_else(|| panic!("no #{id}")))
            .unwrap_or_else(|| panic!("no box for #{id}"))
    }

    fn assert_rect(got: Rect, x: f32, y: f32, w: f32, h: f32) {
        let ok = (got.x - x).abs() < 0.01
            && (got.y - y).abs() < 0.01
            && (got.w - w).abs() < 0.01
            && (got.h - h).abs() < 0.01;
        assert!(ok, "got {got:?}, want x={x} y={y} w={w} h={h}");
    }

    fn lines_of(l: &Layout, dom: &Dom, id: &str) -> Vec<String> {
        fn find(b: &LBox, node: NodeId) -> Option<&LBox> {
            if b.node == Some(node) {
                return Some(b);
            }
            b.children.iter().find_map(|c| find(c, node))
        }
        fn texts(b: &LBox, out: &mut Vec<String>) {
            for ln in &b.lines {
                let s: String = ln
                    .items
                    .iter()
                    .filter_map(|i| {
                        if let Item::Text { text, .. } = i {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                out.push(s);
            }
            for c in &b.children {
                texts(c, out);
            }
        }
        let b = find(&l.root, dom.by_id(id).unwrap()).unwrap();
        let mut out = Vec::new();
        texts(b, &mut out);
        out
    }

    #[test]
    fn a_block_fills_the_width_and_the_body_has_its_margin() {
        let (d, l) = page(r#"<div id="a" style="height:50px">x</div>"#, "", 800.0);
        assert_rect(r(&d, &l, "a"), 8.0, 8.0, 784.0, 50.0);
        assert_rect(
            rect_of(&l, d.body().unwrap()).unwrap(),
            8.0,
            8.0,
            784.0,
            50.0,
        );
        assert!((l.height - 66.0).abs() < 0.01, "{}", l.height);
    }

    #[test]
    fn widths_percentages_auto_margins_and_box_sizing() {
        let css = "body{margin:0} #a{width:50%;margin:0 auto;height:10px} #b{width:200px;padding:10px;border:5px solid black;height:10px} #c{width:200px;padding:10px;border:5px solid black;box-sizing:border-box;height:10px} #d{width:100px;margin-left:auto;height:10px} #e{max-width:300px;min-width:50px;height:10px} #f{width:calc(100% - 100px);height:10px}";
        let (d, l) = page(
            r#"<div id="a"></div><div id="b"></div><div id="c"></div><div id="d"></div><div id="e"></div><div id="f"></div>"#,
            css,
            800.0,
        );
        assert_rect(r(&d, &l, "a"), 200.0, 0.0, 400.0, 10.0);
        assert_rect(r(&d, &l, "b"), 0.0, 10.0, 230.0, 40.0);
        assert_rect(r(&d, &l, "c"), 0.0, 50.0, 200.0, 30.0);
        assert_rect(r(&d, &l, "d"), 700.0, 80.0, 100.0, 10.0);
        assert_rect(r(&d, &l, "e"), 0.0, 90.0, 300.0, 10.0);
        assert_rect(r(&d, &l, "f"), 0.0, 100.0, 700.0, 10.0);
    }

    #[test]
    fn vertical_margins_collapse_between_siblings_and_through_parents() {
        let css = "body{margin:0} p{margin:16px 0;height:20px} #pad{padding:1px}";
        let (d, l) = page(
            r#"<p id="p1"></p><p id="p2"></p><div id="outer"><p id="p3"></p></div>"#,
            css,
            400.0,
        );
        assert_rect(r(&d, &l, "p1"), 0.0, 16.0, 400.0, 20.0);
        assert_rect(r(&d, &l, "p2"), 0.0, 52.0, 400.0, 20.0);
        // The first child's margin escapes the parent with no padding or border.
        assert_rect(r(&d, &l, "outer"), 0.0, 88.0, 400.0, 20.0);
        assert_rect(r(&d, &l, "p3"), 0.0, 88.0, 400.0, 20.0);
        let (d, l) = page(
            r#"<div id="outer" style="padding:1px"><p id="p">x</p></div>"#,
            "body{margin:0} p{margin:16px 0;height:20px}",
            400.0,
        );
        assert_rect(r(&d, &l, "outer"), 0.0, 0.0, 400.0, 20.0 + 32.0 + 2.0);
        assert_rect(r(&d, &l, "p"), 1.0, 17.0, 398.0, 20.0);
    }

    #[test]
    fn negative_margins_and_empty_blocks_collapse_correctly() {
        let css = "body{margin:0} .a{height:10px;margin-bottom:30px} .b{height:10px;margin-top:-10px} .e{margin:20px 0}";
        let (d, l) = page(
            r#"<div class="a" id="a"></div><div class="e"></div><div class="b" id="b"></div>"#,
            css,
            400.0,
        );
        assert_rect(r(&d, &l, "a"), 0.0, 0.0, 400.0, 10.0);
        // 30 + (empty 20 collapses into it) + (-10): max positive 30, min negative -10: 20.
        assert_rect(r(&d, &l, "b"), 0.0, 30.0, 400.0, 10.0);
    }

    #[test]
    fn text_wraps_at_spaces_and_lines_stack_by_line_height() {
        let (d, l) = page(
            r#"<div id="t" style="width:100px">aaaa bbbb cccc</div>"#,
            "body{margin:0}",
            400.0,
        );
        assert_eq!(lines_of(&l, &d, "t"), vec!["aaaa bbbb", "cccc"]);
        assert_rect(r(&d, &l, "t"), 0.0, 0.0, 100.0, 40.0);
        let (d, l) = page(
            r#"<div id="t" style="width:20px">longword</div>"#,
            "body{margin:0}",
            400.0,
        );
        assert_eq!(
            lines_of(&l, &d, "t"),
            vec!["longword"],
            "an overlong word overflows rather than breaking"
        );
        let (d, l) = page(
            r#"<div id="t" style="width:20px;overflow-wrap:break-word">longword</div>"#,
            "body{margin:0}",
            400.0,
        );
        assert_eq!(lines_of(&l, &d, "t"), vec!["lo", "ng", "wo", "rd"]);
    }

    #[test]
    fn whitespace_is_collapsed_and_preformatted_text_is_kept() {
        let (d, l) = page(
            "<div id=\"t\" style=\"width:400px\">  a \n\n  b   <b>c</b> d  </div>",
            "body{margin:0}",
            400.0,
        );
        assert_eq!(lines_of(&l, &d, "t"), vec!["a b c d"]);
        let (d, l) = page("<pre id=\"t\">a  b\n  c</pre>", "body{margin:0}", 400.0);
        assert_eq!(lines_of(&l, &d, "t"), vec!["a  b", "  c"]);
        let (d, l) = page(
            r#"<div id="t" style="width:30px;white-space:nowrap">aaaa bbbb</div>"#,
            "body{margin:0}",
            400.0,
        );
        assert_eq!(lines_of(&l, &d, "t"), vec!["aaaa bbbb"]);
        let (d, l) = page(
            "<div id=\"t\">a<br>b<br><br>c</div>",
            "body{margin:0}",
            400.0,
        );
        assert_eq!(lines_of(&l, &d, "t"), vec!["a", "b", "", "c"]);
    }

    #[test]
    fn text_alignment_places_each_line() {
        let x_of_first = |css: &str| -> f32 {
            let (d, l) = page(
                &format!(r#"<div id="t" style="width:100px;{css}">aaaa</div>"#),
                "body{margin:0}",
                400.0,
            );
            let mut x = -1.0;
            fn first(b: &LBox, x: &mut f32) {
                for ln in &b.lines {
                    if let Some(Item::Text { x: ix, .. }) = ln.items.first() {
                        *x = *ix;
                        return;
                    }
                }
                for c in &b.children {
                    first(c, x);
                }
            }
            first(&l.root, &mut x);
            let _ = d;
            x
        };
        assert_eq!(x_of_first(""), 0.0);
        assert_eq!(x_of_first("text-align:center"), 34.0);
        assert_eq!(x_of_first("text-align:right"), 68.0);
    }

    #[test]
    fn mixed_font_sizes_make_taller_lines_and_line_height_applies() {
        let (d, l) = page(
            r#"<div id="a">a <span style="font-size:32px">b</span></div><div id="b" style="line-height:30px">x</div><div id="c" style="line-height:2">y</div>"#,
            "body{margin:0}",
            400.0,
        );
        assert_rect(r(&d, &l, "a"), 0.0, 0.0, 400.0, 40.0);
        assert_rect(r(&d, &l, "b"), 0.0, 40.0, 400.0, 30.0);
        assert_rect(r(&d, &l, "c"), 0.0, 70.0, 400.0, 32.0);
    }

    #[test]
    fn inline_blocks_shrink_to_fit_and_sit_on_the_baseline() {
        let css = "body{margin:0} #ib{display:inline-block;padding:4px;background:red} #w{width:20px;height:60px;display:inline-block}";
        let (d, l) = page(
            r#"<div id="p">hello <span id="ib">abc</span> <span id="w"></span></div>"#,
            css,
            400.0,
        );
        assert_rect(r(&d, &l, "ib"), 48.0, 32.0, 32.0, 28.0); // the 60px box raises the baseline
        let w = r(&d, &l, "w");
        assert_eq!((w.w, w.h), (20.0, 60.0));
        let p = r(&d, &l, "p");
        assert!(p.h >= 60.0, "the line grows to hold the tall box: {}", p.h);
    }

    #[test]
    fn inline_elements_have_fragments_for_their_background() {
        let (d, l) = page(
            r#"<div id="a" style="width:80px">xx <b id="b" style="background:red;padding:0 4px">bb cc dd</b> y</div>"#,
            "body{margin:0}",
            400.0,
        );
        let spans: Vec<(f32, f32, bool, bool)> = {
            let mut out = Vec::new();
            fn walk(b: &LBox, out: &mut Vec<(f32, f32, bool, bool)>) {
                for ln in &b.lines {
                    for it in &ln.items {
                        if let Item::Span {
                            x,
                            w,
                            left_edge,
                            right_edge,
                            ..
                        } = it
                        {
                            out.push((*x, *w, *left_edge, *right_edge));
                        }
                    }
                }
                for c in &b.children {
                    walk(c, out);
                }
            }
            walk(&l.root, &mut out);
            out
        };
        assert!(
            spans.len() >= 2,
            "the element wraps onto two lines: {spans:?}"
        );
        assert!(
            spans.first().unwrap().2 && !spans.first().unwrap().3,
            "starts here, continues"
        );
        assert!(
            !spans.last().unwrap().2 && spans.last().unwrap().3,
            "ends here"
        );
        let _ = d;
    }

    #[test]
    fn flex_rows_share_space_by_grow_and_basis() {
        let css = "body{margin:0} #c{display:flex;width:300px} #c>div{height:10px} #a{flex:1} #b{flex:2} #x{width:60px}";
        let (d, l) = page(
            r#"<div id="c"><div id="a"></div><div id="b"></div></div><div id="c2" style="display:flex;width:300px"><div id="x"></div><div id="y" style="flex:1;height:10px"></div></div>"#,
            css,
            400.0,
        );
        assert_rect(r(&d, &l, "a"), 0.0, 0.0, 100.0, 10.0);
        assert_rect(r(&d, &l, "b"), 100.0, 0.0, 200.0, 10.0);
        let x = r(&d, &l, "x");
        let y = r(&d, &l, "y");
        assert_eq!((x.w, y.x, y.w), (60.0, 60.0, 240.0));
    }

    #[test]
    fn flex_shrinks_justifies_aligns_gaps_and_wraps() {
        let css = "body{margin:0} .c{display:flex;width:300px;height:100px} .c>div{height:20px;width:50px}";
        // Justify and align.
        let (d, l) = page(
            r#"<div class="c" style="justify-content:center;align-items:center"><div id="a"></div><div id="b"></div></div>"#,
            css,
            400.0,
        );
        assert_rect(r(&d, &l, "a"), 100.0, 40.0, 50.0, 20.0);
        assert_rect(r(&d, &l, "b"), 150.0, 40.0, 50.0, 20.0);
        let (d, l) = page(
            r#"<div class="c" style="justify-content:space-between;align-items:flex-end"><div id="a"></div><div id="b"></div><div id="c3"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "a").x, 0.0);
        assert_eq!(r(&d, &l, "b").x, 125.0);
        assert_eq!(r(&d, &l, "c3").x, 250.0);
        assert_eq!(r(&d, &l, "a").y, 80.0);
        // Gaps.
        let (d, l) = page(
            r#"<div class="c" style="gap:10px"><div id="a"></div><div id="b"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "b").x, 60.0);
        // Shrinking: three 150px items in 300px.
        let (d, l) = page(
            r#"<div class="c" style="height:auto"><div id="a" style="width:150px"></div><div id="b" style="width:150px"></div><div id="c3" style="width:150px"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "a").w, 100.0);
        assert_eq!(r(&d, &l, "c3").x, 200.0);
        // Wrapping.
        let (d, l) = page(
            r#"<div class="c" style="flex-wrap:wrap;height:auto"><div id="a" style="width:200px"></div><div id="b" style="width:200px"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "a").y, 0.0);
        assert_eq!(r(&d, &l, "b").y, 20.0);
        assert_eq!(r(&d, &l, "b").x, 0.0);
    }

    #[test]
    fn flex_columns_reverse_order_and_auto_margins() {
        let css = "body{margin:0} .c{display:flex;width:200px} .c>div{height:20px;width:50px}";
        let (d, l) = page(
            r#"<div class="c" style="flex-direction:column"><div id="a"></div><div id="b"></div></div>"#,
            css,
            400.0,
        );
        assert_rect(r(&d, &l, "a"), 0.0, 0.0, 50.0, 20.0);
        assert_rect(r(&d, &l, "b"), 0.0, 20.0, 50.0, 20.0);
        let (d, l) = page(
            r#"<div class="c" style="flex-direction:row-reverse"><div id="a"></div><div id="b"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "a").x, 150.0);
        assert_eq!(r(&d, &l, "b").x, 100.0);
        let (d, l) = page(
            r#"<div class="c"><div id="a"></div><div id="b" style="margin-left:auto"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "b").x, 150.0);
        let (d, l) = page(
            r#"<div class="c"><div id="a" style="order:2"></div><div id="b" style="order:1"></div></div>"#,
            css,
            400.0,
        );
        assert!(r(&d, &l, "b").x < r(&d, &l, "a").x);
        let (d, l) = page(
            r#"<div class="c" style="height:100px;flex-direction:column;justify-content:flex-end"><div id="a"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "a").y, 80.0);
    }

    #[test]
    fn flex_items_stretch_to_the_line_and_text_items_wrap() {
        let css = "body{margin:0} .c{display:flex;width:200px} #a{width:50px;background:red} #b{width:50px;height:40px}";
        let (d, l) = page(
            r#"<div class="c"><div id="a">x</div><div id="b"></div></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "a").h, 40.0, "stretched to the tallest item");
        let (d, l) = page(
            r#"<div class="c" style="width:100px"><div id="a" style="flex:1">aaaa bbbb cccc</div></div>"#,
            "body{margin:0}",
            400.0,
        );
        assert_eq!(lines_of(&l, &d, "a").len(), 2);
    }

    #[test]
    fn grid_tracks_share_space_with_fixed_flexible_and_gaps() {
        let css = "body{margin:0} .g{display:grid;width:400px;grid-template-columns:100px 1fr 2fr;gap:10px} .g>div{height:20px}";
        let (d, l) = page(
            r#"<div class="g"><div id="a"></div><div id="b"></div><div id="c"></div></div>"#,
            css,
            600.0,
        );
        assert_rect(r(&d, &l, "a"), 0.0, 0.0, 100.0, 20.0);
        let fr = (400.0 - 100.0 - 20.0) / 3.0;
        assert_rect(r(&d, &l, "b"), 110.0, 0.0, fr, 20.0);
        assert_rect(r(&d, &l, "c"), 120.0 + fr, 0.0, 2.0 * fr, 20.0);
        // repeat(), rows wrap onto new lines, and row gaps add up; a row is as tall as its tallest item.
        let css = "body{margin:0} .g{display:grid;width:300px;grid-template-columns:repeat(3,1fr);row-gap:5px} #a{height:30px} #d{height:50px}";
        let html = r#"<div class="g"><div id="a"></div><div id="b"></div><div id="c"></div><div id="d"></div><div id="e"></div></div><div id="after" style="height:1px"></div>"#;
        let (d, l) = page(html, css, 600.0);
        assert_rect(r(&d, &l, "b"), 100.0, 0.0, 100.0, 30.0);
        assert_eq!(r(&d, &l, "b").h, 30.0, "items stretch to the row");
        assert_rect(r(&d, &l, "d"), 0.0, 35.0, 100.0, 50.0);
        assert_rect(r(&d, &l, "e"), 100.0, 35.0, 100.0, 50.0);
        assert_eq!(
            r(&d, &l, "after").y,
            85.0,
            "the container is as tall as its rows and gap"
        );
    }

    #[test]
    fn grid_items_can_span_and_be_placed_explicitly() {
        let css = "body{margin:0} .g{display:grid;width:300px;grid-template-columns:repeat(3,100px);grid-auto-rows:20px} #a{grid-column:1 / 3} #b{grid-column:span 2} #c{grid-column:1 / -1} #d{grid-row:4;grid-column:3} #e{grid-column:2}";
        let html = r#"<div class="g"><div id="a"></div><div id="b"></div><div id="c"></div><div id="d"></div><div id="e"></div></div>"#;
        let (d, l) = page(html, css, 600.0);
        assert_rect(r(&d, &l, "a"), 0.0, 0.0, 200.0, 20.0);
        // b does not fit beside a (one column left), so it starts the next row.
        assert_rect(r(&d, &l, "b"), 0.0, 20.0, 200.0, 20.0);
        assert_rect(r(&d, &l, "c"), 0.0, 40.0, 300.0, 20.0);
        assert_rect(r(&d, &l, "d"), 200.0, 60.0, 100.0, 20.0);
        assert_eq!(r(&d, &l, "e").x, 100.0);
    }

    #[test]
    fn auto_fill_makes_as_many_columns_as_fit_and_auto_columns_follow_their_content() {
        let css = "body{margin:0} .g{display:grid;width:350px;grid-template-columns:repeat(auto-fill,minmax(100px,1fr));gap:0} .g>div{height:10px}";
        let (d, l) = page(
            r#"<div class="g"><div id="a"></div><div id="b"></div><div id="c"></div><div id="d"></div></div>"#,
            css,
            600.0,
        );
        let w = 350.0 / 3.0;
        assert_rect(r(&d, &l, "a"), 0.0, 0.0, w, 10.0);
        assert_rect(r(&d, &l, "c"), 2.0 * w, 0.0, w, 10.0);
        assert_rect(r(&d, &l, "d"), 0.0, 10.0, w, 10.0);
        // `auto 1fr`: the sidebar is as wide as its text, the rest goes to the content.
        let css = "body{margin:0} .g{display:grid;width:300px;grid-template-columns:auto 1fr}";
        let (d, l) = page(
            r#"<div class="g"><div id="a">abcd</div><div id="b">x</div></div>"#,
            css,
            600.0,
        );
        let a = r(&d, &l, "a").w;
        assert!(a > 10.0 && a < 100.0, "the first column fits its text: {a}");
        assert_rect(r(&d, &l, "b"), a, 0.0, 300.0 - a, r(&d, &l, "b").h);
    }

    #[test]
    fn grid_alignment_margins_and_hostile_templates() {
        let css = "body{margin:0} .g{display:grid;width:200px;grid-template-columns:1fr 1fr;grid-auto-rows:100px;align-items:center} #a{height:20px;margin:5px} #b{height:40px;align-self:end}";
        let (d, l) = page(
            r#"<div class="g"><div id="a"></div><div id="b"></div></div>"#,
            css,
            600.0,
        );
        assert_rect(r(&d, &l, "a"), 5.0, 40.0, 90.0, 20.0);
        assert_rect(r(&d, &l, "b"), 100.0, 60.0, 100.0, 40.0);
        for css in [
            "body{margin:0} .g{display:grid;grid-template-columns:repeat(100000,1fr)}",
            "body{margin:0} .g{display:grid;grid-template-columns:repeat(auto-fill,0px)}",
            "body{margin:0} .g{display:grid;grid-template-columns:repeat(auto-fill,minmax(0,1fr)) repeat(auto-fill,1fr)}",
            "body{margin:0} .g{display:grid;grid-template-columns:minmax(1fr,1fr) -5px nonsense 1fr 1fr}",
            "body{margin:0} .g{display:grid;grid-template-columns:100px} .g>div{grid-column:1 / 99999;grid-row:-5 / span 99999}",
            "body{margin:0} .g{display:grid;grid-template-columns:1fr 1fr} .g>div{grid-column:0;grid-row:span 0}",
            "body{margin:0} .g{display:inline-grid;grid-template-columns:50% 50%;height:0} .g>div{grid-column:2 / 1}",
        ] {
            let (d, l) = page(r#"<div class="g"><div id="a">x</div><div id="b">y</div><div id="c">z</div></div>"#, css, 300.0);
            assert!(l.height.is_finite() && r(&d, &l, "a").w.is_finite(), "{css}");
        }
        // A grid with no children is empty and does not break its neighbours.
        let (d, l) = page(
            r#"<div class="g"></div><div id="x" style="height:5px"></div>"#,
            "body{margin:0}.g{display:grid;grid-template-columns:1fr}",
            300.0,
        );
        assert_eq!(r(&d, &l, "x").y, 0.0);
    }

    #[test]
    fn positioned_boxes_use_their_containing_block() {
        let css = "body{margin:0} #p{position:relative;width:200px;height:100px;margin:20px} #a{position:absolute;top:10px;left:20px;width:50px;height:30px} #b{position:absolute;bottom:5px;right:5px;width:10px;height:10px} #c{position:absolute;left:0;right:0;top:0;height:5px} #rel{position:relative;top:5px;left:7px;height:10px}";
        let (d, l) = page(
            r#"<div id="p"><div id="a"></div><div id="b"></div><div id="c"></div></div><div id="rel"></div><div id="after" style="height:10px"></div>"#,
            css,
            400.0,
        );
        assert_rect(r(&d, &l, "a"), 40.0, 30.0, 50.0, 30.0);
        assert_rect(
            r(&d, &l, "b"),
            20.0 + 200.0 - 5.0 - 10.0,
            20.0 + 100.0 - 5.0 - 10.0,
            10.0,
            10.0,
        );
        assert_rect(r(&d, &l, "c"), 20.0, 20.0, 200.0, 5.0);
        // A relative box moves but leaves its place in the flow.
        assert_eq!(r(&d, &l, "rel").x, 7.0);
        assert_eq!(r(&d, &l, "rel").y, 140.0 + 5.0);
        assert_eq!(
            r(&d, &l, "after").y,
            140.0 + 10.0,
            "the next box ignores the shift"
        );
        // Fixed boxes use the viewport.
        let (d, l) = page(
            r#"<div style="height:1000px"><div id="f" style="position:fixed;right:0;bottom:0;width:100px;height:50px"></div></div>"#,
            "body{margin:0}",
            400.0,
        );
        assert_rect(r(&d, &l, "f"), 300.0, 550.0, 100.0, 50.0);
    }

    #[test]
    fn tables_get_column_widths_from_their_content() {
        let css = "body{margin:0} table{border-spacing:0} td{padding:0}";
        let (d, l) = page(
            r#"<table id="t"><tr><td id="a">aaaa</td><td id="b">bb</td></tr><tr><td id="c">c</td><td id="e">eeeeee</td></tr></table>"#,
            css,
            400.0,
        );
        let a = r(&d, &l, "a");
        let b = r(&d, &l, "b");
        assert_eq!(
            (a.x, a.w),
            (0.0, 32.0),
            "the first column fits its widest cell"
        );
        assert_eq!((b.x, b.w), (32.0, 48.0));
        assert_eq!(r(&d, &l, "c").y, 20.0);
        assert_eq!(
            r(&d, &l, "t").w,
            80.0,
            "an auto-width table shrinks to its content"
        );
        // Spacing and colspan.
        let (d, l) = page(
            r#"<table id="t" style="border-spacing:4px"><tr><td id="a" colspan="2">spanning cell</td></tr><tr><td id="b">x</td><td id="c">y</td></tr></table>"#,
            "body{margin:0}td{padding:0}",
            400.0,
        );
        assert_eq!(r(&d, &l, "a").x, 4.0);
        assert!(r(&d, &l, "a").w >= r(&d, &l, "b").w + r(&d, &l, "c").w);
        // A table with a width fills it.
        let (d, l) = page(
            r#"<table id="t" style="width:300px"><tr><td id="a">a</td><td id="b">b</td></tr></table>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "t").w, 300.0);
        assert!((r(&d, &l, "a").w + r(&d, &l, "b").w - 300.0).abs() < 0.01);
    }

    #[test]
    fn lists_get_markers_in_order() {
        let (d, l) = page(
            r#"<ol><li id="a">x</li><li id="b">y</li><li id="c" style="list-style-type:lower-roman">z</li></ol><ul><li id="d">w</li></ul>"#,
            "body{margin:0}",
            400.0,
        );
        let marker = |id: &str| {
            fn find(b: &LBox, node: NodeId) -> Option<&LBox> {
                if b.node == Some(node) {
                    return Some(b);
                }
                b.children.iter().find_map(|c| find(c, node))
            }
            find(&l.root, d.by_id(id).unwrap()).and_then(|b| b.marker.clone())
        };
        assert_eq!(marker("a").unwrap().kind, MarkerKind::Text("1.".into()));
        assert_eq!(marker("b").unwrap().kind, MarkerKind::Text("2.".into()));
        assert_eq!(marker("c").unwrap().kind, MarkerKind::Text("iii.".into()));
        assert_eq!(marker("d").unwrap().kind, MarkerKind::Disc);
        assert!(marker("a").unwrap().x < 0.0, "markers sit outside the item");
        assert_eq!(alpha(1, false), "a");
        assert_eq!(alpha(27, true), "AA");
        assert_eq!(roman(1994), "MCMXCIV");
    }

    #[test]
    fn display_none_takes_no_space_and_replaced_elements_get_sizes() {
        let (d, l) = page(
            r#"<div id="a" style="height:10px"></div><div style="display:none;height:99px"></div><div id="b" style="height:10px;visibility:hidden"></div><img id="i" width="120" height="40" alt="logo"><input id="n" size="5">"#,
            "body{margin:0}",
            400.0,
        );
        assert_eq!(
            r(&d, &l, "b").y,
            10.0,
            "display:none leaves no gap, visibility:hidden keeps its space"
        );
        let i = r(&d, &l, "i");
        assert_eq!((i.w, i.h), (120.0, 40.0));
        let n = r(&d, &l, "n");
        assert!(
            (n.w - (5.0 * 6.65 + 12.0 + 2.0)).abs() < 3.0 || n.w > 30.0,
            "{n:?}"
        );
    }

    #[test]
    fn heights_min_max_and_overflow_clip() {
        let css = "body{margin:0} #a{height:10px;min-height:30px} #b{height:100px;max-height:40px} #c{height:20px;overflow:hidden} #d{min-height:50%}";
        let (d, l) = page(
            r#"<div id="a"></div><div id="b"></div><div id="c"><div style="height:500px"></div></div><div id="d"></div>"#,
            css,
            400.0,
        );
        assert_eq!(r(&d, &l, "a").h, 30.0);
        assert_eq!(r(&d, &l, "b").h, 40.0);
        assert_eq!(r(&d, &l, "c").h, 20.0);
        fn find(b: &LBox, node: NodeId) -> Option<&LBox> {
            if b.node == Some(node) {
                return Some(b);
            }
            b.children.iter().find_map(|c| find(c, node))
        }
        assert!(find(&l.root, d.by_id("c").unwrap()).unwrap().clips());
    }

    #[test]
    fn hit_testing_finds_the_innermost_element() {
        let (d, l) = page(
            r#"<div id="a" style="padding:10px"><p id="p" style="margin:0">hello <a id="k" href="x">link</a></p></div>"#,
            "body{margin:0}",
            400.0,
        );
        let p = r(&d, &l, "p");
        assert_eq!(hit_test(&l, p.x + 2.0, p.y + 2.0), d.by_id("p"));
        // "hello " is 6 chars of 8px: the link starts at x = 10 + 48.
        assert_eq!(hit_test(&l, 10.0 + 48.0 + 4.0, p.y + 8.0), d.by_id("k"));
        assert_eq!(hit_test(&l, 5.0, 5.0), d.by_id("a"));
        assert_eq!(
            hit_test(&l, 399.0, 590.0),
            d.by_id("html").or(Some(d.children(0)[0]))
        );
    }

    #[test]
    fn the_canvas_colour_comes_from_html_or_body() {
        let (_, l) = page("<p>x</p>", "body{background:#123456}", 400.0);
        assert_eq!(l.canvas_color, 0xFF12_3456);
        let (_, l) = page(
            "<p>x</p>",
            "html{background:#abcdef} body{background:#123456}",
            400.0,
        );
        assert_eq!(l.canvas_color, 0xFFAB_CDEF);
        let (_, l) = page("<p>x</p>", "", 400.0);
        assert_eq!(l.canvas_color, 0xFFFF_FFFF);
    }

    #[test]
    fn a_realistic_page_lays_out_without_overlap_surprises() {
        let html = r#"<html><head><style>
            body{font-family:sans-serif;margin:0}
            header{background:#333;color:white;padding:10px 20px}
            nav a{margin-right:10px}
            .cols{display:flex;gap:20px;padding:20px}
            .main{flex:3}.side{flex:1;background:#eee;padding:10px}
            footer{text-align:center;padding:10px}
        </style></head><body>
            <header id="h"><h1>Title</h1><nav><a href="x">One</a><a href="x">Two</a></nav></header>
            <div class="cols"><div class="main" id="m"><p>Some body text that is long enough to wrap onto several lines in a narrow column.</p></div><div class="side" id="s">Side</div></div>
            <footer id="f">Footer</footer></body></html>"#;
        let (d, l) = page(html, "", 600.0);
        let (h, m, s, f) = (
            r(&d, &l, "h"),
            r(&d, &l, "m"),
            r(&d, &l, "s"),
            r(&d, &l, "f"),
        );
        assert_eq!(h.w, 600.0);
        assert!(
            m.x < s.x && (m.x + m.w + 20.0 - s.x).abs() < 0.5,
            "main and side are side by side with the gap: {m:?} {s:?}"
        );
        assert!(
            (m.w - 3.0 * (s.w - 20.0)).abs() < 0.5,
            "3:1 sharing of the content widths (the side box has 20px of padding): {m:?} {s:?}"
        );
        assert!(
            f.y >= s.y + s.h.min(m.h) - 0.01 && f.y >= h.y + h.h,
            "the footer is below everything: {f:?}"
        );
        assert!(l.height >= f.y + f.h);
    }

    #[test]
    fn large_and_hostile_documents_are_handled() {
        let html = "<div><p>some text here and there <b>bold</b> <a href=x>link</a></p><ul><li>one<li>two</ul></div>".repeat(1500);
        let t = std::time::Instant::now();
        let (_, l) = page(&html, "", 800.0);
        assert!(l.height > 1000.0);
        assert!(t.elapsed().as_secs() < 15, "took {:?}", t.elapsed());
        // Deep nesting, absurd sizes and odd values must not panic.
        let deep = format!(
            "{}x{}",
            "<div style=\"padding:1px\">".repeat(150),
            "</div>".repeat(150)
        );
        let (_, l) = page(&deep, "", 800.0);
        assert!(l.height > 300.0);
        for css in [
            "div{width:1e9px;height:99999999px;margin:-99999999px}",
            "div{width:calc(100% - 100000px)}",
            "div{display:flex;width:0;flex:1 1 0}",
            "div{position:absolute;left:99999999px;top:-99999999px}",
            "div{font-size:0;line-height:0;letter-spacing:-5px}",
            "table{width:0}td{width:99999px}",
            "div{padding:50%;border:99999px solid red}",
        ] {
            let (_, l) = page(
                "<div>a<div>b</div><table><tr><td>x<td>y</table></div>",
                css,
                300.0,
            );
            assert!(l.height.is_finite() && l.width.is_finite(), "{css}");
        }
        let (_, l) = page("", "", 300.0);
        assert!(l.height >= 0.0);
        let many = "<span>w </span>".repeat(100_000);
        let (_, l) = page(&many, "", 300.0);
        assert!(l.boxes <= MAX_BOXES + 10);
    }
}
