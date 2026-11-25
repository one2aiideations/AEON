//! CSS: parsing stylesheets, selectors and matching, and the value types (colours, lengths).
//!
//! The parser is tolerant: a rule or declaration it does not understand is skipped, never an
//! error, and everything is bounded in size. Supported: type, class, id and attribute selectors,
//! the descendant, child, adjacent and general sibling combinators, `:first-child`, `:last-child`,
//! `:only-child`, `:nth-child()`, `:nth-last-child()`, `:first-of-type`, `:last-of-type`, `:root`,
//! `:empty`, `:not()`, `:hover`, `:focus`, `:active`, `:checked`, `:disabled`, `:link`; `@media`
//! with width and height conditions; `!important`. Not supported (the rule is ignored): pseudo-
//! elements (`::before`), `@keyframes`, `@font-face`, `@supports`, container queries.

use crate::dom::{Dom, NodeId};

/// Most characters of CSS read.
pub const MAX_CSS: usize = 1_000_000;
/// Most rules kept.
pub const MAX_RULES: usize = 20_000;

/// A property and its value, as written (the value is parsed later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    /// Lowercase property name (or `--custom` name, case kept).
    pub name: String,
    /// The value text, trimmed, without `!important`.
    pub value: String,
    /// Whether it was marked `!important`.
    pub important: bool,
}

/// How two compound selectors relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `a b`
    Descendant,
    /// `a > b`
    Child,
    /// `a + b`
    Adjacent,
    /// `a ~ b`
    Sibling,
}

/// How an attribute selector compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrOp {
    /// `[a]`
    Exists,
    /// `[a=v]`
    Equals(String),
    /// `[a~=v]`
    Word(String),
    /// `[a^=v]`
    Prefix(String),
    /// `[a$=v]`
    Suffix(String),
    /// `[a*=v]`
    Contains(String),
}

/// A pseudo-class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pseudo {
    /// `:first-child`
    FirstChild,
    /// `:last-child`
    LastChild,
    /// `:only-child`
    OnlyChild,
    /// `:nth-child(an+b)`
    NthChild(i32, i32),
    /// `:nth-last-child(an+b)`
    NthLastChild(i32, i32),
    /// `:first-of-type`
    FirstOfType,
    /// `:last-of-type`
    LastOfType,
    /// `:root`
    Root,
    /// `:empty`
    Empty,
    /// `:not(...)`
    Not(Box<Compound>),
    /// `:hover`
    Hover,
    /// `:focus`
    Focus,
    /// `:active`
    Active,
    /// `:checked`
    Checked,
    /// `:disabled`
    Disabled,
    /// `:link` and `:any-link`
    Link,
}

/// One compound selector (`div.note#x[a=b]:first-child`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Compound {
    /// A tag name, or `None` for any.
    pub tag: Option<String>,
    /// An `#id`.
    pub id: Option<String>,
    /// `.class`es.
    pub classes: Vec<String>,
    /// `[attribute]` tests.
    pub attrs: Vec<(String, AttrOp)>,
    /// `:pseudo-class`es.
    pub pseudos: Vec<Pseudo>,
    /// True for something this engine cannot match (a pseudo-element): the selector never matches.
    pub never: bool,
}

/// A complex selector: compounds joined by combinators, and its specificity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    /// From left to right; the combinator of the first entry is unused.
    pub parts: Vec<(Combinator, Compound)>,
    /// Specificity as `ids * 10000 + classes * 100 + tags` (capped).
    pub specificity: u32,
}

/// A media condition on the viewport size.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Media {
    /// `min-width`
    pub min_w: Option<f32>,
    /// `max-width`
    pub max_w: Option<f32>,
    /// `min-height`
    pub min_h: Option<f32>,
    /// `max-height`
    pub max_h: Option<f32>,
    /// A query that can never match here (print, speech, unsupported features).
    pub never: bool,
}

impl Media {
    /// Whether the condition holds for a viewport of this size.
    #[must_use]
    pub fn matches(&self, w: f32, h: f32) -> bool {
        !self.never
            && self.min_w.is_none_or(|v| w >= v)
            && self.max_w.is_none_or(|v| w <= v)
            && self.min_h.is_none_or(|v| h >= v)
            && self.max_h.is_none_or(|v| h <= v)
    }
}

/// A rule: selectors and the declarations they apply.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// The selectors (a comma list).
    pub selectors: Vec<Selector>,
    /// The declarations.
    pub decls: Vec<Decl>,
    /// The `@media` condition around the rule, if any.
    pub media: Option<Media>,
}

/// A parsed stylesheet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stylesheet {
    /// The rules in source order.
    pub rules: Vec<Rule>,
}

// ------------------------------------------------------------------ parsing

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i + 2..]
            .find("*/")
            .map_or("", |e| &rest[i + 2 + e + 2..]);
    }
    out.push_str(rest);
    out
}

/// Finds the byte index of the first `stop` that is not inside a string or brackets.
fn find_top(s: &str, stops: &[char]) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            c if depth == 0 && stops.contains(&c) => return Some(i),
            _ => {}
        }
    }
    None
}

/// The index of the `}` that closes a block whose `{` was just before `s`.
fn block_end(s: &str) -> usize {
    let mut depth = 1i32;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    s.len()
}

/// Parses the declarations inside a block (or a `style` attribute).
#[must_use]
pub fn parse_declarations(text: &str) -> Vec<Decl> {
    let text = strip_comments(text);
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while !rest.trim().is_empty() {
        let end = find_top(rest, &[';']).unwrap_or(rest.len());
        let item = &rest[..end];
        rest = if end < rest.len() {
            &rest[end + 1..]
        } else {
            ""
        };
        let Some(colon) = item.find(':') else {
            continue;
        };
        let name = item[..colon].trim();
        let mut value = item[colon + 1..].trim().to_string();
        let mut important = false;
        let lower = value.to_ascii_lowercase();
        if let Some(i) = lower.rfind("!important") {
            if lower[i + 10..].trim().is_empty() {
                value.truncate(i);
                value = value.trim().to_string();
                important = true;
            }
        }
        if name.is_empty() || value.is_empty() || name.len() > 100 || value.len() > 4000 {
            continue;
        }
        let name = if name.starts_with("--") {
            name.to_string()
        } else {
            name.to_ascii_lowercase()
        };
        if name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            out.push(Decl {
                name,
                value,
                important,
            });
        }
    }
    out
}

