//! A small DOM: a tree built from the tolerant tokenizer.
//!
//! The builder follows the rules that matter for real pages: void elements (`<br>`, `<img>`),
//! implied end tags (`<p>` closed by the next block, `<li>` by the next `<li>`, table cells and
//! rows), end tags that close everything open inside them, and a guaranteed `html` > `head`,
//! `body` shape. Everything is bounded (node count and depth), so hostile markup cannot make the
//! tree or later recursion unbounded. Text is never executed or interpreted here.

use crate::html::{tokenize, Token};

/// A node's number in the tree.
pub type NodeId = usize;

/// Most nodes in one document.
pub const MAX_NODES: usize = 100_000;
/// Deepest element nesting kept; deeper elements are flattened into their parent.
pub const MAX_DEPTH: usize = 200;

/// What a node is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The root.
    Document,
    /// An element with its attributes (names lowercased).
    Element {
        /// Lowercase tag name.
        tag: String,
        /// Attributes in source order.
        attrs: Vec<(String, String)>,
    },
    /// Text (entities decoded).
    Text(String),
}

/// One node and its place in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// What it is.
    pub kind: Kind,
    /// Its parent (`None` for the root).
    pub parent: Option<NodeId>,
    /// Its children, in order.
    pub children: Vec<NodeId>,
}

/// A document.
#[derive(Debug, Clone)]
pub struct Dom {
    nodes: Vec<Node>,
}

const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

const HEAD_ONLY: [&str; 5] = ["title", "meta", "link", "style", "base"];

/// Elements that close an open `<p>`.
const CLOSES_P: [&str; 26] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
];

fn closes_p(tag: &str) -> bool {
    CLOSES_P.contains(&tag) || matches!(tag, "table" | "ul")
}

impl Dom {
    /// Builds a document from HTML text.
    #[must_use]
    pub fn parse(html: &str) -> Self {
        let mut d = Dom {
            nodes: vec![Node {
                kind: Kind::Document,
                parent: None,
                children: Vec::new(),
            }],
        };
        let mut open: Vec<NodeId> = vec![0];
        for token in tokenize(html) {
            match token {
                Token::Start { tag, attrs, .. } => {
                    d.implied_ends(&mut open, &tag);
                    let Some(id) = d.add(
                        *open.last().unwrap_or(&0),
                        Kind::Element {
                            tag: tag.clone(),
                            attrs,
                        },
                    ) else {
                        break;
                    };
                    if !VOID.contains(&tag.as_str()) && open.len() < MAX_DEPTH {
                        open.push(id);
                    }
                }
                Token::End(tag) => {
                    if let Some(pos) = open.iter().rposition(|&n| d.tag(n) == Some(tag.as_str())) {
                        if pos > 0 {
                            open.truncate(pos);
                        }
                    }
                }
                Token::Text(s) => {
                    let parent = *open.last().unwrap_or(&0);
                    let merged = d.nodes[parent]
                        .children
                        .last()
                        .copied()
                        .filter(|&c| matches!(d.nodes[c].kind, Kind::Text(_)));
                    if let Some(c) = merged {
                        if let Kind::Text(t) = &mut d.nodes[c].kind {
                            t.push_str(&s);
                        }
                    } else if d.add(parent, Kind::Text(s)).is_none() {
                        break;
                    }
                }
            }
        }
        d.normalise();
        d
    }

