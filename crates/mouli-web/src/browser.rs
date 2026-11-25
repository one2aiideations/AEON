//! What a browser needs besides a page engine, with no input or output of its own: web
//! addresses (reading, joining relative links, refusing unsafe ones), the history, finding the
//! style sheets and scripts a page needs, and putting what was fetched back into the page.
//!
//! Fetching is the caller's job; this module only decides *what* to fetch and what to do with it.

use crate::dom::{Dom, Kind};
use std::collections::HashMap;

/// Longest address accepted, in bytes.
pub const MAX_URL: usize = 4096;
/// Most style sheets and most scripts fetched for one page.
pub const MAX_SUBRESOURCES: usize = 8;
/// Most addresses kept in the history.
pub const MAX_HISTORY: usize = 200;

/// A web address that is safe to open: `http` or `https`, with a host and no user name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// `http` or `https`.
    pub scheme: String,
    /// The host, lowercase.
    pub host: String,
    /// An explicit port.
    pub port: Option<u16>,
    /// The path, starting with `/`, dot segments removed.
    pub path: String,
    /// The query, without `?`.
    pub query: Option<String>,
    /// The fragment, without `#`.
    pub fragment: Option<String>,
}

impl Address {
    /// Reads an absolute address. `None` for other schemes (`javascript:`, `data:`, `file:`),
    /// missing hosts, user names, control characters or very long text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Address> {
        let t = text.trim();
        if t.len() > MAX_URL || t.chars().any(|c| c.is_control() || c == ' ' || c == '\\') {
            return None;
        }
        let (scheme, rest) = t.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let (rest, fragment) = match rest.split_once('#') {
            Some((a, f)) => (a, Some(f.to_string())),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q.to_string())),
            None => (rest, None),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.contains('@') || authority.is_empty() {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => {
                (h, Some(p.parse::<u16>().ok().filter(|&n| n != 0)?))
            }
            _ => (authority, None),
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty()
            || !host.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '[' | ']' | ':' | '_')
            })
        {
            return None;
        }
        Some(Address {
            scheme,
            host,
            port,
            path: remove_dots(path),
            query,
            fragment,
        })
    }

    /// The address as text, without the fragment (what is sent to the server).
    #[must_use]
    pub fn without_fragment(&self) -> String {
        let mut s = format!("{}://{}", self.scheme, self.host);
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        s.push_str(&self.path);
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        s
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.without_fragment())?;
        if let Some(fr) = &self.fragment {
            write!(f, "#{fr}")?;
        }
        Ok(())
    }
}

fn remove_dots(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    for (i, s) in segs.iter().enumerate() {
        match *s {
            "." => {
                if i + 1 == segs.len() {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if i + 1 == segs.len() {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    }
}

/// Reads what a person typed in the address bar: a full address, a host name (`https://` is
/// assumed), or, if it does not look like an address, `None`.
#[must_use]
pub fn from_typed(text: &str) -> Option<Address> {
    let t = text.trim();
    if t.contains("://") {
        return Address::parse(t);
    }
    if t.is_empty() || t.contains(' ') || !(t.contains('.') || t.starts_with("localhost")) {
        return None;
    }
    Address::parse(&format!("https://{t}"))
}

/// Joins a link found on the page at `base` to an absolute address. `None` when the result is
/// not a safe web address.
#[must_use]
pub fn resolve(base: &str, link: &str) -> Option<Address> {
    let link = link.trim();
    if link.is_empty() || link.len() > MAX_URL {
        return None;
    }
    if link.contains("://") {
        return Address::parse(link);
    }
    let b = Address::parse(base)?;
    let text = if let Some(rest) = link.strip_prefix("//") {
        format!("{}://{rest}", b.scheme)
    } else if link.starts_with('#') {
        format!("{}{link}", b.without_fragment())
    } else if link.starts_with('?') {
        format!("{}://{}{}{}{}", b.scheme, authority(&b), b.path, link, "")
    } else if link.starts_with('/') {
        format!("{}://{}{link}", b.scheme, authority(&b))
    } else if link.contains(':')
        && link
            .split(':')
            .next()
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_alphabetic()))
    {
        return None;
    } else {
        let dir = match b.path.rfind('/') {
            Some(i) => &b.path[..=i],
            None => "/",
        };
        format!("{}://{}{dir}{link}", b.scheme, authority(&b))
    };
    Address::parse(&text)
}

fn authority(a: &Address) -> String {
    match a.port {
        Some(p) => format!("{}:{p}", a.host),
        None => a.host.clone(),
    }
}

/// The pages visited, with a position for back and forward.
#[derive(Debug, Clone, Default)]
pub struct History {
    entries: Vec<String>,
    pos: usize,
}

impl History {
    /// Records a visit (dropping any forward pages).
    pub fn visit(&mut self, url: &str) {
        if self.entries.get(self.pos).map(String::as_str) == Some(url) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.pos + 1);
        }
        self.entries.push(url.to_string());
        if self.entries.len() > MAX_HISTORY {
            self.entries.remove(0);
        }
        self.pos = self.entries.len() - 1;
    }

    /// The current address.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.pos).map(String::as_str)
    }

    /// Steps back; the address to open.
    pub fn back(&mut self) -> Option<&str> {
        if self.pos == 0 {
            return None;
        }
        self.pos -= 1;
        self.current()
    }

    /// Steps forward; the address to open.
    pub fn forward(&mut self) -> Option<&str> {
        if self.pos + 1 >= self.entries.len() {
            return None;
        }
        self.pos += 1;
        self.current()
    }

    /// Whether there is a page to go back to.
    #[must_use]
    pub fn can_back(&self) -> bool {
        self.pos > 0
    }

    /// Whether there is a page to go forward to.
    #[must_use]
    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.entries.len()
    }
}