fn parse_media(q: &str) -> Media {
    let mut m = Media::default();
    let q = q.to_ascii_lowercase();
    if q.trim().is_empty() {
        return m;
    }
    // A comma list: it matches if any part does; keep the first screen/all part that parses.
    let part = q
        .split(',')
        .find(|p| !p.contains("print") && !p.contains("speech"))
        .unwrap_or("");
    if part.is_empty() {
        m.never = true;
        return m;
    }
    let mut rest = part;
    while let Some(i) = rest.find('(') {
        let Some(j) = rest[i..].find(')') else { break };
        let cond = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        let Some((k, v)) = cond.split_once(':') else {
            continue;
        };
        let px = parse_length_px(v.trim());
        match (k.trim(), px) {
            ("min-width", Some(x)) => m.min_w = Some(x),
            ("max-width", Some(x)) => m.max_w = Some(x),
            ("min-height", Some(x)) => m.min_h = Some(x),
            ("max-height", Some(x)) => m.max_h = Some(x),
            ("prefers-color-scheme", _) => m.never = !v.contains("light"),
            ("min-resolution" | "max-resolution" | "orientation" | "hover" | "pointer", _) => {}
            _ => m.never = true,
        }
    }
    if part.contains(" not ") || part.trim_start().starts_with("not ") {
        m.never = true;
    }
    m
}

/// Whole-number pixel value for media queries (`768px`, `48em` at 16px).
fn parse_length_px(s: &str) -> Option<f32> {
    let s = s.trim();
    if let Some(v) = s.strip_suffix("px") {
        return v.trim().parse().ok();
    }
    if let Some(v) = s.strip_suffix("em").or_else(|| s.strip_suffix("rem")) {
        return v.trim().parse::<f32>().ok().map(|x| x * 16.0);
    }
    None
}

impl Stylesheet {
    /// Parses a stylesheet. Anything it cannot read is skipped.
    #[must_use]
    pub fn parse(css: &str) -> Self {
        let css: String = strip_comments(&css.chars().take(MAX_CSS).collect::<String>());
        let mut sheet = Stylesheet::default();
        parse_block(&css, None, &mut sheet, 0);
        sheet
    }
}

fn parse_block(css: &str, media: Option<Media>, sheet: &mut Stylesheet, depth: usize) {
    let mut rest = css;
    while !rest.trim().is_empty() && sheet.rules.len() < MAX_RULES {
        rest = rest.trim_start_matches(|c: char| c == '}' || c.is_whitespace());
        if let Some(at) = rest.strip_prefix('@') {
            let stop = find_top(at, &[';', '{']);
            let Some(stop) = stop else { return };
            let head = at[..stop].trim();
            if at.as_bytes()[stop] == b';' {
                rest = &at[stop + 1..];
                continue;
            }
            let body_start = &at[stop + 1..];
            let end = block_end(body_start);
            let body = &body_start[..end];
            rest = body_start.get(end + 1..).unwrap_or("");
            let name = head
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if name == "media" && depth < 3 {
                let q = head[5..].trim();
                let mut m = parse_media(q);
                if let Some(outer) = media {
                    m.min_w = m.min_w.or(outer.min_w);
                    m.max_w = m.max_w.or(outer.max_w);
                    m.never |= outer.never;
                }
                parse_block(body, Some(m), sheet, depth + 1);
            }
            continue;
        }
        let Some(open) = find_top(rest, &['{']) else {
            return;
        };
        let prelude = &rest[..open];
        let body_start = &rest[open + 1..];
        let end = block_end(body_start);
        let body = &body_start[..end];
        rest = body_start.get(end + 1..).unwrap_or("");
        let selectors: Vec<Selector> = split_list(prelude)
            .iter()
            .filter_map(|s| parse_selector(s))
            .collect();
        if selectors.is_empty() {
            continue;
        }
        let decls = parse_declarations(body);
        if !decls.is_empty() {
            sheet.rules.push(Rule {
                selectors,
                decls,
                media,
            });
        }
    }
}

fn split_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        match find_top(rest, &[',']) {
            Some(i) => {
                out.push(rest[..i].trim().to_string());
                rest = &rest[i + 1..];
            }
            None => {
                out.push(rest.trim().to_string());
                return out;
            }
        }
    }
}

fn ident_end(s: &str) -> usize {
    s.char_indices()
        .find(|&(_, c)| !(c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()))
        .map_or(s.len(), |(i, _)| i)
}

fn parse_nth(arg: &str) -> Option<(i32, i32)> {
    let a: String = arg
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match a.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some((ap, bp)) = a.split_once('n') {
        let step = match ap {
            "" | "+" => 1,
            "-" => -1,
            x => x.parse().ok()?,
        };
        let off = if bp.is_empty() { 0 } else { bp.parse().ok()? };
        return Some((step, off));
    }
    a.parse().ok().map(|b| (0, b))
}

