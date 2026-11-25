//! Loading a web page for the browser window: follow redirects, read the page within a size
//! limit, fetch its style sheets and scripts, and hand back one self-contained page.
//!
//! The network is reached through [`Fetcher`], so tests use a fake one. Nothing here runs a
//! script; the page is only drawn (and its scripts run) once it is back in the window.

use crate::fetch::{Fetcher, MAX_REDIRECTS};
use mouli_web::browser::{self, Address, Content};
use std::collections::HashMap;
use std::io::Read;

/// Largest page read, in bytes.
pub const MAX_PAGE: u64 = 2 * 1024 * 1024;
/// Largest style sheet or script read, in bytes.
pub const MAX_RESOURCE: u64 = 512 * 1024;

/// A page that was loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// Where it really came from, after redirects (with the fragment that was asked for).
    pub url: String,
    /// The page, with its style sheets and scripts inside it.
    pub html: String,
}

fn headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), "MOULI-Browser/1.5".into()),
        (
            "Accept".into(),
            "text/html,application/xhtml+xml,text/plain;q=0.8,*/*;q=0.1".into(),
        ),
        ("Accept-Language".into(), "en".into()),
    ]
}

fn read_limited(mut r: Box<dyn Read>, limit: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    (&mut r)
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("the connection broke while reading: {e}"))?;
    if out.len() as u64 > limit {
        return Err(format!("the page is larger than {} KB", limit / 1024));
    }
    Ok(out)
}

pub use mouli_web::browser::error_page;