    fn add(&mut self, parent: NodeId, kind: Kind) -> Option<NodeId> {
        if self.nodes.len() >= MAX_NODES {
            return None;
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.nodes[parent].children.push(id);
        Some(id)
    }

    /// Closes elements that a new start tag implicitly ends.
    fn implied_ends(&self, open: &mut Vec<NodeId>, tag: &str) {
        let top_tag =
            |open: &Vec<NodeId>| open.last().and_then(|&n| self.tag(n)).map(str::to_string);
        loop {
            let Some(top) = top_tag(open) else { return };
            let close = match (top.as_str(), tag) {
                ("p", t) if closes_p(t) => true,
                ("li", "li") | ("dt" | "dd", "dt" | "dd") | ("option", "option" | "optgroup") => {
                    true
                }
                ("td" | "th", "td" | "th" | "tr" | "tbody" | "thead" | "tfoot") => true,
                ("tr", "tr" | "tbody" | "thead" | "tfoot") => true,
                ("thead" | "tbody" | "tfoot", "tbody" | "tfoot" | "thead") => true,
                _ => false,
            };
            if !close {
                return;
            }
            open.pop();
        }
    }

    /// Makes sure the document is `html` > (`head`, `body`) with the content in `body`.
    fn normalise(&mut self) {
        let root_kids = self.nodes[0].children.clone();
        let html = root_kids
            .iter()
            .copied()
            .find(|&c| self.tag(c) == Some("html"));
        let html = match html {
            Some(h) => {
                // Anything else at the top level (text after `</html>`) goes into the html element.
                for c in root_kids.iter().copied().filter(|&c| c != h) {
                    self.nodes[c].parent = Some(h);
                    self.nodes[h].children.push(c);
                }
                self.nodes[0].children = vec![h];
                h
            }
            None => {
                let h = self.nodes.len();
                self.nodes.push(Node {
                    kind: Kind::Element {
                        tag: "html".into(),
                        attrs: Vec::new(),
                    },
                    parent: Some(0),
                    children: root_kids.clone(),
                });
                for &c in &root_kids {
                    self.nodes[c].parent = Some(h);
                }
                self.nodes[0].children = vec![h];
                h
            }
        };
        let kids = self.nodes[html].children.clone();
        let mut head = kids.iter().copied().find(|&c| self.tag(c) == Some("head"));
        let mut body = kids.iter().copied().find(|&c| self.tag(c) == Some("body"));
        let mut new_head_kids: Vec<NodeId> = Vec::new();
        let mut new_body_kids: Vec<NodeId> = Vec::new();
        let mut seen_body = false;
        for &c in &kids {
            if Some(c) == head {
                continue;
            }
            if Some(c) == body {
                seen_body = true;
                continue;
            }
            let is_head_tag = self.tag(c).is_some_and(|t| HEAD_ONLY.contains(&t));
            let blank_text = matches!(&self.nodes[c].kind, Kind::Text(t) if t.trim().is_empty());
            if is_head_tag && !seen_body {
                new_head_kids.push(c);
            } else if !blank_text || seen_body {
                new_body_kids.push(c);
            }
        }
        let mk = |me: &mut Self, tag: &str| -> NodeId {
            let id = me.nodes.len();
            me.nodes.push(Node {
                kind: Kind::Element {
                    tag: tag.into(),
                    attrs: Vec::new(),
                },
                parent: Some(html),
                children: Vec::new(),
            });
            id
        };
        let head_id = head.take().unwrap_or_else(|| mk(self, "head"));
        let body_id = body.take().unwrap_or_else(|| mk(self, "body"));
        for &c in &new_head_kids {
            self.nodes[c].parent = Some(head_id);
        }
        let mut hk = self.nodes[head_id].children.clone();
        hk.extend(new_head_kids);
        self.nodes[head_id].children = hk;
        for &c in &new_body_kids {
            self.nodes[c].parent = Some(body_id);
        }
        let mut bk = self.nodes[body_id].children.clone();
        bk.extend(new_body_kids);
        self.nodes[body_id].children = bk;
        self.nodes[html].children = vec![head_id, body_id];
    }

    /// The root node (the document).
    #[must_use]
    pub fn root(&self) -> NodeId {
        0
    }

    /// Number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// True when the document has only its root (never the case after parsing).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// A node.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// The tag of an element.
    #[must_use]
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        match &self.nodes.get(id)?.kind {
            Kind::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    /// An attribute of an element.
    #[must_use]
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        match &self.nodes.get(id)?.kind {
            Kind::Element { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    /// The `id` attribute.
    #[must_use]
    pub fn element_id(&self, id: NodeId) -> Option<&str> {
        self.attr(id, "id")
    }

    /// The classes of an element.
    #[must_use]
    pub fn classes(&self, id: NodeId) -> Vec<&str> {
        self.attr(id, "class")
            .map(|c| c.split_whitespace().collect())
            .unwrap_or_default()
    }

    /// The text of a text node.
    #[must_use]
    pub fn text(&self, id: NodeId) -> Option<&str> {
        match &self.nodes.get(id)?.kind {
            Kind::Text(t) => Some(t),
            _ => None,
        }
    }

    /// The children of a node.
    #[must_use]
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.nodes.get(id).map_or(&[], |n| &n.children)
    }

    /// The parent of a node.
    #[must_use]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id)?.parent
    }

    /// The element children of a node.
    #[must_use]
    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.children(id)
            .iter()
            .copied()
            .filter(|&c| self.tag(c).is_some())
            .collect()
    }

    /// All text under a node, concatenated.
    #[must_use]
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(t) = self.text(n) {
                out.push_str(t);
            }
            stack.extend(self.children(n).iter().rev());
        }
        out
    }

    /// Every element with this tag, in document order.
    #[must_use]
    pub fn find_all(&self, tag: &str) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![0];
        while let Some(n) = stack.pop() {
            if self.tag(n) == Some(tag) {
                out.push(n);
            }
            stack.extend(self.children(n).iter().rev());
        }
        out
    }

    /// The element with this `id` attribute, if any.
    #[must_use]
    pub fn by_id(&self, id: &str) -> Option<NodeId> {
        (0..self.nodes.len()).find(|&n| self.element_id(n) == Some(id))
    }

    /// The `body` element.
    #[must_use]
    pub fn body(&self) -> Option<NodeId> {
        self.find_all("body").first().copied()
    }

    /// The document title.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        self.find_all("title")
            .first()
            .map(|&t| self.text_content(t).trim().to_string())
    }

    /// The text of every `<style>` element, in order.
    #[must_use]
    pub fn style_sheets(&self) -> Vec<String> {
        self.find_all("style")
            .into_iter()
            .map(|s| self.text_content(s))
            .collect()
    }

    /// The text of every inline `<script>` (no `src`), in order.
    #[must_use]
    pub fn inline_scripts(&self) -> Vec<String> {
        self.find_all("script")
            .into_iter()
            .filter(|&s| self.attr(s, "src").is_none())
            .map(|s| self.text_content(s))
            .collect()
    }

    /// The nodes on the way from the root to `id`, root first.
    #[must_use]
    pub fn ancestry(&self, id: NodeId) -> Vec<NodeId> {
        let mut v = Vec::new();
        let mut cur = Some(id);
        while let Some(n) = cur {
            v.push(n);
            cur = self.parent(n);
        }
        v.reverse();
        v
    }

    /// The position of an element among its parent's element children (1-based) and how many
    /// element siblings it has.
    #[must_use]
    pub fn sibling_index(&self, id: NodeId) -> (usize, usize) {
        let Some(p) = self.parent(id) else {
            return (1, 1);
        };
        let sibs = self.element_children(p);
        (
            sibs.iter().position(|&s| s == id).map_or(1, |i| i + 1),
            sibs.len(),
        )
    }
}