fn parse_compound(s: &str) -> Option<Compound> {
    let mut c = Compound::default();
    let mut rest = s;
    if rest.is_empty() {
        return None;
    }
    if let Some(r) = rest.strip_prefix('*') {
        rest = r;
    } else if !rest.starts_with(['.', '#', '[', ':']) {
        let n = ident_end(rest);
        let starts_ok = rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || ch == '_' || ch == '-' || !ch.is_ascii());
        if n == 0 || !starts_ok {
            return None;
        }
        c.tag = Some(rest[..n].to_ascii_lowercase());
        rest = &rest[n..];
    }
    while !rest.is_empty() {
        let first = rest.chars().next()?;
        match first {
            '.' => {
                let n = ident_end(&rest[1..]);
                if n == 0 {
                    return None;
                }
                c.classes.push(rest[1..1 + n].to_string());
                rest = &rest[1 + n..];
            }
            '#' => {
                let n = ident_end(&rest[1..]);
                if n == 0 {
                    return None;
                }
                c.id = Some(rest[1..1 + n].to_string());
                rest = &rest[1 + n..];
            }
            '[' => {
                let end = rest.find(']')?;
                let inner = &rest[1..end];
                rest = &rest[end + 1..];
                let name_end = inner
                    .find(['=', '~', '^', '$', '*', '|'])
                    .unwrap_or(inner.len());
                let name = inner[..name_end].trim().to_ascii_lowercase();
                if name.is_empty() {
                    return None;
                }
                let tail = &inner[name_end..];
                let unq = |v: &str| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                let op = if tail.is_empty() {
                    AttrOp::Exists
                } else if let Some(v) = tail.strip_prefix("~=") {
                    AttrOp::Word(unq(v))
                } else if let Some(v) = tail.strip_prefix("^=") {
                    AttrOp::Prefix(unq(v))
                } else if let Some(v) = tail.strip_prefix("$=") {
                    AttrOp::Suffix(unq(v))
                } else if let Some(v) = tail.strip_prefix("*=") {
                    AttrOp::Contains(unq(v))
                } else if let Some(v) = tail.strip_prefix('=') {
                    AttrOp::Equals(unq(
                        v.trim_end_matches(|c: char| c == 'i' || c.is_whitespace())
                    ))
                } else {
                    c.never = true;
                    AttrOp::Exists
                };
                c.attrs.push((name, op));
            }
            ':' => {
                if rest.starts_with("::") {
                    c.never = true;
                    let n = ident_end(&rest[2..]);
                    rest = &rest[2 + n..];
                    if rest.starts_with('(') {
                        rest = &rest[rest.find(')')? + 1..];
                    }
                    continue;
                }
                let n = ident_end(&rest[1..]);
                let name = rest[1..1 + n].to_ascii_lowercase();
                rest = &rest[1 + n..];
                let arg = if rest.starts_with('(') {
                    let mut depth = 0;
                    let mut end = None;
                    for (i, ch) in rest.char_indices() {
                        match ch {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = Some(i);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    let e = end?;
                    let a = rest[1..e].to_string();
                    rest = &rest[e + 1..];
                    Some(a)
                } else {
                    None
                };
                let p = match (name.as_str(), arg) {
                    ("first-child", None) => Pseudo::FirstChild,
                    ("last-child", None) => Pseudo::LastChild,
                    ("only-child", None) => Pseudo::OnlyChild,
                    ("first-of-type", None) => Pseudo::FirstOfType,
                    ("last-of-type", None) => Pseudo::LastOfType,
                    ("root", None) => Pseudo::Root,
                    ("empty", None) => Pseudo::Empty,
                    ("hover", None) => Pseudo::Hover,
                    ("focus" | "focus-within" | "focus-visible", None) => Pseudo::Focus,
                    ("active", None) => Pseudo::Active,
                    ("checked", None) => Pseudo::Checked,
                    ("disabled", None) => Pseudo::Disabled,
                    ("link" | "any-link", None) => Pseudo::Link,
                    ("nth-child", Some(a)) => {
                        let (x, y) = parse_nth(&a)?;
                        Pseudo::NthChild(x, y)
                    }
                    ("nth-last-child", Some(a)) => {
                        let (x, y) = parse_nth(&a)?;
                        Pseudo::NthLastChild(x, y)
                    }
                    ("not", Some(a)) => Pseudo::Not(Box::new(parse_compound(a.trim())?)),
                    // Old-style pseudo-elements and anything unknown: never matches.
                    ("before" | "after" | "first-line" | "first-letter", None) => {
                        c.never = true;
                        continue;
                    }
                    _ => {
                        c.never = true;
                        continue;
                    }
                };
                c.pseudos.push(p);
            }
            _ => return None,
        }
    }
    Some(c)
}

/// Parses one complex selector (no commas).
#[must_use]
pub fn parse_selector(s: &str) -> Option<Selector> {
    let s = s.trim();
    if s.is_empty() || s.len() > 1000 {
        return None;
    }
    let mut parts: Vec<(Combinator, Compound)> = Vec::new();
    let mut comb = Combinator::Descendant;
    let mut cur = String::new();
    let mut depth = 0i32;
    let flush = |cur: &mut String,
                 comb: Combinator,
                 parts: &mut Vec<(Combinator, Compound)>|
     -> Option<()> {
        if !cur.is_empty() {
            parts.push((comb, parse_compound(cur)?));
            cur.clear();
        }
        Some(())
    };
    let mut pending_comb: Option<Combinator> = None;
    for ch in s.chars() {
        if depth == 0 && (ch.is_whitespace() || matches!(ch, '>' | '+' | '~')) {
            if !cur.is_empty() {
                flush(&mut cur, pending_comb.take().unwrap_or(comb), &mut parts)?;
                comb = Combinator::Descendant;
            }
            match ch {
                '>' => pending_comb = Some(Combinator::Child),
                '+' => pending_comb = Some(Combinator::Adjacent),
                '~' => pending_comb = Some(Combinator::Sibling),
                _ => {}
            }
            continue;
        }
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        cur.push(ch);
    }
    let trailing = pending_comb.is_some() && cur.is_empty();
    flush(&mut cur, pending_comb.take().unwrap_or(comb), &mut parts)?;
    if parts.is_empty() || trailing {
        return None;
    }
    let mut spec = 0u32;
    for (_, c) in &parts {
        spec += compound_specificity(c);
    }
    Some(Selector {
        parts,
        specificity: spec.min(900_000),
    })
}

fn compound_specificity(c: &Compound) -> u32 {
    let mut s = 0;
    if c.id.is_some() {
        s += 10_000;
    }
    s += 100 * (c.classes.len() + c.attrs.len()) as u32;
    for p in &c.pseudos {
        s += match p {
            Pseudo::Not(inner) => compound_specificity(inner),
            _ => 100,
        };
    }
    if c.tag.is_some() {
        s += 1;
    }
    s
}

// ------------------------------------------------------------------ matching

/// The interactive state that `:hover`, `:focus` and `:active` depend on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    /// The element under the pointer (it and its ancestors match `:hover`).
    pub hover: Option<NodeId>,
    /// The focused element.
    pub focus: Option<NodeId>,
    /// The element being pressed.
    pub active: Option<NodeId>,
}

