//! Computed styles: the cascade from the default stylesheet, author stylesheets and `style`
//! attributes to one fully resolved [`Style`] per element.
//!
//! What it does: origin and specificity ordering with `!important`, inheritance, `inherit` /
//! `initial` / `unset`, `var()` with fallbacks and inherited custom properties, shorthands
//! (`margin`, `padding`, `border`, `background`, `font`, `flex`, `flex-flow`, `list-style`,
//! `border-radius`, `gap`, `overflow`, `inset`), and converts `em`, `rem`, `vw` and `vh` lengths
//! to pixels (percentages stay, because they need the containing block, which layout knows).
//! A declaration with a value it cannot read is ignored and the earlier value stays, as in a
//! browser.

use crate::css::{
    self, parse_color, parse_len, parse_len_value, Decl, Len, LenValue, State, Stylesheet,
};
use crate::dom::{Dom, NodeId};
use std::collections::BTreeMap;
use std::rc::Rc;

/// The default root font size in pixels.
pub const ROOT_FONT_SIZE: f32 = 16.0;

/// A resolved length: pixels plus a fraction of the containing size.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Dim {
    /// Pixels.
    pub px: f32,
    /// Fraction of the containing block's size.
    pub pct: f32,
}

impl Dim {
    /// This length for a containing size of `base` pixels.
    #[must_use]
    pub fn resolve(self, base: f32) -> f32 {
        self.px + self.pct * base
    }
}

/// A width, height or offset: `auto` or a length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    /// `auto` (or `none` for a maximum).
    Auto,
    /// A length.
    Dim(Dim),
}

impl Size {
    /// The length, if it is not auto.
    #[must_use]
    pub fn dim(self) -> Option<Dim> {
        match self {
            Size::Dim(d) => Some(d),
            Size::Auto => None,
        }
    }
}

macro_rules! keyword_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($(#[$vm])* $v),+ }
    };
}

keyword_enum!(
    /// How a box takes part in layout.
    Display {
        /** Not rendered. */
        None,
        /** A block. */
        Block,
        /** Inline text-level. */
        Inline,
        /** An inline box laid out like a block. */
        InlineBlock,
        /** A flex container. */
        Flex,
        /** An inline flex container. */
        InlineFlex,
        /** A list item (block with a marker). */
        ListItem,
        /** A table. */
        Table,
        /** A table row. */
        TableRow,
        /** A table cell. */
        TableCell,
        /** A row group (`tbody`, `thead`). */
        TableRowGroup
    }
);
keyword_enum!(
    /** Positioning scheme. */
    Position {
        /** Normal flow. */
        Static,
        /** Offset from normal flow. */
        Relative,
        /** Out of flow, relative to a positioned ancestor. */
        Absolute,
        /** Out of flow, relative to the viewport. */
        Fixed
    }
);
keyword_enum!(
    /** Floating. */
    Float {
        /** Not floated. */
        None,
        /** Floated left. */
        Left,
        /** Floated right. */
        Right
    }
);
keyword_enum!(
    /** Clearing floats. */
    Clear {
        /** No clearing. */
        None,
        /** Below left floats. */
        Left,
        /** Below right floats. */
        Right,
        /** Below all floats. */
        Both
    }
);
keyword_enum!(
    /** Overflow handling. */
    Overflow {
        /** Not clipped. */
        Visible,
        /** Clipped. */
        Hidden,
        /** Clipped (scrolling is not supported). */
        Scroll,
        /** Clipped (scrolling is not supported). */
        Auto
    }
);
keyword_enum!(
    /** Horizontal alignment of text. */
    TextAlign {
        /** Left. */
        Left,
        /** Right. */
        Right,
        /** Centred. */
        Center,
        /** Justified (drawn as left). */
        Justify
    }
);
keyword_enum!(
    /** Whitespace handling. */
    WhiteSpace {
        /** Collapse and wrap. */
        Normal,
        /** Collapse, no wrap. */
        NoWrap,
        /** Keep, no wrap. */
        Pre,
        /** Keep and wrap. */
        PreWrap,
        /** Collapse spaces, keep line breaks. */
        PreLine
    }
);
keyword_enum!(
    /** Letter case. */
    TextTransform {
        /** As written. */
        None,
        /** UPPER. */
        Upper,
        /** lower. */
        Lower,
        /** Capitalise Words. */
        Capitalize
    }
);
keyword_enum!(
    /** Vertical alignment inside a line. */
    VAlign {
        /** On the baseline. */
        Baseline,
        /** Top of the line. */
        Top,
        /** Middle of the line. */
        Middle,
        /** Bottom of the line. */
        Bottom,
        /** Lowered. */
        Sub,
        /** Raised. */
        Super
    }
);
keyword_enum!(
    /** List marker. */
    ListStyle {
        /** Filled circle. */
        Disc,
        /** Open circle. */
        Circle,
        /** Square. */
        Square,
        /** 1, 2, 3. */
        Decimal,
        /** a, b, c. */
        LowerAlpha,
        /** A, B, C. */
        UpperAlpha,
        /** i, ii. */
        LowerRoman,
        /** I, II. */
        UpperRoman,
        /** None. */
        None
    }
);
keyword_enum!(
    /** Flex direction. */
    FlexDir {
        /** Left to right. */
        Row,
        /** Right to left. */
        RowReverse,
        /** Top to bottom. */
        Column,
        /** Bottom to top. */
        ColumnReverse
    }
);
keyword_enum!(
    /** Flex wrapping. */
    FlexWrap {
        /** One line. */
        NoWrap,
        /** Wrap. */
        Wrap,
        /** Wrap upwards. */
        WrapReverse
    }
);
keyword_enum!(
    /** Distribution along the main axis. */
    Justify {
        /** Start. */
        Start,
        /** End. */
        End,
        /** Centre. */
        Center,
        /** Space between. */
        SpaceBetween,
        /** Space around. */
        SpaceAround,
        /** Space evenly. */
        SpaceEvenly
    }
);
keyword_enum!(
    /** Alignment on the cross axis. */
    Align {
        /** Fill. */
        Stretch,
        /** Start. */
        Start,
        /** End. */
        End,
        /** Centre. */
        Center,
        /** First baseline (treated as start). */
        Baseline
    }
);
keyword_enum!(
    /** Border line style. */
    BorderStyle {
        /** No border. */
        None,
        /** A solid line. */
        Solid,
        /** Dashes. */
        Dashed,
        /** Dots. */
        Dotted,
        /** Two lines. */
        Double
    }
);

/// Line height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// The font's own.
    Normal,
    /// A multiple of the font size.
    Number(f32),
    /// Pixels.
    Px(f32),
}

/// A gradient background (two or more colour stops, evenly spaced unless positions are given).
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// Angle in degrees (180 is top to bottom).
    pub angle: f32,
    /// Colour stops: position 0 to 1 and `0xAARRGGBB`.
    pub stops: Vec<(f32, u32)>,
}