/// The style sheets and scripts a page asks for, as absolute addresses (at most
/// [`MAX_SUBRESOURCES`] of each, no duplicates, unsafe ones left out).
#[must_use]
pub fn subresources(html: &str, base: &str) -> (Vec<String>, Vec<String>) {
    let dom = Dom::parse(html);
    let mut css = Vec::new();
    let mut js = Vec::new();
    for n in dom.find_all("link") {
        let rel = dom.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
        if rel.split_whitespace().any(|r| r == "stylesheet") {
            if let Some(a) = dom.attr(n, "href").and_then(|h| resolve(base, h)) {
                let u = a.without_fragment();
                if !css.contains(&u) && css.len() < MAX_SUBRESOURCES {
                    css.push(u);
                }
            }
        }
    }
    for n in dom.find_all("script") {
        if let Some(a) = dom.attr(n, "src").and_then(|h| resolve(base, h)) {
            let u = a.without_fragment();
            if !js.contains(&u) && js.len() < MAX_SUBRESOURCES {
                js.push(u);
            }
        }
    }
    (css, js)
}

/// Puts the fetched style sheets and scripts into the page: each `<link rel=stylesheet>` becomes
/// a `<style>` with the fetched text, and each `<script src>` gets the fetched text as its body.
/// Resources that were not fetched are left out of the page (a failed script does not run).
#[must_use]
pub fn inline_resources(
    html: &str,
    base: &str,
    css: &HashMap<String, String>,
    js: &HashMap<String, String>,
) -> String {
    let mut dom = Dom::parse(html);
    for n in dom.find_all("link") {
        let rel = dom.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "stylesheet") {
            continue;
        }
        let url = dom
            .attr(n, "href")
            .and_then(|h| resolve(base, h))
            .map(|a| a.without_fragment());
        if let (Some(text), Some(parent)) = (url.and_then(|u| css.get(&u)), dom.parent(n)) {
            if let Some(style) = dom.create_element("style") {
                dom.set_text_content(style, text);
                dom.insert_before(parent, style, Some(n));
            }
        }
        dom.detach(n);
    }
    for n in dom.find_all("script") {
        let Some(src) = dom.attr(n, "src").map(str::to_string) else {
            continue;
        };
        let url = resolve(base, &src).map(|a| a.without_fragment());
        match url.and_then(|u| js.get(&u)) {
            Some(text) => {
                dom.remove_attr(n, "src");
                dom.set_text_content(n, text);
            }
            None => dom.detach(n),
        }
    }
    dom.inner_html(0)
}