/// Loads `url`: redirects (at most [`MAX_REDIRECTS`]), size limits, the page's style sheets and
/// scripts. Plain text is shown as text; other file types are refused with a message.
///
/// # Errors
/// A message for the person: bad address, connection failure, error status, too large, or a file
/// type that cannot be shown.
pub fn load<F: Fetcher>(fetcher: &F, url: &str) -> Result<Loaded, String> {
    let first = Address::parse(url)
        .ok_or_else(|| "that is not a web address that can be opened".to_string())?;
    let fragment = first.fragment.clone();
    let mut current = first.without_fragment();
    for _ in 0..=MAX_REDIRECTS {
        let reply = fetcher.get(&current, &headers()).map_err(|e| {
            if current.starts_with("http://") {
                format!("{e} (plain http sites cannot be opened; try the https address)")
            } else {
                e
            }
        })?;
        if matches!(reply.status, 301 | 302 | 303 | 307 | 308) {
            let loc = reply
                .header("location")
                .ok_or("the site redirected without saying where")?
                .to_string();
            let next = browser::resolve(&current, &loc)
                .ok_or("the site redirected to an address that cannot be opened")?;
            current = next.without_fragment();
            continue;
        }
        if !(200..300).contains(&reply.status) {
            return Err(format!("the site answered with status {}", reply.status));
        }
        let ct = reply.header("content-type").map(str::to_string);
        let kind = browser::classify(ct.as_deref());
        if kind == Content::Other {
            return Err(format!(
                "this kind of file ({}) cannot be shown",
                ct.unwrap_or_default()
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
            ));
        }
        let bytes = read_limited(reply.body, MAX_PAGE)?;
        let text = browser::decode_body(&bytes, ct.as_deref().unwrap_or(""));
        let shown = match &fragment {
            Some(f) => format!("{current}#{f}"),
            None => current.clone(),
        };
        if kind == Content::Text {
            let html = format!("<body style=\"margin:0;background:#fff\"><pre style=\"margin:0;padding:12px;font-size:12px;color:#212121\">{}</pre></body>", text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"));
            return Ok(Loaded { url: shown, html });
        }
        let (css_urls, js_urls) = browser::subresources(&text, &current);
        let fetch_text = |u: &String| -> Option<(String, String)> {
            let r = fetcher.get(u, &headers()).ok()?;
            if !(200..300).contains(&r.status) {
                return None;
            }
            let ct = r.header("content-type").unwrap_or("").to_string();
            let body = read_limited(r.body, MAX_RESOURCE).ok()?;
            Some((u.clone(), browser::decode_body(&body, &ct)))
        };
        let css: HashMap<String, String> = css_urls.iter().filter_map(fetch_text).collect();
        let js: HashMap<String, String> = js_urls.iter().filter_map(fetch_text).collect();
        let html = if css_urls.is_empty() && js_urls.is_empty() {
            text
        } else {
            browser::inline_resources(&text, &current, &css, &js)
        };
        return Ok(Loaded { url: shown, html });
    }
    Err("the site redirected too many times".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::Reply;
    use std::cell::RefCell;
    use std::io::Cursor;

    type Canned = (u16, Vec<(String, String)>, Vec<u8>);
    type Entry<'a> = (&'a str, u16, &'a [(&'a str, &'a str)], &'a [u8]);

    struct Fake {
        pages: HashMap<String, Canned>,
        asked: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(list: &[Entry]) -> Fake {
            Fake {
                pages: list
                    .iter()
                    .map(|(u, s, h, b)| {
                        (
                            (*u).to_string(),
                            (
                                *s,
                                h.iter()
                                    .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
                                    .collect(),
                                b.to_vec(),
                            ),
                        )
                    })
                    .collect(),
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetcher for Fake {
        fn get(&self, url: &str, _: &[(String, String)]) -> Result<Reply, String> {
            self.asked.borrow_mut().push(url.to_string());
            let (status, headers, body) = self
                .pages
                .get(url)
                .cloned()
                .ok_or_else(|| format!("connection failed: no route to {url}"))?;
            Ok(Reply {
                status,
                headers,
                body: Box::new(Cursor::new(body)),
            })
        }
    }

    #[test]
    fn a_page_is_loaded_with_its_style_sheet_and_script_inside() {
        let page = b"<html><head><link rel=stylesheet href=/s.css><script src=app.js></script></head><body><p>hi</p></body></html>";
        let f = Fake::new(&[
            (
                "https://a.com/dir/p.html",
                200,
                &[("Content-Type", "text/html; charset=utf-8")],
                page,
            ),
            (
                "https://a.com/s.css",
                200,
                &[("Content-Type", "text/css")],
                b"p{color:red}",
            ),
            ("https://a.com/dir/app.js", 200, &[], b"go()"),
        ]);
        let l = load(&f, "https://a.com/dir/p.html#part").unwrap();
        assert_eq!(l.url, "https://a.com/dir/p.html#part");
        assert!(
            l.html.contains("<style>p{color:red}</style>")
                && l.html.contains("<script>go()</script>"),
            "{}",
            l.html
        );
        assert_eq!(f.asked.borrow().len(), 3);
        assert!(
            f.asked.borrow().iter().all(|u| !u.contains('#')),
            "the fragment is never sent"
        );
    }

    #[test]
    fn redirects_are_followed_a_few_times_and_not_forever() {
        let f = Fake::new(&[
            ("https://a.com/", 301, &[("Location", "/b")], b""),
            (
                "https://a.com/b",
                302,
                &[("Location", "https://c.org/c?x=1")],
                b"",
            ),
            (
                "https://c.org/c?x=1",
                200,
                &[("Content-Type", "text/html")],
                b"<p>end</p>",
            ),
            ("https://loop.com/", 302, &[("Location", "/")], b""),
            (
                "https://bad.com/",
                302,
                &[("Location", "javascript:alert(1)")],
                b"",
            ),
            ("https://none.com/", 302, &[], b""),
        ]);
        assert_eq!(
            load(&f, "https://a.com/").unwrap().url,
            "https://c.org/c?x=1"
        );
        assert!(load(&f, "https://loop.com/")
            .unwrap_err()
            .contains("too many times"));
        assert!(load(&f, "https://bad.com/")
            .unwrap_err()
            .contains("cannot be opened"));
        assert!(load(&f, "https://none.com/")
            .unwrap_err()
            .contains("without saying where"));
    }

    #[test]
    fn failures_come_back_as_messages_a_person_can_read() {
        let big = vec![b'a'; MAX_PAGE as usize + 1];
        let f = Fake::new(&[
            ("https://a.com/missing", 404, &[], b"no"),
            (
                "https://a.com/img",
                200,
                &[("Content-Type", "image/png")],
                b"\x89PNG",
            ),
            (
                "https://a.com/big",
                200,
                &[("Content-Type", "text/html")],
                &big,
            ),
            (
                "https://a.com/t.txt",
                200,
                &[("Content-Type", "text/plain")],
                b"a <b> & c",
            ),
        ]);
        assert!(load(&f, "https://a.com/missing")
            .unwrap_err()
            .contains("status 404"));
        assert!(load(&f, "https://a.com/img")
            .unwrap_err()
            .contains("image/png"));
        assert!(load(&f, "https://a.com/big")
            .unwrap_err()
            .contains("larger than"));
        assert!(load(&f, "https://down.example/")
            .unwrap_err()
            .contains("connection failed"));
        assert!(load(&f, "http://a.com/")
            .unwrap_err()
            .contains("try the https address"));
        assert!(load(&f, "javascript:alert(1)")
            .unwrap_err()
            .contains("not a web address"));
        let t = load(&f, "https://a.com/t.txt").unwrap();
        assert!(t.html.contains("<pre") && t.html.contains("a &lt;b&gt; &amp; c"));
    }

    #[test]
    fn a_missing_style_sheet_or_script_does_not_stop_the_page() {
        let page = b"<link rel=stylesheet href=/gone.css><script src=/gone.js></script><script src=/big.js></script><p id=x>hi</p>";
        let big = vec![b'x'; MAX_RESOURCE as usize + 1];
        let f = Fake::new(&[
            ("https://a.com/", 200, &[], page),
            ("https://a.com/big.js", 200, &[], &big),
        ]);
        let l = load(&f, "https://a.com/").unwrap();
        assert!(
            l.html.contains("<p id=\"x\">hi</p>")
                && !l.html.contains("gone")
                && !l.html.contains("xxxx"),
            "{}",
            l.html
        );
    }

    #[test]
    fn the_error_page_escapes_what_it_shows() {
        let p = error_page("https://a.com/<script>", "bad <b>");
        assert!(
            p.contains("&lt;script&gt;") && p.contains("bad &lt;b&gt;") && !p.contains("<script>")
        );
    }
}