/// Changing a document (what scripts do).
impl Dom {
    fn push_node(&mut self, kind: Kind) -> Option<NodeId> {
        if self.nodes.len() >= MAX_NODES {
            return None;
        }
        self.nodes.push(Node {
            kind,
            parent: None,
            children: Vec::new(),
        });
        Some(self.nodes.len() - 1)
    }

    /// Makes a detached element (`None` when the document is full).
    pub fn create_element(&mut self, tag: &str) -> Option<NodeId> {
        let tag: String = tag
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        if tag.is_empty() {
            return None;
        }
        self.push_node(Kind::Element {
            tag,
            attrs: Vec::new(),
        })
    }

    /// Makes a detached text node.
    pub fn create_text(&mut self, text: &str) -> Option<NodeId> {
        self.push_node(Kind::Text(text.to_string()))
    }

    /// Takes a node out of its parent.
    pub fn detach(&mut self, id: NodeId) {
        if let Some(p) = self.nodes.get(id).and_then(|n| n.parent) {
            self.nodes[p].children.retain(|&c| c != id);
            self.nodes[id].parent = None;
        }
    }

    /// Whether `a` is `b` or lies inside `b`.
    #[must_use]
    pub fn is_inside(&self, a: NodeId, b: NodeId) -> bool {
        let mut cur = Some(a);
        let mut guard = 0;
        while let Some(n) = cur {
            if n == b {
                return true;
            }
            guard += 1;
            if guard > MAX_NODES {
                return false;
            }
            cur = self.parent(n);
        }
        false
    }