fn nth_matches(a: i32, b: i32, pos: i32) -> bool {
    if a == 0 {
        return pos == b;
    }
    let n = pos - b;
    n % a == 0 && n / a >= 0
}

fn compound_matches(dom: &Dom, node: NodeId, c: &Compound, st: &State) -> bool {
    if c.never {
        return false;
    }
    let Some(tag) = dom.tag(node) else {
        return false;
    };
    if c.tag.as_deref().is_some_and(|t| t != tag) {
        return false;
    }
    if c.id
        .as_deref()
        .is_some_and(|i| dom.element_id(node) != Some(i))
    {
        return false;
    }
    if !c.classes.is_empty() {
        let have = dom.classes(node);
        if !c.classes.iter().all(|k| have.contains(&k.as_str())) {
            return false;
        }
    }
    for (name, op) in &c.attrs {
        let Some(v) = dom.attr(node, name) else {
            return false;
        };
        let ok = match op {
            AttrOp::Exists => true,
            AttrOp::Equals(x) => v == x,
            AttrOp::Word(x) => v.split_whitespace().any(|w| w == x),
            AttrOp::Prefix(x) => !x.is_empty() && v.starts_with(x.as_str()),
            AttrOp::Suffix(x) => !x.is_empty() && v.ends_with(x.as_str()),
            AttrOp::Contains(x) => !x.is_empty() && v.contains(x.as_str()),
        };
        if !ok {
            return false;
        }
    }
    if c.pseudos.is_empty() {
        return true;
    }
    // Sibling positions are only worked out when a pseudo-class needs them.
    let (pos, total) = dom.sibling_index(node);
    let parent_children = dom
        .parent(node)
        .map(|p| dom.element_children(p))
        .unwrap_or_default();
    for p in &c.pseudos {
        let ok = match p {
            Pseudo::FirstChild => pos == 1,
            Pseudo::LastChild => pos == total,
            Pseudo::OnlyChild => total == 1,
            Pseudo::NthChild(a, b) => nth_matches(*a, *b, pos as i32),
            Pseudo::NthLastChild(a, b) => nth_matches(*a, *b, (total - pos + 1) as i32),
            Pseudo::FirstOfType => {
                parent_children.iter().find(|&&s| dom.tag(s) == Some(tag)) == Some(&node)
            }
            Pseudo::LastOfType => {
                parent_children
                    .iter()
                    .rev()
                    .find(|&&s| dom.tag(s) == Some(tag))
                    == Some(&node)
            }
            Pseudo::Root => tag == "html",
            Pseudo::Empty => dom
                .children(node)
                .iter()
                .all(|&k| dom.text(k).is_some_and(str::is_empty)),
            Pseudo::Not(inner) => !compound_matches(dom, node, inner, st),
            Pseudo::Hover => st.hover.is_some_and(|h| dom.ancestry(h).contains(&node)),
            Pseudo::Focus => st.focus == Some(node),
            Pseudo::Active => st.active == Some(node),
            Pseudo::Checked => dom.attr(node, "checked").is_some(),
            Pseudo::Disabled => dom.attr(node, "disabled").is_some(),
            Pseudo::Link => tag == "a" && dom.attr(node, "href").is_some(),
        };
        if !ok {
            return false;
        }
    }
    true
}

fn match_from(dom: &Dom, node: NodeId, parts: &[(Combinator, Compound)], st: &State) -> bool {
    let Some(((comb, last), before)) = parts.split_last() else {
        return true;
    };
    if !compound_matches(dom, node, last, st) {
        return false;
    }
    if before.is_empty() {
        return true;
    }
    match comb {
        Combinator::Child => dom
            .parent(node)
            .is_some_and(|p| match_from(dom, p, before, st)),
        Combinator::Descendant => {
            let mut cur = dom.parent(node);
            while let Some(p) = cur {
                if match_from(dom, p, before, st) {
                    return true;
                }
                cur = dom.parent(p);
            }
            false
        }
        Combinator::Adjacent | Combinator::Sibling => {
            let Some(par) = dom.parent(node) else {
                return false;
            };
            let sibs = dom.element_children(par);
            let Some(i) = sibs.iter().position(|&s| s == node) else {
                return false;
            };
            if *comb == Combinator::Adjacent {
                i > 0 && match_from(dom, sibs[i - 1], before, st)
            } else {
                sibs[..i].iter().any(|&s| match_from(dom, s, before, st))
            }
        }
    }
}

/// Whether `node` matches the selector.
#[must_use]
pub fn matches(dom: &Dom, node: NodeId, sel: &Selector, st: &State) -> bool {
    match_from(dom, node, &sel.parts, st)
}

// ------------------------------------------------------------------ values

/// A length as a sum of units, so `calc(100% - 2em)` stays exact until layout.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Len {
    /// Pixels.
    pub px: f32,
    /// `em` (the element's font size).
    pub em: f32,
    /// `rem` (the root font size).
    pub rem: f32,
    /// Fraction of the containing size (50% is 0.5).
    pub pct: f32,
    /// Percent of the viewport width, as a fraction.
    pub vw: f32,
    /// Percent of the viewport height, as a fraction.
    pub vh: f32,
}

impl Len {
    /// Plain pixels.
    #[must_use]
    pub fn px(v: f32) -> Self {
        Len {
            px: v,
            ..Len::default()
        }
    }

    fn scaled(self, k: f32) -> Self {
        Len {
            px: self.px * k,
            em: self.em * k,
            rem: self.rem * k,
            pct: self.pct * k,
            vw: self.vw * k,
            vh: self.vh * k,
        }
    }

    fn plus(self, o: Self) -> Self {
        Len {
            px: self.px + o.px,
            em: self.em + o.em,
            rem: self.rem + o.rem,
            pct: self.pct + o.pct,
            vw: self.vw + o.vw,
            vh: self.vh + o.vh,
        }
    }
}