/// The fully resolved style of one element.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// Display type.
    pub display: Display,
    /// Positioning.
    pub position: Position,
    /// Float.
    pub float: Float,
    /// Clear.
    pub clear: Clear,
    /// `visibility: hidden`.
    pub hidden: bool,
    /// Opacity, 0 to 1.
    pub opacity: f32,
    /// Width.
    pub width: Size,
    /// Height.
    pub height: Size,
    /// Minimum width.
    pub min_width: Dim,
    /// Maximum width (`Auto` is none).
    pub max_width: Size,
    /// Minimum height.
    pub min_height: Dim,
    /// Maximum height.
    pub max_height: Size,
    /// Margins: top, right, bottom, left (`Auto` is auto).
    pub margin: [Size; 4],
    /// Paddings.
    pub padding: [Dim; 4],
    /// Border widths in pixels.
    pub border_width: [f32; 4],
    /// Border styles.
    pub border_style: [BorderStyle; 4],
    /// Border colours (`0xAARRGGBB`).
    pub border_color: [u32; 4],
    /// Corner radii in pixels: top-left, top-right, bottom-right, bottom-left.
    pub radius: [Dim; 4],
    /// `box-sizing: border-box`.
    pub border_box: bool,
    /// Horizontal overflow.
    pub overflow_x: Overflow,
    /// Vertical overflow.
    pub overflow_y: Overflow,
    /// Offsets for positioned boxes: top, right, bottom, left.
    pub inset: [Size; 4],
    /// Stacking order.
    pub z_index: i32,
    /// Text colour.
    pub color: u32,
    /// Background colour.
    pub background: u32,
    /// Background gradient.
    pub gradient: Option<Gradient>,
    /// Font size in pixels.
    pub font_size: f32,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Monospace face.
    pub mono: bool,
    /// Line height.
    pub line_height: LineHeight,
    /// Extra space between letters, in pixels.
    pub letter_spacing: f32,
    /// Text alignment.
    pub text_align: TextAlign,
    /// Underline.
    pub underline: bool,
    /// Strike-through.
    pub strike: bool,
    /// Letter case.
    pub text_transform: TextTransform,
    /// Whitespace handling.
    pub white_space: WhiteSpace,
    /// Break long words that do not fit.
    pub break_words: bool,
    /// List marker.
    pub list_style: ListStyle,
    /// Vertical alignment.
    pub vertical_align: VAlign,
    /// Flex direction.
    pub flex_direction: FlexDir,
    /// Flex wrapping.
    pub flex_wrap: FlexWrap,
    /// Main-axis distribution.
    pub justify: Justify,
    /// Cross-axis alignment of items.
    pub align_items: Align,
    /// Cross-axis alignment of this item (`None` follows the container).
    pub align_self: Option<Align>,
    /// Flex grow factor.
    pub flex_grow: f32,
    /// Flex shrink factor.
    pub flex_shrink: f32,
    /// Flex basis.
    pub flex_basis: Size,
    /// Flex order.
    pub order: i32,
    /// Row gap.
    pub row_gap: Dim,
    /// Column gap.
    pub column_gap: Dim,
    /// Space between table cells, in pixels (inherited).
    pub border_spacing: f32,
    /// `border-collapse: collapse` (inherited).
    pub border_collapse: bool,
    /// Custom properties (`--name`), inherited.
    pub vars: Rc<BTreeMap<String, String>>,
}

impl Style {
    /// The style of the root: initial values.
    #[must_use]
    pub fn initial() -> Self {
        let zero = Dim::default();
        Style {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            hidden: false,
            opacity: 1.0,
            width: Size::Auto,
            height: Size::Auto,
            min_width: zero,
            max_width: Size::Auto,
            min_height: zero,
            max_height: Size::Auto,
            margin: [Size::Dim(zero); 4],
            padding: [zero; 4],
            border_width: [0.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [0xFF00_0000; 4],
            radius: [zero; 4],
            border_box: false,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            inset: [Size::Auto; 4],
            z_index: 0,
            color: 0xFF00_0000,
            background: 0,
            gradient: None,
            font_size: ROOT_FONT_SIZE,
            bold: false,
            italic: false,
            mono: false,
            line_height: LineHeight::Normal,
            letter_spacing: 0.0,
            text_align: TextAlign::Left,
            underline: false,
            strike: false,
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            break_words: false,
            list_style: ListStyle::Disc,
            vertical_align: VAlign::Baseline,
            flex_direction: FlexDir::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify: Justify::Start,
            align_items: Align::Stretch,
            align_self: None,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Size::Auto,
            order: 0,
            row_gap: zero,
            column_gap: zero,
            border_spacing: 0.0,
            border_collapse: false,
            vars: Rc::new(BTreeMap::new()),
        }
    }

    /// The style a child starts from: inherited properties copied, the rest initial.
    pub fn inherit_from(parent: &Style) -> Self {
        let mut s = Style::initial();
        s.hidden = parent.hidden;
        s.color = parent.color;
        s.font_size = parent.font_size;
        s.bold = parent.bold;
        s.italic = parent.italic;
        s.mono = parent.mono;
        s.line_height = parent.line_height;
        s.letter_spacing = parent.letter_spacing;
        s.text_align = parent.text_align;
        s.text_transform = parent.text_transform;
        s.white_space = parent.white_space;
        s.break_words = parent.break_words;
        s.list_style = parent.list_style;
        s.border_spacing = parent.border_spacing;
        s.border_collapse = parent.border_collapse;
        s.vars = parent.vars.clone();
        s
    }

    /// Whether the element generates any box.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.display != Display::None
    }

    /// Whether the element is a block-level box in its parent.
    #[must_use]
    pub fn block_level(&self) -> bool {
        matches!(
            self.display,
            Display::Block
                | Display::Flex
                | Display::ListItem
                | Display::Table
                | Display::TableRow
                | Display::TableRowGroup
        )
    }
}

/// The viewport the page is laid out in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Width in pixels.
    pub w: f32,
    /// Height in pixels.
    pub h: f32,
}

// ------------------------------------------------------------------ the default stylesheet