/// Turns the bytes of a response into text: UTF-8, or Latin-1 when the content type says so.
#[must_use]
pub fn decode_body(bytes: &[u8], content_type: &str) -> String {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("iso-8859-1") || ct.contains("latin1") || ct.contains("windows-1252") {
        bytes.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// What kind of thing a response is, from its `Content-Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// A web page.
    Html,
    /// Plain text (shown as text).
    Text,
    /// Anything else (not shown).
    Other,
}

/// Classifies a `Content-Type` header value (a missing one is treated as HTML).
#[must_use]
pub fn classify(content_type: Option<&str>) -> Content {
    let Some(ct) = content_type else {
        return Content::Html;
    };
    let ct = ct.to_ascii_lowercase();
    if ct.starts_with("text/html") || ct.starts_with("application/xhtml") {
        Content::Html
    } else if ct.starts_with("text/") {
        Content::Text
    } else {
        Content::Other
    }
}

/// Whether a node of the page is a link and where it points (for the page's own `<a>` check).
#[must_use]
pub fn link_target(dom: &Dom, node: usize) -> Option<String> {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if matches!(dom.node(n).map(|x| &x.kind), Some(Kind::Element { tag, .. }) if tag == "a") {
            return dom.attr(n, "href").map(str::to_string);
        }
        cur = dom.parent(n);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> String {
        Address::parse(s)
            .map(|a| a.to_string())
            .unwrap_or_else(|| "none".into())
    }

    #[test]
    fn addresses_are_read_and_unsafe_ones_refused() {
        assert_eq!(
            u("HTTPS://Example.COM/a/../b/./c?x=1#top"),
            "https://example.com/b/c?x=1#top"
        );
        assert_eq!(u("http://localhost:8080"), "http://localhost:8080/");
        assert_eq!(u("https://a.b/%20x"), "https://a.b/%20x");
        for bad in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "ftp://x/y",
            "https://user:pw@evil.com/",
            "https://",
            "https:///x",
            "https://a b/",
            "https://a.com:0/",
            "https://a.com:99999/",
            "https://exa\u{0}mple.com/",
            "https://a.com\\@b.com/",
            "https://ex$ample.com/",
            "",
        ] {
            assert_eq!(u(bad), "none", "{bad:?}");
        }
        assert_eq!(u(&format!("https://a.com/{}", "x".repeat(MAX_URL))), "none");
        assert_eq!(
            Address::parse("https://a.com:8443/p").unwrap().port,
            Some(8443)
        );
        assert_eq!(
            Address::parse("https://a.com/p?q#f")
                .unwrap()
                .without_fragment(),
            "https://a.com/p?q"
        );
    }

    #[test]
    fn typed_text_becomes_an_address_when_it_looks_like_one() {
        assert_eq!(
            from_typed("example.com").unwrap().to_string(),
            "https://example.com/"
        );
        assert_eq!(
            from_typed("  http://localhost:3000/x ")
                .unwrap()
                .to_string(),
            "http://localhost:3000/x"
        );
        assert_eq!(from_typed("localhost:3000").unwrap().port, Some(3000));
        assert!(from_typed("hello world").is_none());
        assert!(from_typed("hello").is_none());
        assert!(from_typed("javascript:alert(1)").is_none());
        assert!(from_typed("").is_none());
    }

    #[test]
    fn relative_links_are_joined_like_a_browser_does() {
        let base = "https://example.com/docs/guide/index.html?v=2#old";
        let r = |l: &str| resolve(base, l).map(|a| a.to_string());
        assert_eq!(
            r("page2.html").as_deref(),
            Some("https://example.com/docs/guide/page2.html")
        );
        assert_eq!(
            r("../up.html").as_deref(),
            Some("https://example.com/docs/up.html")
        );
        assert_eq!(
            r("../../../../way.html").as_deref(),
            Some("https://example.com/way.html")
        );
        assert_eq!(r("/root").as_deref(), Some("https://example.com/root"));
        assert_eq!(
            r("//cdn.example.org/x.js").as_deref(),
            Some("https://cdn.example.org/x.js")
        );
        assert_eq!(
            r("#sec").as_deref(),
            Some("https://example.com/docs/guide/index.html?v=2#sec")
        );
        assert_eq!(
            r("?q=1").as_deref(),
            Some("https://example.com/docs/guide/index.html?q=1")
        );
        assert_eq!(
            r("http://other.com/a").as_deref(),
            Some("http://other.com/a")
        );
        assert_eq!(
            r("sub/").as_deref(),
            Some("https://example.com/docs/guide/sub/")
        );
        assert_eq!(r(".").as_deref(), Some("https://example.com/docs/guide/"));
        for bad in [
            "javascript:void(0)",
            "mailto:a@b.c",
            "data:text/plain,x",
            "tel:123",
            "",
            "   ",
        ] {
            assert!(r(bad).is_none(), "{bad:?}");
        }
        assert!(resolve("not a url", "x").is_none());
        assert_eq!(
            resolve("https://a.com:8443/x/y", "z").unwrap().to_string(),
            "https://a.com:8443/x/z"
        );
    }

    #[test]
    fn history_goes_back_and_forward_and_drops_forward_pages_on_a_new_visit() {
        let mut h = History::default();
        assert!(
            h.current().is_none() && !h.can_back() && h.back().is_none() && h.forward().is_none()
        );
        for p in ["a", "b", "c"] {
            h.visit(p);
        }
        h.visit("c");
        assert_eq!(
            (h.current(), h.can_back(), h.can_forward()),
            (Some("c"), true, false)
        );
        assert_eq!(h.back(), Some("b"));
        assert_eq!(h.back(), Some("a"));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some("b"));
        h.visit("x");
        assert!(!h.can_forward());
        assert_eq!(h.back(), Some("b"));
        for n in 0..300 {
            h.visit(&format!("p{n}"));
        }
        assert_eq!(h.current(), Some("p299"));
        let mut steps = 0;
        while h.back().is_some() {
            steps += 1;
        }
        assert!(steps < MAX_HISTORY);
    }

    #[test]
    fn style_sheets_and_scripts_are_found_fetched_and_put_back() {
        let html = "<html><head><link rel='stylesheet' href='/a.css'><link rel=icon href=/i.png><link rel='preload stylesheet' href='b.css'><link rel=stylesheet href='javascript:x'><script src='//cdn.x.org/app.js'></script><script src=app.js></script></head><body><p>hi</p><script>inline()</script><script src='gone.js'></script></body></html>";
        let base = "https://example.com/dir/page.html";
        let (css, js) = subresources(html, base);
        assert_eq!(
            css,
            vec!["https://example.com/a.css", "https://example.com/dir/b.css"]
        );
        assert_eq!(
            js,
            vec![
                "https://cdn.x.org/app.js",
                "https://example.com/dir/app.js",
                "https://example.com/dir/gone.js"
            ]
        );
        let css_map: HashMap<String, String> = [(
            "https://example.com/a.css".to_string(),
            "p{color:red}".to_string(),
        )]
        .into();
        let js_map: HashMap<String, String> = [(
            "https://example.com/dir/app.js".to_string(),
            "one()".to_string(),
        )]
        .into();
        let out = inline_resources(html, base, &css_map, &js_map);
        assert!(out.contains("<style>p{color:red}</style>"), "{out}");
        assert!(
            !out.contains("stylesheet"),
            "every stylesheet link is gone, fetched or not: {out}"
        );
        assert!(out.contains("<script>one()</script>"), "{out}");
        assert!(out.contains("<script>inline()</script>"), "{out}");
        assert!(
            !out.contains("cdn.x.org") && !out.contains("gone.js"),
            "scripts that were not fetched are dropped: {out}"
        );
        // The result is a page the engine can read again.
        assert_eq!(Dom::parse(&out).inline_scripts(), vec!["one()", "inline()"]);
        // Duplicates and too many are cut.
        let many: String = (0..30)
            .map(|i| {
                format!(
                    "<link rel=stylesheet href=/{}.css><link rel=stylesheet href=/0.css>",
                    i % 20
                )
            })
            .collect();
        assert_eq!(subresources(&many, base).0.len(), MAX_SUBRESOURCES);
    }

    #[test]
    fn bodies_are_decoded_and_content_types_classified() {
        assert_eq!(
            decode_body("héllo".as_bytes(), "text/html; charset=utf-8"),
            "héllo"
        );
        assert_eq!(
            decode_body(&[0x68, 0xE9], "text/html; charset=ISO-8859-1"),
            "hé"
        );
        assert!(decode_body(&[0xFF, 0x61], "text/html").contains('a'));
        assert_eq!(classify(Some("text/html; charset=utf-8")), Content::Html);
        assert_eq!(classify(None), Content::Html);
        assert_eq!(classify(Some("application/xhtml+xml")), Content::Html);
        assert_eq!(classify(Some("text/plain")), Content::Text);
        assert_eq!(classify(Some("image/png")), Content::Other);
        assert_eq!(classify(Some("application/pdf")), Content::Other);
        let d = Dom::parse("<p><a id=l href='/x'><b id=b>t</b></a><i id=i>n</i></p>");
        assert_eq!(
            link_target(&d, d.by_id("b").unwrap()).as_deref(),
            Some("/x")
        );
        assert_eq!(link_target(&d, d.by_id("i").unwrap()), None);
    }
}