/// A length, `auto`, or `none`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LenValue {
    /// `auto`
    Auto,
    /// `none`
    None,
    /// A length.
    Len(Len),
}

fn parse_number_unit(s: &str) -> Option<(f32, &str)> {
    let end = s
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0)))
        .map_or(s.len(), |(i, _)| i);
    if end == 0 {
        return None;
    }
    let n: f32 = s[..end].parse().ok()?;
    n.is_finite().then_some((n, &s[end..]))
}

fn unit_len(n: f32, unit: &str) -> Option<Len> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "px" => Len::px(n),
        "em" => Len {
            em: n,
            ..Len::default()
        },
        "rem" => Len {
            rem: n,
            ..Len::default()
        },
        "%" => Len {
            pct: n / 100.0,
            ..Len::default()
        },
        "vw" | "vi" => Len {
            vw: n / 100.0,
            ..Len::default()
        },
        "vh" | "vb" => Len {
            vh: n / 100.0,
            ..Len::default()
        },
        "vmin" => Len {
            vw: n / 100.0,
            ..Len::default()
        },
        "vmax" => Len {
            vh: n / 100.0,
            ..Len::default()
        },
        "pt" => Len::px(n * 96.0 / 72.0),
        "pc" => Len::px(n * 16.0),
        "in" => Len::px(n * 96.0),
        "cm" => Len::px(n * 96.0 / 2.54),
        "mm" => Len::px(n * 96.0 / 25.4),
        "ch" | "ex" => Len {
            em: n * 0.5,
            ..Len::default()
        },
        _ => return None,
    })
}

struct CalcParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl CalcParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    /// Returns `(length, is_plain_number)`.
    fn atom(&mut self, depth: usize) -> Option<(Len, bool)> {
        self.ws();
        if depth > 12 || self.i >= self.s.len() {
            return None;
        }
        if self.s[self.i] == b'(' {
            self.i += 1;
            let v = self.sum(depth + 1)?;
            self.ws();
            if self.s.get(self.i) != Some(&b')') {
                return None;
            }
            self.i += 1;
            return Some(v);
        }
        let rest = std::str::from_utf8(&self.s[self.i..]).ok()?;
        let (n, tail) = parse_number_unit(rest)?;
        let used_num = rest.len() - tail.len();
        self.i += used_num;
        let unit_end = tail
            .find(|c: char| !(c.is_ascii_alphabetic() || c == '%'))
            .unwrap_or(tail.len());
        let unit = &tail[..unit_end];
        self.i += unit_end;
        if unit.is_empty() {
            Some((Len::px(n), true))
        } else {
            Some((unit_len(n, unit)?, false))
        }
    }

    fn product(&mut self, depth: usize) -> Option<(Len, bool)> {
        let (mut v, mut plain) = self.atom(depth)?;
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'*') => {
                    self.i += 1;
                    let (r, rp) = self.atom(depth)?;
                    if plain {
                        v = r.scaled(v.px);
                        plain = rp;
                    } else if rp {
                        v = v.scaled(r.px);
                    } else {
                        return None;
                    }
                }
                Some(b'/') => {
                    self.i += 1;
                    let (r, rp) = self.atom(depth)?;
                    if !rp || r.px == 0.0 {
                        return None;
                    }
                    v = v.scaled(1.0 / r.px);
                }
                _ => return Some((v, plain)),
            }
        }
    }

    fn sum(&mut self, depth: usize) -> Option<(Len, bool)> {
        let (mut v, mut plain) = self.product(depth)?;
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'+') => {
                    self.i += 1;
                    let (r, rp) = self.product(depth)?;
                    v = v.plus(r);
                    plain &= rp;
                }
                Some(b'-') => {
                    self.i += 1;
                    let (r, rp) = self.product(depth)?;
                    v = v.plus(r.scaled(-1.0));
                    plain &= rp;
                }
                _ => return Some((v, plain)),
            }
        }
    }
}

/// Parses a length (`12px`, `1.5em`, `50%`, `calc(...)`, a bare `0`). `auto` and `none` are
/// handled by [`parse_len_value`].
#[must_use]
pub fn parse_len(s: &str) -> Option<Len> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(inner) = lower
        .strip_prefix("calc(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let mut p = CalcParser {
            s: inner.as_bytes(),
            i: 0,
        };
        let (v, plain) = p.sum(0)?;
        p.ws();
        return (p.i == p.s.len() && !plain).then_some(v);
    }
    let (n, unit) = parse_number_unit(&lower)?;
    if unit.is_empty() {
        return (n == 0.0).then(Len::default);
    }
    unit_len(n, unit)
}

/// Parses a length, `auto` or `none`.
#[must_use]
pub fn parse_len_value(s: &str) -> Option<LenValue> {
    match s.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(LenValue::Auto),
        "none" => Some(LenValue::None),
        _ => parse_len(s).map(LenValue::Len),
    }
}