/// The built-in stylesheet every page starts from.
pub const USER_AGENT_CSS: &str = "
html, address, blockquote, body, dd, div, dl, dt, fieldset, form, frame, frameset, h1, h2, h3, h4, h5, h6,
noframes, ol, p, ul, center, dir, hr, menu, pre, article, aside, footer, header, hgroup, main, nav, section,
figure, figcaption, details, summary { display: block }
head, link, meta, script, style, title, base, template, area, datalist, noscript, param, source, track,
[hidden] { display: none }
li { display: list-item }
table { display: table; border-spacing: 2px }
tr { display: table-row }
thead, tbody, tfoot { display: table-row-group }
td, th { display: table-cell; padding: 1px }
caption { display: block; text-align: center }
body { margin: 8px }
h1 { font-size: 2em; margin: 0.67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: 0.83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { font-size: 1em; margin: 1.33em 0; font-weight: bold }
h5 { font-size: 0.83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: 0.67em; margin: 2.33em 0; font-weight: bold }
p, dl, menu { margin: 1em 0 }
blockquote { margin: 1em 40px }
figure { margin: 1em 40px }
dd { margin-left: 40px }
ul, ol, dir, menu { margin: 1em 0; padding-left: 40px }
ul ul, ol ul { list-style-type: circle }
ul ul ul, ol ul ul { list-style-type: square }
ol { list-style-type: decimal }
ul ul, ul ol, ol ul, ol ol { margin: 0 }
nav ul, nav ol { list-style-type: none }
b, strong, th { font-weight: bold }
i, em, cite, var, address, dfn { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
a:link { color: #0000EE; text-decoration: underline }
mark { background-color: yellow; color: black }
small { font-size: 0.83em }
big { font-size: 1.17em }
sub { font-size: 0.83em; vertical-align: sub }
sup { font-size: 0.83em; vertical-align: super }
code, kbd, samp, tt { font-family: monospace; font-size: 0.8125em }
pre { font-family: monospace; font-size: 0.8125em; white-space: pre; margin: 1em 0 }
pre code { font-size: 1em }
hr { margin: 0.5em auto; border: 1px solid #9a9a9a; border-width: 1px 0 0 0; height: 0 }
th { text-align: center }
center { text-align: center }
img { display: inline }
button, input, select, textarea { display: inline-block; font-size: 13.33px; padding: 1px 6px;
  border: 1px solid #767676; background-color: #efefef; color: black; border-radius: 3px }
input[type=checkbox], input[type=radio] { padding: 0; width: 13px; height: 13px }
textarea { white-space: pre-wrap }
input[type=hidden] { display: none }
fieldset { margin: 0 2px; padding: 0.35em 0.75em 0.625em; border: 2px groove #c0c0c0 }
legend { padding: 0 2px }
summary { display: block }
video, canvas, iframe, object, embed { display: inline-block }
";

// ------------------------------------------------------------------ the cascade

/// The computed style of every node of a document.
#[derive(Debug, Clone)]
pub struct Styles {
    by_node: Vec<Option<Rc<Style>>>,
}

impl Styles {
    /// The style of an element (`None` for text and the document).
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&Style> {
        self.by_node.get(id).and_then(Option::as_deref)
    }

    /// The shared style of an element.
    #[must_use]
    pub fn rc(&self, id: NodeId) -> Option<Rc<Style>> {
        self.by_node.get(id).and_then(Clone::clone)
    }
}

struct Entry {
    origin: u8,
    sheet: usize,
    rule: usize,
    sel: usize,
    order: usize,
}

#[derive(Default)]
struct Index {
    by_id: BTreeMap<String, Vec<usize>>,
    by_class: BTreeMap<String, Vec<usize>>,
    by_tag: BTreeMap<String, Vec<usize>>,
    universal: Vec<usize>,
    entries: Vec<Entry>,
}

fn build_index(sheets: &[(&Stylesheet, u8)], vp: Viewport) -> Index {
    let mut idx = Index::default();
    let mut order = 0;
    for (si, (sheet, origin)) in sheets.iter().enumerate() {
        for (ri, rule) in sheet.rules.iter().enumerate() {
            if rule.media.is_some_and(|m| !m.matches(vp.w, vp.h)) {
                continue;
            }
            for (ci, sel) in rule.selectors.iter().enumerate() {
                let Some((_, last)) = sel.parts.last() else {
                    continue;
                };
                let e = idx.entries.len();
                idx.entries.push(Entry {
                    origin: *origin,
                    sheet: si,
                    rule: ri,
                    sel: ci,
                    order,
                });
                order += 1;
                if let Some(id) = &last.id {
                    idx.by_id.entry(id.clone()).or_default().push(e);
                } else if let Some(c) = last.classes.first() {
                    idx.by_class.entry(c.clone()).or_default().push(e);
                } else if let Some(t) = &last.tag {
                    idx.by_tag.entry(t.clone()).or_default().push(e);
                } else {
                    idx.universal.push(e);
                }
            }
        }
    }
    idx
}

/// Computes the style of every element of `dom`.
///
/// `author` are the page's stylesheets in order; the built-in stylesheet comes first.
#[must_use]
pub fn compute(dom: &Dom, author: &[Stylesheet], vp: Viewport, state: &State) -> Styles {
    let ua = Stylesheet::parse(USER_AGENT_CSS);
    let mut all: Vec<(&Stylesheet, u8)> = vec![(&ua, 0)];
    all.extend(author.iter().map(|s| (s, 1)));
    let index = build_index(&all, vp);
    let mut by_node: Vec<Option<Rc<Style>>> = vec![None; dom.len()];
    let root_style = Style::initial();
    let mut stack: Vec<NodeId> = vec![dom.root()];
    let mut parent_of: Vec<usize> = vec![usize::MAX; dom.len()];
    while let Some(n) = stack.pop() {
        if dom.tag(n).is_some() {
            let parent_style = parent_of
                .get(n)
                .copied()
                .filter(|&p| p != usize::MAX)
                .and_then(|p| by_node[p].clone())
                .unwrap_or_else(|| Rc::new(root_style.clone()));
            let s = style_for(dom, n, &all, &index, vp, state, &parent_style);
            by_node[n] = Some(Rc::new(s));
        }
        for &c in dom.children(n).iter().rev() {
            parent_of[c] = if dom.tag(n).is_some() {
                n
            } else {
                parent_of[n]
            };
            stack.push(c);
        }
    }
    Styles { by_node }
}

struct Matched<'a> {
    layer: u8,
    spec: u32,
    order: usize,
    decl: &'a Decl,
}

fn style_for(
    dom: &Dom,
    n: NodeId,
    sheets: &[(&Stylesheet, u8)],
    idx: &Index,
    vp: Viewport,
    state: &State,
    parent: &Style,
) -> Style {
    let mut cand: Vec<usize> = Vec::new();
    if let Some(id) = dom.element_id(n) {
        if let Some(v) = idx.by_id.get(id) {
            cand.extend(v);
        }
    }
    for c in dom.classes(n) {
        if let Some(v) = idx.by_class.get(c) {
            cand.extend(v);
        }
    }
    if let Some(t) = dom.tag(n) {
        if let Some(v) = idx.by_tag.get(t) {
            cand.extend(v);
        }
    }
    cand.extend(&idx.universal);
    cand.sort_unstable();
    cand.dedup();
    let mut matched: Vec<Matched<'_>> = Vec::new();
    for e in cand {
        let en = &idx.entries[e];
        let rule = &sheets[en.sheet].0.rules[en.rule];
        let sel = &rule.selectors[en.sel];
        if css::matches(dom, n, sel, state) {
            for d in &rule.decls {
                let layer = match (en.origin, d.important) {
                    (0, _) => 0,
                    (_, false) => 1,
                    (_, true) => 3,
                };
                matched.push(Matched {
                    layer,
                    spec: sel.specificity,
                    order: en.order,
                    decl: d,
                });
            }
        }
    }
    let inline: Vec<Decl> = dom
        .attr(n, "style")
        .map(css::parse_declarations)
        .unwrap_or_default();
    for d in &inline {
        matched.push(Matched {
            layer: if d.important { 4 } else { 2 },
            spec: u32::MAX,
            order: usize::MAX,
            decl: d,
        });
    }
    matched.sort_by_key(|m| (m.layer, m.spec, m.order));
    // Custom properties first.
    let mut vars: BTreeMap<String, String> = (*parent.vars).clone();
    let mut own_vars = false;
    for m in &matched {
        if m.decl.name.starts_with("--") {
            vars.insert(m.decl.name.clone(), m.decl.value.clone());
            own_vars = true;
        }
    }
    let mut s = Style::inherit_from(parent);
    if own_vars {
        s.vars = Rc::new(vars);
    }
    let ctx = Ctx { parent, vp };
    let mut border_color_set = [false; 4];
    // Expand every declaration, then apply font sizes first so that `em` lengths in the same
    // element use its final font size, as the cascade defines.
    let mut expanded: Vec<(String, String)> = Vec::new();
    for m in &matched {
        if m.decl.name.starts_with("--") {
            continue;
        }
        let Some(value) = substitute_vars(&m.decl.value, &s.vars, 0) else {
            continue;
        };
        expanded.extend(expand_shorthand(&m.decl.name, &value));
    }
    for (name, v) in expanded.iter().filter(|(n, _)| n == "font-size") {
        apply(&mut s, name, v, &ctx);
    }
    for (name, v) in expanded.iter().filter(|(n, _)| n != "font-size") {
        if let Some(side) = [
            "border-top-color",
            "border-right-color",
            "border-bottom-color",
            "border-left-color",
        ]
        .iter()
        .position(|x| x == name)
        {
            border_color_set[side] = true;
        }
        apply(&mut s, name, v, &ctx);
    }
    // `currentcolor` and unset border colours follow the text colour.
    for (i, set) in border_color_set.iter().enumerate() {
        if !set || s.border_color[i] == CURRENT {
            s.border_color[i] = s.color;
        }
    }
    s
}

/// A marker colour meaning `currentcolor` until the cascade finishes.
const CURRENT: u32 = 0x0123_4567;

fn substitute_vars(value: &str, vars: &BTreeMap<String, String>, depth: usize) -> Option<String> {
    if !value.contains("var(") {
        return Some(value.to_string());
    }
    if depth > 8 {
        return None;
    }
    let mut out = String::new();
    let mut rest = value;
    while let Some(i) = rest.find("var(") {
        out.push_str(&rest[..i]);
        let args_start = i + 4;
        let mut depth_p = 1;
        let mut end = None;
        for (j, c) in rest[args_start..].char_indices() {
            match c {
                '(' => depth_p += 1,
                ')' => {
                    depth_p -= 1;
                    if depth_p == 0 {
                        end = Some(args_start + j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let inner = &rest[args_start..end];
        let (name, fallback) = match inner.find(',') {
            Some(c) => (inner[..c].trim(), Some(inner[c + 1..].trim())),
            None => (inner.trim(), None),
        };
        let resolved = match vars.get(name) {
            Some(v) => substitute_vars(v, vars, depth + 1)?,
            None => substitute_vars(fallback?, vars, depth + 1)?,
        };
        out.push_str(&resolved);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    (out.len() < 20_000).then_some(out)
}

/// Splits on spaces and commas-free top-level whitespace, keeping `(...)` groups whole.
fn split_values(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            _ => {}
        }
        if c.is_whitespace() && depth == 0 {
            if let Some(st) = start.take() {
                out.push(&s[st..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

fn four<'a>(v: &[&'a str]) -> Option<[&'a str; 4]> {
    match v.len() {
        1 => Some([v[0]; 4]),
        2 => Some([v[0], v[1], v[0], v[1]]),
        3 => Some([v[0], v[1], v[2], v[1]]),
        4 => Some([v[0], v[1], v[2], v[3]]),
        _ => None,
    }
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

fn is_border_style(s: &str) -> bool {
    matches!(
        s,
        "none"
            | "hidden"
            | "solid"
            | "dashed"
            | "dotted"
            | "double"
            | "groove"
            | "ridge"
            | "inset"
            | "outset"
    )
}

/// Turns a shorthand into longhands (other properties pass through).
fn expand_shorthand(name: &str, value: &str) -> Vec<(String, String)> {
    let v = split_values(value);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |n: String, val: &str| out.push((n, val.to_string()));
    match name {
        "margin" | "padding" => {
            if let Some(f) = four(&v) {
                for (i, side) in SIDES.iter().enumerate() {
                    push(format!("{name}-{side}"), f[i]);
                }
            }
        }
        "inset" => {
            if let Some(f) = four(&v) {
                for (i, side) in SIDES.iter().enumerate() {
                    push((*side).to_string(), f[i]);
                }
            }
        }
        "border-width" | "border-style" | "border-color" => {
            if let Some(f) = four(&v) {
                let kind = &name[7..];
                for (i, side) in SIDES.iter().enumerate() {
                    push(format!("border-{side}-{kind}"), f[i]);
                }
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let sides: Vec<&str> = match name.strip_prefix("border-") {
                Some(s) => vec![SIDES.iter().find(|x| **x == s).copied().unwrap_or("top")],
                None => SIDES.to_vec(),
            };
            let (mut w, mut st, mut c) = ("medium", "none", "currentcolor");
            for t in &v {
                if is_border_style(t) {
                    st = t;
                } else if parse_len(t).is_some() || matches!(*t, "thin" | "medium" | "thick") {
                    w = t;
                } else {
                    c = t;
                }
            }
            if v.len() == 1 && v[0] == "none" {
                st = "none";
            }
            for side in sides {
                push(format!("border-{side}-width"), w);
                push(format!("border-{side}-style"), st);
                push(format!("border-{side}-color"), c);
            }
        }
        "border-radius" => {
            let first: Vec<&str> = value
                .split('/')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect();
            if let Some(f) = four(&first) {
                for (i, corner) in ["top-left", "top-right", "bottom-right", "bottom-left"]
                    .iter()
                    .enumerate()
                {
                    push(format!("border-{corner}-radius"), f[i]);
                }
            }
        }
        "overflow" => {
            let x = v.first().copied().unwrap_or("visible");
            push("overflow-x".into(), x);
            push("overflow-y".into(), v.get(1).copied().unwrap_or(x));
        }
        "gap" | "grid-gap" => {
            if let Some(r) = v.first() {
                push("row-gap".into(), r);
                push("column-gap".into(), v.get(1).unwrap_or(r));
            }
        }
        "flex-flow" => {
            for t in &v {
                match *t {
                    "row" | "row-reverse" | "column" | "column-reverse" => {
                        push("flex-direction".into(), t)
                    }
                    "nowrap" | "wrap" | "wrap-reverse" => push("flex-wrap".into(), t),
                    _ => {}
                }
            }
        }
        "flex" => match v.as_slice() {
            ["none"] => {
                push("flex-grow".into(), "0");
                push("flex-shrink".into(), "0");
                push("flex-basis".into(), "auto");
            }
            ["auto"] => {
                push("flex-grow".into(), "1");
                push("flex-shrink".into(), "1");
                push("flex-basis".into(), "auto");
            }
            [g] if g.parse::<f32>().is_ok() => {
                push("flex-grow".into(), g);
                push("flex-shrink".into(), "1");
                push("flex-basis".into(), "0px");
            }
            [g, s] if g.parse::<f32>().is_ok() && s.parse::<f32>().is_ok() => {
                push("flex-grow".into(), g);
                push("flex-shrink".into(), s);
                push("flex-basis".into(), "0px");
            }
            [g, s, b] => {
                push("flex-grow".into(), g);
                push("flex-shrink".into(), s);
                push("flex-basis".into(), b);
            }
            [g, b] if g.parse::<f32>().is_ok() => {
                push("flex-grow".into(), g);
                push("flex-shrink".into(), "1");
                push("flex-basis".into(), b);
            }
            [b] => {
                push("flex-grow".into(), "1");
                push("flex-shrink".into(), "1");
                push("flex-basis".into(), b);
            }
            _ => {}
        },
        "background" => {
            let mut colour = None;
            let mut image = None;
            for t in &v {
                if t.contains("gradient(") || t.starts_with("url(") {
                    image = Some(*t);
                } else if parse_color(t).is_some() || *t == "currentcolor" {
                    colour = Some(*t);
                }
            }
            push("background-color".into(), colour.unwrap_or("transparent"));
            push("background-image".into(), image.unwrap_or("none"));
        }
        "font" => {
            // [style] [variant] [weight] size[/line-height] family
            let mut style = "normal";
            let mut weight = "normal";
            let mut size = None;
            let mut lh = None;
            let mut family: Vec<&str> = Vec::new();
            for (i, t) in v.iter().enumerate() {
                if size.is_some() {
                    family.push(t);
                    continue;
                }
                match *t {
                    "italic" | "oblique" => style = "italic",
                    "bold" | "bolder" | "lighter" | "100" | "200" | "300" | "400" | "500"
                    | "600" | "700" | "800" | "900" => weight = t,
                    "normal" | "small-caps" => {}
                    t => {
                        let (sz, l) = t.split_once('/').map_or((t, None), |(a, b)| (a, Some(b)));
                        if parse_len(sz).is_some() || is_font_size_keyword(sz) {
                            size = Some(sz);
                            lh = l;
                        } else if i == 0 {
                            break;
                        }
                    }
                }
            }
            if let Some(sz) = size {
                push("font-style".into(), style);
                push("font-weight".into(), weight);
                push("font-size".into(), sz);
                push("line-height".into(), lh.unwrap_or("normal"));
                push("font-family".into(), &family.join(" "));
            }
        }
        "list-style" => {
            for t in &v {
                if matches!(
                    *t,
                    "none"
                        | "disc"
                        | "circle"
                        | "square"
                        | "decimal"
                        | "lower-alpha"
                        | "upper-alpha"
                        | "lower-roman"
                        | "upper-roman"
                        | "lower-latin"
                        | "upper-latin"
                ) {
                    push("list-style-type".into(), t);
                }
            }
        }
        "word-wrap" => push("overflow-wrap".into(), value),
        _ => push(name.to_string(), value),
    }
    out
}

fn is_font_size_keyword(s: &str) -> bool {
    matches!(
        s,
        "xx-small"
            | "x-small"
            | "small"
            | "medium"
            | "large"
            | "x-large"
            | "xx-large"
            | "smaller"
            | "larger"
    )
}

struct Ctx<'a> {
    parent: &'a Style,
    vp: Viewport,
}

fn to_dim(l: Len, font: f32, vp: Viewport) -> Dim {
    Dim {
        px: l.px + l.em * font + l.rem * ROOT_FONT_SIZE + l.vw * vp.w + l.vh * vp.h,
        pct: l.pct,
    }
}

fn dim_value(v: &str, font: f32, vp: Viewport) -> Option<Dim> {
    parse_len(v).map(|l| to_dim(l, font, vp))
}

fn size_value(v: &str, font: f32, vp: Viewport) -> Option<Size> {
    match parse_len_value(v)? {
        LenValue::Auto | LenValue::None => Some(Size::Auto),
        LenValue::Len(l) => Some(Size::Dim(to_dim(l, font, vp))),
    }
}

fn border_width_value(v: &str, font: f32, vp: Viewport) -> Option<f32> {
    match v {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        _ => dim_value(v, font, vp).map(|d| d.px.max(0.0)),
    }
}

fn colour_value(v: &str, current: u32) -> Option<u32> {
    if v.eq_ignore_ascii_case("currentcolor") {
        Some(current)
    } else {
        parse_color(v)
    }
}

fn parse_gradient(v: &str) -> Option<Gradient> {
    let lower = v.trim().to_ascii_lowercase();
    let inner = lower.strip_prefix("linear-gradient(")?.strip_suffix(')')?;
    let parts: Vec<&str> = {
        let mut out = Vec::new();
        let mut depth = 0;
        let mut start = 0;
        for (i, c) in inner.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(inner[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.push(inner[start..].trim());
        out
    };
    let mut angle = 180.0;
    let mut first = 0;
    if let Some(a) = parts.first() {
        if let Some(d) = a
            .strip_suffix("deg")
            .and_then(|x| x.trim().parse::<f32>().ok())
        {
            angle = d;
            first = 1;
        } else if let Some(dir) = a.strip_prefix("to ") {
            angle = match dir.trim() {
                "top" => 0.0,
                "right" => 90.0,
                "bottom" => 180.0,
                "left" => 270.0,
                "top right" | "right top" => 45.0,
                "bottom right" | "right bottom" => 135.0,
                "bottom left" | "left bottom" => 225.0,
                "top left" | "left top" => 315.0,
                _ => return None,
            };
            first = 1;
        }
    }
    let mut stops: Vec<(Option<f32>, u32)> = Vec::new();
    for p in &parts[first..] {
        let toks = split_values(p);
        let col = parse_color(toks.first()?)?;
        let pos = toks
            .get(1)
            .and_then(|t| t.strip_suffix('%'))
            .and_then(|t| t.parse::<f32>().ok())
            .map(|x| x / 100.0);
        stops.push((pos, col));
    }
    if stops.len() < 2 || stops.len() > 16 {
        return None;
    }
    let n = stops.len();
    let resolved = stops
        .iter()
        .enumerate()
        .map(|(i, (p, c))| (p.unwrap_or(i as f32 / (n - 1) as f32).clamp(0.0, 1.0), *c))
        .collect();
    Some(Gradient {
        angle,
        stops: resolved,
    })
}

fn font_size_value(v: &str, ctx: &Ctx<'_>) -> Option<f32> {
    let base = ctx.parent.font_size;
    let keyword = match v {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "smaller" => Some(base / 1.2),
        "larger" => Some(base * 1.2),
        _ => None,
    };
    if let Some(k) = keyword {
        return Some(k);
    }
    let l = parse_len(v)?;
    let px = l.px
        + l.em * base
        + l.rem * ROOT_FONT_SIZE
        + l.pct * base
        + l.vw * ctx.vp.w
        + l.vh * ctx.vp.h;
    (px.is_finite() && px >= 0.0).then_some(px.min(2000.0))
}

fn apply(s: &mut Style, name: &str, raw: &str, ctx: &Ctx<'_>) {
    let v = raw.trim();
    let lower = v.to_ascii_lowercase();
    // CSS-wide keywords.
    match lower.as_str() {
        "inherit" => {
            apply_inherit(s, name, ctx.parent);
            return;
        }
        "initial" | "unset" | "revert" => {
            let init = Style::initial();
            let inherited = matches!(
                name,
                "color"
                    | "font-size"
                    | "font-weight"
                    | "font-style"
                    | "font-family"
                    | "line-height"
                    | "letter-spacing"
                    | "text-align"
                    | "text-transform"
                    | "white-space"
                    | "list-style-type"
                    | "visibility"
            );
            if lower == "unset" && inherited {
                apply_inherit(s, name, ctx.parent);
            } else {
                apply_inherit(s, name, &init);
            }
            return;
        }
        _ => {}
    }
    let font = s.font_size;
    let vp = ctx.vp;
    let kw = lower.as_str();
    match name {
        "display" => {
            s.display = match kw {
                "none" => Display::None,
                "block" | "flow-root" | "grid" | "inline-grid" | "contents" => Display::Block,
                "inline" => Display::Inline,
                "inline-block" => Display::InlineBlock,
                "flex" => Display::Flex,
                "inline-flex" => Display::InlineFlex,
                "list-item" => Display::ListItem,
                "table" | "inline-table" => Display::Table,
                "table-row" => Display::TableRow,
                "table-cell" => Display::TableCell,
                "table-row-group" | "table-header-group" | "table-footer-group" => {
                    Display::TableRowGroup
                }
                _ => return,
            }
        }
        "position" => {
            s.position = match kw {
                "static" => Position::Static,
                "relative" | "sticky" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                _ => return,
            }
        }
        "float" => {
            s.float = match kw {
                "none" => Float::None,
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                _ => return,
            }
        }
        "clear" => {
            s.clear = match kw {
                "none" => Clear::None,
                "left" => Clear::Left,
                "right" => Clear::Right,
                "both" => Clear::Both,
                _ => return,
            }
        }
        "visibility" => s.hidden = matches!(kw, "hidden" | "collapse"),
        "opacity" => {
            if let Ok(o) = v.trim_end_matches('%').parse::<f32>() {
                s.opacity = (if v.ends_with('%') { o / 100.0 } else { o }).clamp(0.0, 1.0);
            }
        }
        "width" => {
            if let Some(x) = size_value(v, font, vp) {
                s.width = x;
            }
        }
        "height" => {
            if let Some(x) = size_value(v, font, vp) {
                s.height = x;
            }
        }
        "min-width" => {
            if let Some(x) = dim_value(v, font, vp) {
                s.min_width = x;
            } else if kw == "auto" {
                s.min_width = Dim::default();
            }
        }
        "min-height" => {
            if let Some(x) = dim_value(v, font, vp) {
                s.min_height = x;
            } else if kw == "auto" {
                s.min_height = Dim::default();
            }
        }
        "max-width" => {
            if let Some(x) = size_value(v, font, vp) {
                s.max_width = x;
            }
        }
        "max-height" => {
            if let Some(x) = size_value(v, font, vp) {
                s.max_height = x;
            }
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            let i = SIDES.iter().position(|x| name.ends_with(x)).unwrap_or(0);
            match kw {
                "auto" => s.margin[i] = Size::Auto,
                _ => {
                    if let Some(d) = dim_value(v, font, vp) {
                        s.margin[i] = Size::Dim(d);
                    }
                }
            }
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
            let i = SIDES.iter().position(|x| name.ends_with(x)).unwrap_or(0);
            if let Some(d) = dim_value(v, font, vp) {
                s.padding[i] = Dim {
                    px: d.px.max(0.0),
                    pct: d.pct.max(0.0),
                };
            }
        }
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            let i = SIDES.iter().position(|x| name.contains(x)).unwrap_or(0);
            if let Some(w) = border_width_value(kw, font, vp) {
                s.border_width[i] = w;
            }
        }
        "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            let i = SIDES.iter().position(|x| name.contains(x)).unwrap_or(0);
            s.border_style[i] = match kw {
                "none" | "hidden" => BorderStyle::None,
                "dashed" => BorderStyle::Dashed,
                "dotted" => BorderStyle::Dotted,
                "double" => BorderStyle::Double,
                "solid" | "groove" | "ridge" | "inset" | "outset" => BorderStyle::Solid,
                _ => return,
            };
        }
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => {
            let i = SIDES.iter().position(|x| name.contains(x)).unwrap_or(0);
            if kw == "currentcolor" {
                s.border_color[i] = CURRENT;
            } else if let Some(c) = parse_color(v) {
                s.border_color[i] = c;
            }
        }
        "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius" => {
            let i = ["top-left", "top-right", "bottom-right", "bottom-left"]
                .iter()
                .position(|x| name.contains(x))
                .unwrap_or(0);
            if let Some(d) = dim_value(v.split_whitespace().next().unwrap_or(""), font, vp) {
                s.radius[i] = Dim {
                    px: d.px.max(0.0),
                    pct: d.pct.max(0.0),
                };
            }
        }
        "box-sizing" => s.border_box = kw == "border-box",
        "overflow-x" | "overflow-y" => {
            let o = match kw {
                "visible" => Overflow::Visible,
                "hidden" | "clip" => Overflow::Hidden,
                "scroll" => Overflow::Scroll,
                "auto" => Overflow::Auto,
                _ => return,
            };
            if name == "overflow-x" {
                s.overflow_x = o;
            } else {
                s.overflow_y = o;
            }
        }
        "top" | "right" | "bottom" | "left" => {
            let i = SIDES.iter().position(|x| *x == name).unwrap_or(0);
            if let Some(x) = size_value(v, font, vp) {
                s.inset[i] = x;
            }
        }
        "z-index" => {
            if let Ok(z) = v.parse::<i32>() {
                s.z_index = z;
            }
        }
        "color" => {
            if let Some(c) = colour_value(v, ctx.parent.color) {
                s.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = colour_value(v, s.color) {
                s.background = c;
            }
        }
        "background-image" => {
            if kw == "none" {
                s.gradient = None;
            } else if let Some(g) = parse_gradient(v) {
                s.gradient = Some(g);
            }
        }
        "font-size" => {
            if let Some(px) = font_size_value(kw, ctx) {
                s.font_size = px;
            }
        }
        "font-weight" => {
            s.bold = match kw {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                n => match n.parse::<u32>() {
                    Ok(w) => w >= 600,
                    Err(_) => return,
                },
            }
        }
        "font-style" => s.italic = matches!(kw, "italic" | "oblique"),
        "font-family" => {
            let l = kw;
            let first = l
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(|c| c == '"' || c == '\'');
            s.mono = first == "monospace"
                || first.contains("mono")
                || first.contains("courier")
                || first.contains("consolas")
                || (l.contains("monospace") && !l.contains("sans") && !l.contains("serif"));
        }
        "line-height" => {
            s.line_height = match kw {
                "normal" => LineHeight::Normal,
                n => {
                    if let Ok(x) = n.parse::<f32>() {
                        LineHeight::Number(x.max(0.0))
                    } else if let Some(d) = dim_value(n, font, vp) {
                        LineHeight::Px((d.px + d.pct * font).max(0.0))
                    } else {
                        return;
                    }
                }
            }
        }
        "letter-spacing" => {
            if kw == "normal" {
                s.letter_spacing = 0.0;
            } else if let Some(d) = dim_value(v, font, vp) {
                s.letter_spacing = d.px;
            }
        }
        "text-align" => {
            s.text_align = match kw {
                "left" | "start" => TextAlign::Left,
                "right" | "end" => TextAlign::Right,
                "center" => TextAlign::Center,
                "justify" => TextAlign::Justify,
                _ => return,
            }
        }
        "text-decoration" | "text-decoration-line" => {
            s.underline = kw.contains("underline");
            s.strike = kw.contains("line-through");
        }
        "text-transform" => {
            s.text_transform = match kw {
                "none" => TextTransform::None,
                "uppercase" => TextTransform::Upper,
                "lowercase" => TextTransform::Lower,
                "capitalize" => TextTransform::Capitalize,
                _ => return,
            }
        }
        "white-space" => {
            s.white_space = match kw {
                "normal" => WhiteSpace::Normal,
                "nowrap" => WhiteSpace::NoWrap,
                "pre" => WhiteSpace::Pre,
                "pre-wrap" | "break-spaces" => WhiteSpace::PreWrap,
                "pre-line" => WhiteSpace::PreLine,
                _ => return,
            }
        }
        "overflow-wrap" | "word-break" => {
            s.break_words = matches!(kw, "break-word" | "anywhere" | "break-all")
        }
        "list-style-type" => {
            s.list_style = match kw {
                "none" => ListStyle::None,
                "disc" => ListStyle::Disc,
                "circle" => ListStyle::Circle,
                "square" => ListStyle::Square,
                "decimal" | "decimal-leading-zero" => ListStyle::Decimal,
                "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
                "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
                "lower-roman" => ListStyle::LowerRoman,
                "upper-roman" => ListStyle::UpperRoman,
                _ => return,
            }
        }
        "vertical-align" => {
            s.vertical_align = match kw {
                "baseline" => VAlign::Baseline,
                "top" | "text-top" => VAlign::Top,
                "middle" => VAlign::Middle,
                "bottom" | "text-bottom" => VAlign::Bottom,
                "sub" => VAlign::Sub,
                "super" => VAlign::Super,
                _ => return,
            }
        }
        "flex-direction" => {
            s.flex_direction = match kw {
                "row" => FlexDir::Row,
                "row-reverse" => FlexDir::RowReverse,
                "column" => FlexDir::Column,
                "column-reverse" => FlexDir::ColumnReverse,
                _ => return,
            }
        }
        "flex-wrap" => {
            s.flex_wrap = match kw {
                "nowrap" => FlexWrap::NoWrap,
                "wrap" => FlexWrap::Wrap,
                "wrap-reverse" => FlexWrap::WrapReverse,
                _ => return,
            }
        }
        "justify-content" => {
            s.justify = match kw {
                "flex-start" | "start" | "left" | "normal" | "stretch" => Justify::Start,
                "flex-end" | "end" | "right" => Justify::End,
                "center" => Justify::Center,
                "space-between" => Justify::SpaceBetween,
                "space-around" => Justify::SpaceAround,
                "space-evenly" => Justify::SpaceEvenly,
                _ => return,
            }
        }
        "align-items" | "align-self" => {
            let a = match kw {
                "stretch" | "normal" => Align::Stretch,
                "flex-start" | "start" | "self-start" => Align::Start,
                "flex-end" | "end" | "self-end" => Align::End,
                "center" => Align::Center,
                "baseline" => Align::Baseline,
                "auto" if name == "align-self" => {
                    s.align_self = None;
                    return;
                }
                _ => return,
            };
            if name == "align-items" {
                s.align_items = a;
            } else {
                s.align_self = Some(a);
            }
        }
        "flex-grow" => {
            if let Ok(x) = v.parse::<f32>() {
                s.flex_grow = x.max(0.0);
            }
        }
        "flex-shrink" => {
            if let Ok(x) = v.parse::<f32>() {
                s.flex_shrink = x.max(0.0);
            }
        }
        "flex-basis" => {
            if let Some(x) = size_value(v, font, vp) {
                s.flex_basis = x;
            }
        }
        "order" => {
            if let Ok(x) = v.parse::<i32>() {
                s.order = x;
            }
        }
        "row-gap" => {
            if let Some(d) = dim_value(v, font, vp) {
                s.row_gap = d;
            }
        }
        "column-gap" => {
            if let Some(d) = dim_value(v, font, vp) {
                s.column_gap = d;
            }
        }
        "border-spacing" => {
            if let Some(d) = dim_value(v.split_whitespace().next().unwrap_or(""), font, vp) {
                s.border_spacing = d.px.max(0.0);
            }
        }
        "border-collapse" => s.border_collapse = kw == "collapse",
        _ => {}
    }
}

fn apply_inherit(s: &mut Style, name: &str, from: &Style) {
    let side = |names: [&str; 4], n: &str| names.iter().position(|x| *x == n);
    match name {
        "display" => s.display = from.display,
        "color" => s.color = from.color,
        "background-color" => s.background = from.background,
        "font-size" => s.font_size = from.font_size,
        "font-weight" => s.bold = from.bold,
        "font-style" => s.italic = from.italic,
        "font-family" => s.mono = from.mono,
        "line-height" => s.line_height = from.line_height,
        "letter-spacing" => s.letter_spacing = from.letter_spacing,
        "text-align" => s.text_align = from.text_align,
        "text-transform" => s.text_transform = from.text_transform,
        "white-space" => s.white_space = from.white_space,
        "list-style-type" => s.list_style = from.list_style,
        "visibility" => s.hidden = from.hidden,
        "width" => s.width = from.width,
        "height" => s.height = from.height,
        "opacity" => s.opacity = from.opacity,
        "position" => s.position = from.position,
        "float" => s.float = from.float,
        "box-sizing" => s.border_box = from.border_box,
        n => {
            if let Some(i) = side(
                ["margin-top", "margin-right", "margin-bottom", "margin-left"],
                n,
            ) {
                s.margin[i] = from.margin[i];
            } else if let Some(i) = side(
                [
                    "padding-top",
                    "padding-right",
                    "padding-bottom",
                    "padding-left",
                ],
                n,
            ) {
                s.padding[i] = from.padding[i];
            } else if let Some(i) = side(
                [
                    "border-top-width",
                    "border-right-width",
                    "border-bottom-width",
                    "border-left-width",
                ],
                n,
            ) {
                s.border_width[i] = from.border_width[i];
            } else if let Some(i) = side(
                [
                    "border-top-color",
                    "border-right-color",
                    "border-bottom-color",
                    "border-left-color",
                ],
                n,
            ) {
                s.border_color[i] = from.border_color[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VP: Viewport = Viewport { w: 800.0, h: 600.0 };

    fn styles(html: &str, css: &str) -> (Dom, Styles) {
        let dom = Dom::parse(html);
        let sheets = if css.is_empty() {
            vec![]
        } else {
            vec![Stylesheet::parse(css)]
        };
        let s = compute(&dom, &sheets, VP, &State::default());
        (dom, s)
    }

    fn of<'a>(dom: &Dom, s: &'a Styles, tag: &str) -> &'a Style {
        s.get(dom.find_all(tag)[0]).unwrap()
    }

    fn px(s: Size) -> f32 {
        s.dim().unwrap().px
    }

    #[test]
    fn the_default_stylesheet_gives_browser_defaults() {
        let (d, s) = styles("<h1>T</h1><p>p</p><ul><li>x</li></ul><b>b</b><a href=x>l</a><code>c</code><pre>p</pre>", "");
        let h1 = of(&d, &s, "h1");
        assert_eq!(
            (h1.display, h1.font_size, h1.bold),
            (Display::Block, 32.0, true)
        );
        assert!((px(h1.margin[0]) - 21.44).abs() < 0.01, "0.67em of 32px");
        assert_eq!(px(of(&d, &s, "body").margin[1]), 8.0);
        assert_eq!(px(of(&d, &s, "p").margin[0]), 16.0);
        assert_eq!(of(&d, &s, "li").display, Display::ListItem);
        assert_eq!(of(&d, &s, "ul").padding[3].px, 40.0);
        assert!(of(&d, &s, "b").bold);
        let a = of(&d, &s, "a");
        assert_eq!((a.color, a.underline), (0xFF00_00EE, true));
        let code = of(&d, &s, "code");
        assert!(code.mono && (code.font_size - 13.0).abs() < 0.01);
        assert_eq!(of(&d, &s, "pre").white_space, WhiteSpace::Pre);
        assert_eq!(of(&d, &s, "head").display, Display::None);
        assert_eq!(of(&d, &s, "html").display, Display::Block);
    }

    #[test]
    fn specificity_order_and_important_decide() {
        let css = "p { color: red } .a { color: green } #i { color: blue } p { color: yellow } .b { color: lime !important } p.b { color: gray }";
        let (d, s) = styles(
            r#"<p id="i" class="a b">x</p><p class="a">y</p><p>z</p>"#,
            css,
        );
        let ps = d.find_all("p");
        assert_eq!(
            s.get(ps[0]).unwrap().color,
            0xFF00_FF00,
            "!important beats the id"
        );
        assert_eq!(s.get(ps[1]).unwrap().color, 0xFF00_8000, "class beats tag");
        assert_eq!(
            s.get(ps[2]).unwrap().color,
            0xFFFF_FF00,
            "later rule wins at equal specificity"
        );
        let (d, s) = styles(
            r#"<p id="i" style="color: red">x</p>"#,
            "#i { color: blue }",
        );
        assert_eq!(
            of(&d, &s, "p").color,
            0xFFFF_0000,
            "the style attribute beats an id selector"
        );
        let (d, s) = styles(
            r#"<p style="color: red">x</p>"#,
            "p { color: blue !important }",
        );
        assert_eq!(
            of(&d, &s, "p").color,
            0xFF00_00FF,
            "an important author rule beats a normal inline style"
        );
    }

    #[test]
    fn inheritance_and_the_css_wide_keywords() {
        let css = "div { color: red; font-size: 20px; border: 1px solid blue; background: yellow } span { font-size: 1.5em } p { color: inherit; border-color: inherit } .u { color: unset } .i { background-color: inherit } .z { color: initial }";
        let (d, s) = styles(
            r#"<div><span>a<b>b</b></span><p>p</p><i class="u">u</i><i class="i">i</i><i class="z">z</i></div>"#,
            css,
        );
        let span = of(&d, &s, "span");
        assert_eq!(
            (span.color, span.font_size),
            (0xFFFF_0000, 30.0),
            "colour inherited, em relative to the parent's size"
        );
        assert_eq!(of(&d, &s, "b").font_size, 30.0);
        assert_eq!(span.border_width, [0.0; 4], "borders are not inherited");
        assert_eq!(span.background, 0, "backgrounds are not inherited");
        let p = of(&d, &s, "p");
        assert_eq!(p.color, 0xFFFF_0000);
        let is = d.find_all("i");
        assert_eq!(
            s.get(is[0]).unwrap().color,
            0xFFFF_0000,
            "unset on an inherited property inherits"
        );
        assert_eq!(
            s.get(is[1]).unwrap().background,
            0xFFFF_FF00,
            "inherit takes the parent's value"
        );
        assert_eq!(s.get(is[2]).unwrap().color, 0xFF00_0000, "initial is black");
    }

    #[test]
    fn shorthands_expand() {
        let css = "div { margin: 1px 2px 3px 4px; padding: 5px 6px; border: 2px dashed red } p { margin: 0 auto; border-top: 3px solid; border-radius: 4px 8px } span { font: italic bold 20px/30px Georgia, serif } i { flex: 2 1 10%; gap: 4px 8px } b { overflow: hidden scroll; list-style: square } u { inset: 1px 2px }";
        let (d, s) = styles(
            "<div>a</div><p>b</p><span>c</span><i>d</i><b>e</b><u>f</u>",
            css,
        );
        let div = of(&d, &s, "div");
        assert_eq!(div.margin.map(px), [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(div.padding.map(|x| x.px), [5.0, 6.0, 5.0, 6.0]);
        assert_eq!(
            (div.border_width, div.border_style[0], div.border_color[0]),
            ([2.0; 4], BorderStyle::Dashed, 0xFFFF_0000)
        );
        let p = of(&d, &s, "p");
        assert_eq!(
            (p.margin[0], p.margin[1]),
            (Size::Dim(Dim::default()), Size::Auto)
        );
        assert_eq!(
            (p.border_width, p.border_style[0], p.border_style[1]),
            ([3.0, 0.0, 0.0, 0.0], BorderStyle::Solid, BorderStyle::None)
        );
        assert_eq!(p.border_color[0], p.color, "no colour means currentcolor");
        assert_eq!(p.radius.map(|x| x.px), [4.0, 8.0, 4.0, 8.0]);
        let span = of(&d, &s, "span");
        assert_eq!(
            (span.italic, span.bold, span.font_size, span.line_height),
            (true, true, 20.0, LineHeight::Px(30.0))
        );
        let i = of(&d, &s, "i");
        assert_eq!(
            (i.flex_grow, i.flex_shrink, i.flex_basis),
            (2.0, 1.0, Size::Dim(Dim { px: 0.0, pct: 0.1 }))
        );
        assert_eq!((i.row_gap.px, i.column_gap.px), (4.0, 8.0));
        let b = of(&d, &s, "b");
        assert_eq!(
            (b.overflow_x, b.overflow_y, b.list_style),
            (Overflow::Hidden, Overflow::Scroll, ListStyle::Square)
        );
        assert_eq!(
            of(&d, &s, "u").inset.map(|x| x.dim().map(|d| d.px)),
            [Some(1.0), Some(2.0), Some(1.0), Some(2.0)]
        );
    }

    #[test]
    fn lengths_resolve_against_the_right_base() {
        let css = "div { font-size: 10px; width: 5em; height: 2rem; margin-left: 10vw; padding-top: 50%; min-height: 10vh; max-width: calc(100% - 2em) }";
        let (d, s) = styles("<div>x</div>", css);
        let div = of(&d, &s, "div");
        assert_eq!(div.width, Size::Dim(Dim { px: 50.0, pct: 0.0 }));
        assert_eq!(div.height, Size::Dim(Dim { px: 32.0, pct: 0.0 }));
        assert_eq!(px(div.margin[3]), 80.0);
        assert_eq!(div.padding[0], Dim { px: 0.0, pct: 0.5 });
        assert_eq!(div.min_height.px, 60.0);
        assert_eq!(
            div.max_width,
            Size::Dim(Dim {
                px: -20.0,
                pct: 1.0
            })
        );
        assert_eq!(div.max_width.dim().unwrap().resolve(500.0), 480.0);
    }

    #[test]
    fn custom_properties_substitute_inherit_and_fall_back() {
        let css = ":root { --main: #00f; --gap: 10px; --pair: var(--gap) } div { color: var(--main); margin-top: var(--pair); padding-top: var(--nope, 7px); width: var(--nope) } span { --main: red } .c { color: var(--main) }";
        let (d, s) = styles(r#"<div>a<span class="c">b</span></div>"#, css);
        let div = of(&d, &s, "div");
        assert_eq!(div.color, 0xFF00_00FF);
        assert_eq!(px(div.margin[0]), 10.0, "variables can refer to variables");
        assert_eq!(div.padding[0].px, 7.0, "the fallback is used");
        assert_eq!(
            div.width,
            Size::Auto,
            "an undefined variable without a fallback drops the declaration"
        );
        assert_eq!(
            of(&d, &s, "span").color,
            0xFFFF_0000,
            "an element can override a variable for itself and its children"
        );
        let loopy = ":root { --a: var(--b); --b: var(--a) } p { color: var(--a); margin-top: 3px }";
        let (d, s) = styles("<p>x</p>", loopy);
        assert_eq!(
            px(of(&d, &s, "p").margin[0]),
            3.0,
            "a variable cycle is ignored, not followed forever"
        );
    }

    #[test]
    fn media_queries_and_hover_follow_the_inputs() {
        let css = "p { color: red } @media (max-width: 500px) { p { color: green } } @media (min-width: 700px) { p { color: blue } } a:hover { color: lime }";
        let (d, s) = styles("<p>x</p>", css);
        assert_eq!(
            of(&d, &s, "p").color,
            0xFF00_00FF,
            "an 800px viewport matches the min-width rule"
        );
        let dom = Dom::parse("<a href=x>l</a>");
        let sheets = vec![Stylesheet::parse(css)];
        let a = dom.find_all("a")[0];
        let normal = compute(&dom, &sheets, VP, &State::default());
        let hovered = compute(
            &dom,
            &sheets,
            VP,
            &State {
                hover: Some(a),
                ..State::default()
            },
        );
        assert_eq!(normal.get(a).unwrap().color, 0xFF00_00EE);
        assert_eq!(hovered.get(a).unwrap().color, 0xFF00_FF00);
        let narrow = compute(
            &Dom::parse("<p>x</p>"),
            &sheets,
            Viewport { w: 400.0, h: 300.0 },
            &State::default(),
        );
        let d2 = Dom::parse("<p>x</p>");
        assert_eq!(narrow.get(d2.find_all("p")[0]).unwrap().color, 0xFF00_8000);
    }

    #[test]
    fn unreadable_values_are_ignored_and_the_earlier_value_stays() {
        let css = "p { color: red; color: notacolour; width: 10px; width: 10; display: flux; margin: 1px 2px 3px 4px 5px; font-size: huge }";
        let (d, s) = styles("<p>x</p>", css);
        let p = of(&d, &s, "p");
        assert_eq!(p.color, 0xFFFF_0000);
        assert_eq!(p.width, Size::Dim(Dim { px: 10.0, pct: 0.0 }));
        assert_eq!(p.display, Display::Block);
        assert_eq!(
            px(p.margin[0]),
            16.0,
            "five values is invalid, the default stays"
        );
        assert_eq!(p.font_size, 16.0);
    }

    #[test]
    fn display_visibility_hidden_attribute_and_text_properties() {
        let css = ".h { visibility: hidden } .t { text-transform: uppercase; letter-spacing: 2px; text-decoration: underline line-through; white-space: nowrap; text-align: center; line-height: 1.5; font-weight: 700; font-family: 'Courier New', monospace }";
        let (d, s) = styles(
            r#"<p hidden>a</p><div class="h">b</div><div class="t">c</div><script>x</script>"#,
            css,
        );
        assert_eq!(of(&d, &s, "p").display, Display::None);
        assert!(s.get(d.find_all("div")[0]).unwrap().hidden);
        let t = s.get(d.find_all("div")[1]).unwrap();
        assert_eq!(
            (
                t.text_transform,
                t.letter_spacing,
                t.underline,
                t.strike,
                t.white_space,
                t.text_align,
                t.line_height,
                t.bold,
                t.mono
            ),
            (
                TextTransform::Upper,
                2.0,
                true,
                true,
                WhiteSpace::NoWrap,
                TextAlign::Center,
                LineHeight::Number(1.5),
                true,
                true
            )
        );
        assert_eq!(of(&d, &s, "script").display, Display::None);
    }

    #[test]
    fn gradients_and_colours_parse() {
        let css = "div { background: linear-gradient(to right, #f00, #00f 50%, rgb(0 255 0)) } p { background: #123 url(x.png) } span { background-image: linear-gradient(45deg, red, blue) } i { background: linear-gradient(red) }";
        let (d, s) = styles("<div>a</div><p>b</p><span>c</span><i>d</i>", css);
        let g = of(&d, &s, "div").gradient.clone().unwrap();
        assert_eq!(g.angle, 90.0);
        assert_eq!(
            g.stops,
            vec![(0.0, 0xFFFF_0000), (0.5, 0xFF00_00FF), (1.0, 0xFF00_FF00)]
        );
        assert_eq!(of(&d, &s, "p").background, 0xFF11_2233);
        assert_eq!(of(&d, &s, "span").gradient.clone().unwrap().angle, 45.0);
        assert!(
            of(&d, &s, "i").gradient.is_none(),
            "a gradient needs two stops"
        );
    }

    #[test]
    fn a_large_document_is_styled_quickly() {
        let html = "<div class='a'><p class='b'>x</p><span>y</span></div>".repeat(3000);
        let css = ".a{color:red}.b{margin:1px}div p{padding:2px}span{font-weight:bold}p:first-child{color:blue}".repeat(20);
        let t = std::time::Instant::now();
        let (d, s) = styles(&html, &css);
        assert!(s.get(d.find_all("p")[0]).is_some());
        assert!(t.elapsed().as_secs() < 10, "took {:?}", t.elapsed());
    }
}
