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
}