const NAMED: [(&str, u32); 62] = [
    ("black", 0x000000),
    ("silver", 0xC0C0C0),
    ("gray", 0x808080),
    ("grey", 0x808080),
    ("white", 0xFFFFFF),
    ("maroon", 0x800000),
    ("red", 0xFF0000),
    ("purple", 0x800080),
    ("fuchsia", 0xFF00FF),
    ("magenta", 0xFF00FF),
    ("green", 0x008000),
    ("lime", 0x00FF00),
    ("olive", 0x808000),
    ("yellow", 0xFFFF00),
    ("navy", 0x000080),
    ("blue", 0x0000FF),
    ("teal", 0x008080),
    ("aqua", 0x00FFFF),
    ("cyan", 0x00FFFF),
    ("orange", 0xFFA500),
    ("pink", 0xFFC0CB),
    ("brown", 0xA52A2A),
    ("gold", 0xFFD700),
    ("indigo", 0x4B0082),
    ("violet", 0xEE82EE),
    ("tomato", 0xFF6347),
    ("coral", 0xFF7F50),
    ("salmon", 0xFA8072),
    ("crimson", 0xDC143C),
    ("khaki", 0xF0E68C),
    ("beige", 0xF5F5DC),
    ("ivory", 0xFFFFF0),
    ("lavender", 0xE6E6FA),
    ("turquoise", 0x40E0D0),
    ("skyblue", 0x87CEEB),
    ("royalblue", 0x4169E1),
    ("steelblue", 0x4682B4),
    ("dodgerblue", 0x1E90FF),
    ("lightblue", 0xADD8E6),
    ("darkblue", 0x00008B),
    ("lightgreen", 0x90EE90),
    ("darkgreen", 0x006400),
    ("forestgreen", 0x228B22),
    ("seagreen", 0x2E8B57),
    ("limegreen", 0x32CD32),
    ("lightgray", 0xD3D3D3),
    ("lightgrey", 0xD3D3D3),
    ("darkgray", 0xA9A9A9),
    ("darkgrey", 0xA9A9A9),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("gainsboro", 0xDCDCDC),
    ("whitesmoke", 0xF5F5F5),
    ("snow", 0xFFFAFA),
    ("darkred", 0x8B0000),
    ("firebrick", 0xB22222),
    ("orangered", 0xFF4500),
    ("darkorange", 0xFF8C00),
    ("chocolate", 0xD2691E),
    ("tan", 0xD2B48C),
    ("wheat", 0xF5DEB3),
    ("plum", 0xDDA0DD),
];