    /// Puts `child` last under `parent` (moving it if it is elsewhere). `false` if that would make
    /// a cycle or either node is unknown.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        self.insert_before(parent, child, None)
    }

    /// Puts `child` under `parent` before `reference` (or last when `None`).
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        reference: Option<NodeId>,
    ) -> bool {
        if parent >= self.nodes.len()
            || child >= self.nodes.len()
            || self.is_inside(parent, child)
            || matches!(self.nodes[parent].kind, Kind::Text(_))
            || matches!(self.nodes[child].kind, Kind::Document)
        {
            return false;
        }
        if let Some(r) = reference {
            if self.nodes.get(r).and_then(|n| n.parent) != Some(parent) {
                return false;
            }
        }
        self.detach(child);
        let at = reference
            .and_then(|r| self.nodes[parent].children.iter().position(|&c| c == r))
            .unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(at, child);
        self.nodes[child].parent = Some(parent);
        true
    }

    /// Sets an attribute (name lowercased).
    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        let name = name.to_ascii_lowercase();
        if let Some(Node {
            kind: Kind::Element { attrs, .. },
            ..
        }) = self.nodes.get_mut(id)
        {
            if let Some(a) = attrs.iter_mut().find(|(n, _)| *n == name) {
                a.1 = value.to_string();
            } else if attrs.len() < 64 {
                attrs.push((name, value.to_string()));
            }
        }
    }

    /// Removes an attribute.
    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        let name = name.to_ascii_lowercase();
        if let Some(Node {
            kind: Kind::Element { attrs, .. },
            ..
        }) = self.nodes.get_mut(id)
        {
            attrs.retain(|(n, _)| *n != name);
        }
    }

    /// Replaces a node's children with one text node (or sets the text of a text node).
    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        match self.nodes.get_mut(id).map(|n| &mut n.kind) {
            Some(Kind::Text(t)) => *t = text.to_string(),
            Some(_) => {
                self.clear_children(id);
                if !text.is_empty() {
                    if let Some(t) = self.create_text(text) {
                        self.append_child(id, t);
                    }
                }
            }
            None => {}
        }
    }

    /// Removes all of a node's children.
    pub fn clear_children(&mut self, id: NodeId) {
        let kids = self.children(id).to_vec();
        for k in kids {
            self.detach(k);
        }
    }

    /// Copies `src` and everything below it from `from` into this document, detached.
    pub fn import(&mut self, from: &Dom, src: NodeId) -> Option<NodeId> {
        let node = from.nodes.get(src)?;
        let copy = match &node.kind {
            Kind::Document => return None,
            k => self.push_node(k.clone())?,
        };
        for &c in &node.children {
            if let Some(cc) = self.import(from, c) {
                self.append_child(copy, cc);
            }
        }
        Some(copy)
    }

    /// Copies a node of this document, with its descendants when `deep`.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> Option<NodeId> {
        let snapshot = self.clone();
        let copy = self.import(&snapshot, id)?;
        if !deep {
            self.clear_children(copy);
        }
        Some(copy)
    }

    /// Replaces a node's children with the nodes parsed from `html`.
    pub fn set_inner_html(&mut self, id: NodeId, html: &str) {
        if matches!(
            self.nodes.get(id).map(|n| &n.kind),
            None | Some(Kind::Text(_))
        ) {
            return;
        }
        let frag = Dom::parse(&format!("<body>{html}</body>"));
        self.clear_children(id);
        if let Some(body) = frag.body() {
            for &c in frag.children(body) {
                if let Some(copy) = self.import(&frag, c) {
                    self.append_child(id, copy);
                }
            }
        }
    }

    /// The HTML of a node's children.
    #[must_use]
    pub fn inner_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        for &c in self.children(id) {
            self.write_html(c, &mut out, 0);
        }
        out
    }

    fn write_html(&self, id: NodeId, out: &mut String, depth: usize) {
        if depth > MAX_DEPTH + 10 || out.len() > 8_000_000 {
            return;
        }
        let Some(n) = self.nodes.get(id) else { return };
        match &n.kind {
            Kind::Text(t) => {
                let raw = n
                    .parent
                    .and_then(|p| self.tag(p))
                    .is_some_and(|t| matches!(t, "script" | "style"));
                if raw {
                    out.push_str(t);
                } else {
                    out.push_str(
                        &t.replace('&', "&amp;")
                            .replace('<', "&lt;")
                            .replace('>', "&gt;"),
                    );
                }
            }
            Kind::Element { tag, attrs } => {
                out.push('<');
                out.push_str(tag);
                for (k, v) in attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&v.replace('&', "&amp;").replace('"', "&quot;"));
                    out.push('"');
                }
                out.push('>');
                if !VOID.contains(&tag.as_str()) {
                    for &c in &n.children {
                        self.write_html(c, out, depth + 1);
                    }
                    out.push_str("</");
                    out.push_str(tag);
                    out.push('>');
                }
            }
            Kind::Document => {}
        }
    }

    /// Elements below `id` (not `id` itself) in document order.
    #[must_use]
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.children(id).iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if out.len() >= MAX_NODES {
                break;
            }
            if matches!(self.nodes[n].kind, Kind::Element { .. }) {
                out.push(n);
            }
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(d: &Dom, id: NodeId) -> Vec<String> {
        d.element_children(id)
            .iter()
            .map(|&c| d.tag(c).unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_fragment_gets_html_head_and_body() {
        let d = Dom::parse("<p>Hello</p>");
        let html = d.children(0)[0];
        assert_eq!(d.tag(html), Some("html"));
        assert_eq!(tags(&d, html), vec!["head", "body"]);
        let body = d.body().unwrap();
        assert_eq!(tags(&d, body), vec!["p"]);
        assert_eq!(d.text_content(body), "Hello");
    }

    #[test]
    fn a_full_document_keeps_its_shape_and_head_content() {
        let d = Dom::parse("<!doctype html><html><head><title> Hi </title><style>p{color:red}</style></head><body><h1>T</h1></body></html>");
        assert_eq!(d.title().as_deref(), Some("Hi"));
        assert_eq!(d.style_sheets(), vec!["p{color:red}"]);
        assert_eq!(tags(&d, d.body().unwrap()), vec!["h1"]);
        assert_eq!(d.find_all("body").len(), 1);
        assert_eq!(d.find_all("head").len(), 1);
    }

    #[test]
    fn head_tags_without_a_head_go_to_the_head() {
        let d = Dom::parse("<title>T</title><meta charset=utf-8><style>a{}</style><div>x</div>");
        let head = d.find_all("head")[0];
        assert_eq!(tags(&d, head), vec!["title", "meta", "style"]);
        assert_eq!(tags(&d, d.body().unwrap()), vec!["div"]);
    }

    #[test]
    fn void_elements_and_self_closing_tags_have_no_children() {
        let d = Dom::parse("<p>a<br>b<img src=x>c<input value=1>d</p>");
        let p = d.find_all("p")[0];
        assert_eq!(tags(&d, p), vec!["br", "img", "input"]);
        assert_eq!(d.text_content(p), "abcd");
        assert!(d.children(d.find_all("br")[0]).is_empty());
        // `<div/>` is not self-closing in HTML.
        let d = Dom::parse("<div/>text");
        assert_eq!(d.text_content(d.find_all("div")[0]), "text");
    }

    #[test]
    fn implied_end_tags_close_paragraphs_items_and_cells() {
        let d = Dom::parse("<p>one<p>two<div>three</div>");
        let body = d.body().unwrap();
        assert_eq!(tags(&d, body), vec!["p", "p", "div"]);
        let d = Dom::parse("<ul><li>a<li>b<li>c</ul>");
        assert_eq!(tags(&d, d.find_all("ul")[0]), vec!["li", "li", "li"]);
        let d = Dom::parse("<table><tr><td>1<td>2<tr><td>3</table>");
        let t = d.find_all("table")[0];
        assert_eq!(tags(&d, t), vec!["tr", "tr"]);
        assert_eq!(tags(&d, d.find_all("tr")[0]), vec!["td", "td"]);
        let d = Dom::parse("<dl><dt>a<dd>b<dt>c</dl>");
        assert_eq!(tags(&d, d.find_all("dl")[0]), vec!["dt", "dd", "dt"]);
    }

    #[test]
    fn an_end_tag_closes_everything_open_inside_it_and_stray_ones_are_ignored() {
        let d = Dom::parse("<div><span><b>bold</div>after</span></b>");
        let body = d.body().unwrap();
        assert_eq!(tags(&d, body), vec!["div"]);
        assert_eq!(d.text_content(body), "boldafter");
        let d = Dom::parse("</p></div>text</span>");
        assert_eq!(d.text_content(d.body().unwrap()), "text");
        // Nested lists are not closed by an inner </li>.
        let d = Dom::parse("<ul><li>a<ul><li>b</li></ul></li><li>c</li></ul>");
        assert_eq!(d.find_all("li").len(), 3);
        assert_eq!(tags(&d, d.find_all("ul")[0]), vec!["li", "li"]);
    }

    #[test]
    fn attributes_classes_ids_and_lookup_work() {
        let d =
            Dom::parse(r#"<div id="main" class="a  b c" data-x='1'><p class=intro>t</p></div>"#);
        let div = d.by_id("main").unwrap();
        assert_eq!(d.classes(div), vec!["a", "b", "c"]);
        assert_eq!(d.attr(div, "data-x"), Some("1"));
        assert_eq!(d.attr(div, "nope"), None);
        let p = d.find_all("p")[0];
        assert_eq!(d.classes(p), vec!["intro"]);
        assert_eq!(d.parent(p), Some(div));
        assert_eq!(d.sibling_index(p), (1, 1));
        assert!(d.by_id("zzz").is_none());
        assert_eq!(d.ancestry(p).len(), 5, "document, html, body, div, p");
    }

    #[test]
    fn text_is_merged_whitespace_only_text_before_the_body_is_dropped_and_scripts_are_kept_raw() {
        let d = Dom::parse("  \n<html>\n<body>a&amp;b <b>c</b>\n</body>\n</html>\n");
        assert!(d.text_content(d.body().unwrap()).starts_with("a&b c"));
        assert_eq!(d.find_all("b").len(), 1);
        let d = Dom::parse("<script>if (a<b) { x(); }</script><p>t</p>");
        assert_eq!(d.inline_scripts(), vec!["if (a<b) { x(); }"]);
        assert_eq!(
            Dom::parse("<script src=x.js></script>")
                .inline_scripts()
                .len(),
            0
        );
    }

    #[test]
    fn sibling_positions_count_elements_only() {
        let d = Dom::parse("<ul>text<li>a</li> <li>b</li><li>c</li></ul>");
        let lis = d.find_all("li");
        assert_eq!(d.sibling_index(lis[0]), (1, 3));
        assert_eq!(d.sibling_index(lis[2]), (3, 3));
    }

    #[test]
    fn hostile_markup_is_bounded() {
        let deep = "<div>".repeat(10_000);
        let d = Dom::parse(&deep);
        assert!(d.ancestry(d.find_all("div").last().copied().unwrap()).len() <= MAX_DEPTH + 4);
        let many = "<i>x".repeat(MAX_NODES);
        let d = Dom::parse(&many);
        assert!(d.len() <= MAX_NODES + 4, "{}", d.len());
        for s in [
            "",
            "<",
            "<<<<",
            "<a",
            "</",
            "<!--",
            "<p <b>",
            "&",
            "\u{0}\u{1}",
        ] {
            let d = Dom::parse(s);
            assert!(d.body().is_some(), "{s:?}");
        }
    }

    #[test]
    fn scripts_can_change_the_tree() {
        let mut d = Dom::parse("<ul id=l><li>a</li></ul>");
        let l = d.by_id("l").unwrap();
        let li = d.create_element("LI").unwrap();
        d.set_text_content(li, "b <&>");
        assert!(d.append_child(l, li));
        assert_eq!(d.inner_html(l), "<li>a</li><li>b &lt;&amp;&gt;</li>");
        // Moving a node takes it out of its old place.
        let first = d.element_children(l)[0];
        assert!(d.append_child(l, first));
        assert_eq!(d.inner_html(l), "<li>b &lt;&amp;&gt;</li><li>a</li>");
        // Cycles and bad targets are refused.
        assert!(!d.append_child(first, l));
        assert!(!d.append_child(l, l));
        let t = d.create_text("x").unwrap();
        assert!(!d.append_child(t, li));
        assert!(!d.insert_before(l, li, Some(t)));
        d.set_attr(l, "Class", "x y");
        assert_eq!(d.classes(l), vec!["x", "y"]);
        d.remove_attr(l, "class");
        assert!(d.classes(l).is_empty());
        d.set_inner_html(l, "<b>1</b><i>2</i>");
        assert_eq!(d.inner_html(l), "<b>1</b><i>2</i>");
        let copy = d.clone_node(l, true).unwrap();
        assert_eq!(d.inner_html(copy), "<b>1</b><i>2</i>");
        let shallow = d.clone_node(l, false).unwrap();
        assert_eq!(d.inner_html(shallow), "");
        assert_eq!(d.descendants(l).len(), 2);
        d.set_text_content(l, "");
        assert!(d.children(l).is_empty());
        assert!(d.create_element("").is_none());
    }
}