fn comp(v: f32, scale: f32) -> u32 {
    (v / scale * 255.0).round().clamp(0.0, 255.0) as u32
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u32, u32, u32) {
    let h = h.rem_euclid(360.0) / 360.0;
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u32
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

/// Parses a colour into `0xAARRGGBB`. `currentcolor` is not handled here.
#[must_use]
pub fn parse_color(s: &str) -> Option<u32> {
    let s = s.trim().to_ascii_lowercase();
    if s == "transparent" {
        return Some(0);
    }
    if let Some(h) = s.strip_prefix('#') {
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let d = |i: usize| u32::from_str_radix(&h[i..=i], 16).ok().map(|v| v * 17);
        let b = |i: usize| u32::from_str_radix(&h[i..i + 2], 16).ok();
        return match h.len() {
            3 => Some(0xFF00_0000 | (d(0)? << 16) | (d(1)? << 8) | d(2)?),
            4 => Some((d(3)? << 24) | (d(0)? << 16) | (d(1)? << 8) | d(2)?),
            6 => Some(0xFF00_0000 | (b(0)? << 16) | (b(2)? << 8) | b(4)?),
            8 => Some((b(6)? << 24) | (b(0)? << 16) | (b(2)? << 8) | b(4)?),
            _ => None,
        };
    }
    for fname in ["rgba", "rgb", "hsla", "hsl"] {
        if let Some(args) = s
            .strip_prefix(fname)
            .and_then(|r| r.trim_start().strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'))
        {
            let args = args.replace(['/', ','], " ");
            let v: Vec<&str> = args.split_whitespace().collect();
            if v.len() < 3 || v.len() > 4 {
                return None;
            }
            let num = |t: &str, pct_scale: f32| -> Option<f32> {
                match t.strip_suffix('%') {
                    Some(p) => p.parse::<f32>().ok().map(|x| x / 100.0 * pct_scale),
                    None => t.trim_end_matches("deg").parse().ok(),
                }
            };
            let alpha = match v.get(3) {
                Some(t) => num(t, 1.0)?.clamp(0.0, 1.0),
                None => 1.0,
            };
            let a = (alpha * 255.0).round() as u32;
            let (r, g, b) = if fname.starts_with("rgb") {
                (
                    comp(num(v[0], 255.0)?, 255.0),
                    comp(num(v[1], 255.0)?, 255.0),
                    comp(num(v[2], 255.0)?, 255.0),
                )
            } else {
                hsl_to_rgb(num(v[0], 360.0)?, num(v[1], 1.0)?, num(v[2], 1.0)?)
            };
            return Some((a << 24) | (r << 16) | (g << 8) | b);
        }
    }
    NAMED
        .iter()
        .find(|(n, _)| *n == s)
        .map(|(_, c)| 0xFF00_0000 | c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str) -> Selector {
        parse_selector(s).unwrap_or_else(|| panic!("no selector: {s}"))
    }

    fn found(dom: &Dom, s: &str) -> Vec<String> {
        let sel = sel(s);
        (0..dom.len())
            .filter(|&n| dom.tag(n).is_some() && matches(dom, n, &sel, &State::default()))
            .map(|n| {
                dom.element_id(n)
                    .map_or_else(|| dom.tag(n).unwrap().to_string(), str::to_string)
            })
            .collect()
    }

    #[test]
    fn declarations_are_read_with_important_and_odd_spacing() {
        let d = parse_declarations("color : red ; Margin:0 auto!important;;background: url(a;b.png) no-repeat; bad; :x; y:; --Brand: #fff ");
        let names: Vec<(&str, &str, bool)> = d
            .iter()
            .map(|x| (x.name.as_str(), x.value.as_str(), x.important))
            .collect();
        assert_eq!(
            names,
            vec![
                ("color", "red", false),
                ("margin", "0 auto", true),
                ("background", "url(a;b.png) no-repeat", false),
                ("--Brand", "#fff", false),
            ]
        );
    }

    #[test]
    fn rules_at_rules_comments_and_garbage_are_handled() {
        let css = "/* c */ @charset 'utf-8'; @import url(x.css);\n a, b > c { color: red } \
                   @font-face { font-family: X; src: url(x) } @keyframes k { from { a: b } to { a: c } } \
                   @media (max-width: 600px) { p { margin: 0 } } \
                   @media print { p { display: none } } \
                   }}} .x { top: 1px } ::before { content: 'x' } p::after { content: 'y' } .y { left: 2px";
        let s = Stylesheet::parse(css);
        let heads: Vec<usize> = s.rules.iter().map(|r| r.selectors.len()).collect();
        assert_eq!(s.rules[0].selectors.len(), 2);
        assert!(s.rules[0].media.is_none());
        let media: Vec<Option<Media>> = s.rules.iter().map(|r| r.media).collect();
        assert!(media
            .iter()
            .any(|m| m.is_some_and(|m| m.max_w == Some(600.0))));
        assert!(
            media.iter().any(|m| m.is_some_and(|m| m.never)),
            "print media never matches"
        );
        assert!(s.rules.iter().any(|r| r.decls[0].name == "top"));
        assert!(!heads.is_empty());
        assert!(s
            .rules
            .iter()
            .all(|r| r.decls.iter().all(|d| d.name != "content")
                || r.selectors
                    .iter()
                    .all(|x| x.parts.iter().any(|(_, c)| c.never))));
    }

    #[test]
    fn media_conditions_follow_the_viewport() {
        let s = Stylesheet::parse("@media (min-width: 500px) and (max-width: 900px) { a{b:c} } @media screen { d{e:f} } @media (min-width: 48em) { g{h:i} }");
        let m = |i: usize| s.rules[i].media.unwrap();
        assert!(
            m(0).matches(600.0, 400.0)
                && !m(0).matches(400.0, 400.0)
                && !m(0).matches(1000.0, 400.0)
        );
        assert!(m(1).matches(10.0, 10.0));
        assert!(m(2).matches(768.0, 10.0) && !m(2).matches(767.0, 10.0));
    }

    #[test]
    fn selectors_parse_with_specificity() {
        assert_eq!(sel("p").specificity, 1);
        assert_eq!(sel(".a").specificity, 100);
        assert_eq!(sel("#a").specificity, 10_000);
        assert_eq!(
            sel("div.a#b[x]:first-child").specificity,
            10_000 + 100 + 100 + 100 + 1
        );
        assert_eq!(sel("ul li a").specificity, 3);
        assert_eq!(sel(":not(.a)").specificity, 100);
        for bad in [
            "",
            ">",
            "a >",
            "a..b",
            "#",
            ".",
            "a[",
            "a:nth-child(x)",
            "1a",
        ] {
            assert!(parse_selector(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn simple_and_attribute_selectors_match() {
        let d = Dom::parse(
            r#"<div id="a" class="x y"><p id="b" class="y" data-k="hello world" lang="en-US">t</p><a id="c" href="/x.png">l</a><input id="d" disabled></div>"#,
        );
        assert_eq!(found(&d, "div"), vec!["a"]);
        assert_eq!(found(&d, ".y"), vec!["a", "b"]);
        assert_eq!(found(&d, ".x.y"), vec!["a"]);
        assert_eq!(found(&d, "#b"), vec!["b"]);
        assert_eq!(found(&d, "p#b.y"), vec!["b"]);
        assert_eq!(found(&d, "[data-k]"), vec!["b"]);
        assert_eq!(found(&d, "[data-k='hello world']"), vec!["b"]);
        assert_eq!(found(&d, "[data-k~=world]"), vec!["b"]);
        assert_eq!(found(&d, "[data-k^=hel]"), vec!["b"]);
        assert_eq!(found(&d, "[href$='.png']"), vec!["c"]);
        assert_eq!(found(&d, "[data-k*=o\\ w]").len(), 0);
        assert_eq!(found(&d, "[data-k*=lo]"), vec!["b"]);
        assert_eq!(found(&d, "a:link"), vec!["c"]);
        assert_eq!(found(&d, "input:disabled"), vec!["d"]);
        assert_eq!(found(&d, ".nope"), Vec::<String>::new());
        assert!(found(&d, "*").len() >= 6);
    }

    #[test]
    fn combinators_match_correctly() {
        let d = Dom::parse(
            r#"<div id="a"><ul id="u"><li id="l1">1</li><li id="l2">2</li><li id="l3"><b id="bb">x</b></li></ul><p id="p">p</p></div><span id="s">s</span>"#,
        );
        assert_eq!(found(&d, "div li"), vec!["l1", "l2", "l3"]);
        assert_eq!(found(&d, "div > li"), Vec::<String>::new());
        assert_eq!(found(&d, "ul > li"), vec!["l1", "l2", "l3"]);
        assert_eq!(found(&d, "div > ul > li > b"), vec!["bb"]);
        assert_eq!(found(&d, "li + li"), vec!["l2", "l3"]);
        assert_eq!(found(&d, "li + li + li"), vec!["l3"]);
        assert_eq!(found(&d, "li ~ li"), vec!["l2", "l3"]);
        assert_eq!(found(&d, "ul + p"), vec!["p"]);
        assert_eq!(found(&d, "div span"), Vec::<String>::new());
        assert_eq!(
            found(&d, "div+span"),
            vec!["s"],
            "the span immediately follows the div"
        );
        assert_eq!(found(&d, "body > div ~ span"), vec!["s"]);
        assert_eq!(found(&d, "div   >   ul"), vec!["u"]);
    }

    #[test]
    fn structural_pseudo_classes_match() {
        let d = Dom::parse(
            r#"<ul><li id="a">1</li><li id="b">2</li><li id="c">3</li><li id="d">4</li><li id="e">5</li></ul><div id="x"></div><p id="only">o</p>"#,
        );
        assert_eq!(found(&d, "li:first-child"), vec!["a"]);
        assert_eq!(found(&d, "li:last-child"), vec!["e"]);
        assert_eq!(found(&d, "li:nth-child(2)"), vec!["b"]);
        assert_eq!(found(&d, "li:nth-child(odd)"), vec!["a", "c", "e"]);
        assert_eq!(found(&d, "li:nth-child(even)"), vec!["b", "d"]);
        assert_eq!(found(&d, "li:nth-child(2n+1)"), vec!["a", "c", "e"]);
        assert_eq!(found(&d, "li:nth-child(3n)"), vec!["c"]);
        assert_eq!(found(&d, "li:nth-child(n+4)"), vec!["d", "e"]);
        assert_eq!(found(&d, "li:nth-child(-n+2)"), vec!["a", "b"]);
        assert_eq!(found(&d, "li:nth-last-child(1)"), vec!["e"]);
        assert_eq!(
            found(&d, "li:not(:first-child):not(:last-child)"),
            vec!["b", "c", "d"]
        );
        assert_eq!(found(&d, "li:not(.z)").len(), 5);
        assert_eq!(found(&d, "div:empty"), vec!["x"]);
        assert_eq!(found(&d, "html:root"), vec!["html"]);
        assert_eq!(found(&d, "li:first-of-type"), vec!["a"]);
        assert_eq!(found(&d, "li:last-of-type"), vec!["e"]);
        assert_eq!(found(&d, "ul:only-child"), Vec::<String>::new());
    }

    #[test]
    fn pseudo_elements_and_unknown_pseudos_never_match_and_hover_follows_the_state() {
        let d = Dom::parse(r#"<div id="a"><p id="b">t</p></div>"#);
        assert!(found(&d, "p::before").is_empty());
        assert!(found(&d, "p:before").is_empty());
        assert!(found(&d, "p:frobnicate").is_empty());
        let hover = sel("div:hover");
        let (a, b) = (d.by_id("a").unwrap(), d.by_id("b").unwrap());
        assert!(!matches(&d, a, &hover, &State::default()));
        assert!(
            matches(
                &d,
                a,
                &hover,
                &State {
                    hover: Some(b),
                    ..State::default()
                }
            ),
            "an ancestor of the hovered element is hovered too"
        );
        assert!(matches(
            &d,
            b,
            &sel("p:focus"),
            &State {
                focus: Some(b),
                ..State::default()
            }
        ));
    }

    #[test]
    fn colours_parse_in_every_syntax() {
        for (s, c) in [
            ("red", 0xFFFF0000),
            ("RED", 0xFFFF0000),
            ("#f00", 0xFFFF0000),
            ("#ff0000", 0xFFFF0000),
            ("#ff000080", 0x80FF0000),
            ("#f008", 0x88FF0000),
            ("rgb(255, 0, 0)", 0xFFFF0000),
            ("rgb(255 0 0)", 0xFFFF0000),
            ("rgba(0,0,255,0.5)", 0x800000FF),
            ("rgb(0 0 255 / 50%)", 0x800000FF),
            ("rgb(100%, 0%, 0%)", 0xFFFF0000),
            ("hsl(120, 100%, 50%)", 0xFF00FF00),
            ("hsla(240, 100%, 50%, 1)", 0xFF0000FF),
            ("transparent", 0),
            ("rebeccapurple", 0),
        ] {
            let got = parse_color(s);
            if s == "rebeccapurple" {
                assert_eq!(got, None);
            } else {
                assert_eq!(got, Some(c), "{s}");
            }
        }
        for bad in [
            "",
            "#",
            "#12",
            "#12345",
            "#gg0000",
            "rgb(1,2)",
            "rgb(a,b,c)",
            "hsl()",
            "notacolour",
            "rgb(1,2,3,4,5)",
        ] {
            assert_eq!(parse_color(bad), None, "{bad:?}");
        }
        assert_eq!(
            parse_color("rgb(300, -5, 0)"),
            Some(0xFFFF0000),
            "values are clamped"
        );
    }

    #[test]
    fn lengths_and_calc_parse_exactly() {
        assert_eq!(parse_len("12px"), Some(Len::px(12.0)));
        assert_eq!(
            parse_len("1.5em"),
            Some(Len {
                em: 1.5,
                ..Len::default()
            })
        );
        assert_eq!(
            parse_len("50%"),
            Some(Len {
                pct: 0.5,
                ..Len::default()
            })
        );
        assert_eq!(parse_len("0"), Some(Len::default()));
        assert_eq!(parse_len("12"), None, "a bare number needs a unit");
        assert_eq!(parse_len("-4px"), Some(Len::px(-4.0)));
        assert_eq!(
            parse_len("10vw"),
            Some(Len {
                vw: 0.1,
                ..Len::default()
            })
        );
        assert_eq!(parse_len("72pt"), Some(Len::px(96.0)));
        assert_eq!(parse_len("1in"), Some(Len::px(96.0)));
        assert_eq!(parse_len("12foo"), None);
        assert_eq!(parse_len("px"), None);
        assert_eq!(
            parse_len("calc(100% - 20px)"),
            Some(Len {
                pct: 1.0,
                px: -20.0,
                ..Len::default()
            })
        );
        assert_eq!(
            parse_len("calc(2 * (10px + 1em))"),
            Some(Len {
                px: 20.0,
                em: 2.0,
                ..Len::default()
            })
        );
        assert_eq!(
            parse_len("calc(100% / 4)"),
            Some(Len {
                pct: 0.25,
                ..Len::default()
            })
        );
        assert_eq!(parse_len("calc(10px*3)"), Some(Len::px(30.0)));
        assert_eq!(parse_len("calc(10px / 0)"), None);
        assert_eq!(parse_len("calc(10px * 2px)"), None);
        assert_eq!(parse_len("calc(5)"), None, "a plain number is not a length");
        assert_eq!(parse_len("calc(10px +)"), None);
        assert_eq!(
            parse_len(&format!("calc({}1px{})", "(".repeat(100), ")".repeat(100))),
            None,
            "nesting is bounded"
        );
        assert_eq!(parse_len_value("auto"), Some(LenValue::Auto));
        assert_eq!(parse_len_value("none"), Some(LenValue::None));
        assert_eq!(parse_len_value("3px"), Some(LenValue::Len(Len::px(3.0))));
    }

    #[test]
    fn hostile_css_is_bounded_and_never_panics() {
        for s in [
            "",
            "{",
            "}",
            "{{{{",
            "a{",
            "a{b",
            "a{b:",
            "@media",
            "@media {",
            "@",
            "a,{}",
            "a{;;;}",
            "\u{0}",
            "/*",
            "a{b:c!important!important}",
            "a{b:\"unterminated}",
        ] {
            let _ = Stylesheet::parse(s);
            let _ = parse_declarations(s);
        }
        let many = "a{b:c}".repeat(MAX_RULES + 100);
        assert_eq!(Stylesheet::parse(&many).rules.len(), MAX_RULES);
        let deep = format!("{}a{{b:c}}{}", "@media screen{".repeat(50), "}".repeat(50));
        let _ = Stylesheet::parse(&deep);
    }
}
