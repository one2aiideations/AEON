//! Preview: the file open in the code editor, rendered.
//!
//! HTML files are shown as a web page (our own engine: HTML and CSS), Markdown files are converted
//! first, and anything else is shown as plain text. The preview follows the editor: when the text
//! changes it is laid out again, and when the window is resized it is laid out for the new size.
//! The picture is drawn by the desktop (`draw_pixels`); `draw` is the text-mode fallback.

use crate::grid::{theme, wrap, Grid, Rect};
use crate::key::Key;
use crate::raster::Canvas;
use crate::screen::{Action, Screen};
use crate::webview::Page;
use mouli_web::interp::Value;
use mouli_web::markdown;
use mouli_web::scripting::Scripting;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

/// How a file is previewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An HTML page.
    Html,
    /// Markdown, converted to HTML first.
    Markdown,
    /// Plain text.
    Text,
}

/// Chooses how to preview a file from its name.
#[must_use]
pub fn kind_of(path: Option<&str>) -> Kind {
    let ext = path
        .and_then(|p| p.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" | "xhtml" => Kind::Html,
        "md" | "markdown" | "mdown" => Kind::Markdown,
        _ => Kind::Text,
    }
}

fn hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn source_to_html(kind: Kind, path: Option<&str>, text: &str) -> String {
    if path.is_none() && text.is_empty() {
        return "<body style=\"font-family:sans-serif;color:#555;margin:24px\"><h2>Nothing to preview yet</h2><p>Open an HTML or Markdown file in Code (F9) and it appears here.</p></body>".into();
    }
    match kind {
        Kind::Html => text.to_string(),
        Kind::Markdown => markdown::to_html(text),
        Kind::Text => format!(
            "<body style=\"margin:0;background:#fff\"><pre style=\"margin:0;padding:12px;font-size:12px;color:#212121\">{}</pre></body>",
            escape(&text.chars().take(200_000).collect::<String>())
        ),
    }
}

/// The scripts running on the current page, if it has any.
#[derive(Clone, Default)]
struct Live(Option<Rc<RefCell<Scripting>>>);

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Live({})",
            if self.0.is_some() { "scripts" } else { "none" }
        )
    }
}

#[derive(Debug, Clone, Default)]
struct State {
    live: Live,
    last_tick: Option<Instant>,
    path: Option<String>,
    hash: u64,
    len: usize,
    page: Option<Page>,
    size: (i32, i32),
    scroll: f32,
    kind: Option<Kind>,
}

/// The Preview screen.
#[derive(Debug, Clone, Default)]
pub struct PreviewScreen {
    state: RefCell<State>,
    /// A message for the person (for example where a link points).
    pub message: String,
    /// The address of the last link clicked that leads to another page (taken by the browser).
    pub last_link: Option<String>,
}

impl PreviewScreen {
    /// Makes sure the page matches the source and the window size.
    fn sync(&self, path: Option<&str>, text: &str, w: i32, h: i32) {
        let mut s = self.state.borrow_mut();
        let hh = hash(text);
        let same =
            s.page.is_some() && s.hash == hh && s.len == text.len() && s.path.as_deref() == path;
        if !same {
            let kind = kind_of(path);
            let html = source_to_html(kind, path, text);
            let moved_file = s.path.as_deref() != path;
            if kind == Kind::Html && html.to_ascii_lowercase().contains("<script") {
                let sc = Scripting::new(&html);
                s.page = Some(Page::from_dom(sc.snapshot(), w as f32, h as f32));
                s.live = Live(Some(Rc::new(RefCell::new(sc))));
                s.last_tick = Some(Instant::now());
            } else {
                s.page = Some(Page::load(&html, w as f32, h as f32));
                s.live = Live(None);
            }
            s.hash = hh;
            s.len = text.len();
            s.path = path.map(str::to_string);
            s.kind = Some(kind);
            s.size = (w, h);
            if moved_file {
                s.scroll = 0.0;
            }
        } else if s.size != (w, h) {
            if let Some(p) = s.page.as_mut() {
                p.resize(w as f32, h as f32);
            }
            s.size = (w, h);
        }
        let max = s.page.as_ref().map_or(0.0, Page::max_scroll);
        s.scroll = s.scroll.clamp(0.0, max);
    }

    /// Draws the preview into the rectangle `area` (`x`, `y`, `w`, `h`) of `canvas`, following the
    /// `source` (the path and the text of the open file).
    pub fn draw_pixels(
        &self,
        canvas: &mut Canvas,
        area: (i32, i32, i32, i32),
        source: (Option<&str>, &str),
    ) {
        let (x, y, w, h) = area;
        let (path, text) = source;
        if w < 20 || h < 20 {
            return;
        }
        self.sync(path, text, w, h);
        let s = self.state.borrow();
        if let Some(p) = &s.page {
            p.draw(canvas, x, y, w, h, s.scroll);
        }
    }

    /// Takes the changes scripts made into the page. Call with the state borrowed.
    fn refresh(s: &mut State) {
        let Some(live) = s.live.0.clone() else { return };
        let mut sc = live.borrow_mut();
        if sc.take_dirty() {
            let dom = sc.snapshot();
            if let Some(p) = s.page.as_mut() {
                p.set_dom(dom);
            }
            let max = s.page.as_ref().map_or(0.0, Page::max_scroll);
            s.scroll = s.scroll.clamp(0.0, max);
        }
    }

    /// Whether the page has scripts waiting on timers (the desktop then wakes up regularly).
    #[must_use]
    pub fn wants_ticks(&self) -> bool {
        self.state
            .borrow()
            .live
            .0
            .as_ref()
            .is_some_and(|l| l.borrow().pending_timers() > 0)
    }

    /// Lets time pass for the page's scripts; returns `true` if the picture changed.
    pub fn tick(&self) -> bool {
        let mut s = self.state.borrow_mut();
        let Some(live) = s.live.0.clone() else {
            return false;
        };
        let now = Instant::now();
        let ms = s
            .last_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64() * 1000.0)
            .min(1000.0);
        s.last_tick = Some(now);
        let ran = live.borrow_mut().advance(ms);
        let dirty = live.borrow_mut().take_dirty();
        if dirty {
            let dom = live.borrow().snapshot();
            if let Some(p) = s.page.as_mut() {
                p.set_dom(dom);
            }
        }
        ran > 0 && dirty
    }

    /// What the page's scripts have written to the console.
    #[must_use]
    pub fn console(&self) -> Vec<String> {
        self.state
            .borrow()
            .live
            .0
            .as_ref()
            .map(|l| l.borrow().console().to_vec())
            .unwrap_or_default()
    }

    /// A click at (`x`, `y`) inside the preview. Scripts see it first; if they do not prevent it,
    /// `#anchor` links scroll the page and other links are reported in [`PreviewScreen::message`].
    pub fn click(&mut self, x: f32, y: f32) {
        {
            let mut s = self.state.borrow_mut();
            let hit = s.page.as_ref().and_then(|p| p.element_at(x, y, s.scroll));
            if let (Some(node), Some(live)) = (hit, s.live.0.clone()) {
                let ok = live.borrow_mut().dispatch(node, "click");
                Self::refresh(&mut s);
                if !ok {
                    self.message.clear();
                    return;
                }
            }
        }
        let mut s = self.state.borrow_mut();
        let Some(page) = s.page.as_ref() else { return };
        let Some(href) = page.link_at(x, y, s.scroll) else {
            self.message.clear();
            return;
        };
        if let Some(id) = href.strip_prefix('#') {
            if let Some(top) = page.anchor_y(id) {
                let max = page.max_scroll();
                s.scroll = top.clamp(0.0, max);
                self.message.clear();
                return;
            }
        }
        self.message = format!("Link: {}", href.chars().take(120).collect::<String>());
        self.last_link = Some(href);
    }

    /// The current scroll offset in pixels.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.state.borrow().scroll
    }

    /// How the open file is being previewed.
    #[must_use]
    pub fn kind(&self) -> Option<Kind> {
        self.state.borrow().kind
    }
}

impl Screen for PreviewScreen {
    fn on_key(&mut self, key: Key) -> Action {
        let mut s = self.state.borrow_mut();
        if let Some(live) = s.live.0.clone() {
            let name = match key {
                Key::Char(c) => c.to_string(),
                Key::Enter => "Enter".into(),
                Key::Esc => "Escape".into(),
                Key::Tab => "Tab".into(),
                Key::Backspace => "Backspace".into(),
                Key::Up => "ArrowUp".into(),
                Key::Down => "ArrowDown".into(),
                Key::Left => "ArrowLeft".into(),
                Key::Right => "ArrowRight".into(),
                _ => String::new(),
            };
            if !name.is_empty() {
                let ok =
                    live.borrow_mut()
                        .dispatch_fields(0, "keydown", &[("key", Value::str(&name))]);
                Self::refresh(&mut s);
                if !ok {
                    return Action::None;
                }
            }
        }
        let view = s.size.1 as f32;
        let max = s.page.as_ref().map_or(0.0, Page::max_scroll);
        let step = match key {
            Key::Up | Key::Char('k') => -40.0,
            Key::Down | Key::Char('j') => 40.0,
            Key::PageUp => -(view - 40.0).max(40.0),
            Key::PageDown => (view - 40.0).max(40.0),
            Key::Home => -s.scroll,
            Key::End => max - s.scroll,
            Key::Char('r') => {
                // Force a fresh layout on the next draw.
                s.hash = 0;
                s.page = None;
                return Action::None;
            }
            _ => return Action::None,
        };
        s.scroll = (s.scroll + step).clamp(0.0, max);
        Action::None
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.boxed(area, Some("Preview"), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 10 || inner.h < 3 {
            return;
        }
        let s = self.state.borrow();
        match &s.page {
            None => {
                g.text_fit(
                    inner.x + 1,
                    inner.y,
                    inner.w - 2,
                    "Open an HTML or Markdown file in Code (F9).",
                    theme::DIM,
                    theme::BG,
                );
            }
            Some(p) => {
                let title = p
                    .title()
                    .unwrap_or_else(|| s.path.clone().unwrap_or_else(|| "page".into()));
                g.text_fit(
                    inner.x + 1,
                    inner.y,
                    inner.w - 2,
                    &title,
                    theme::ACCENT,
                    theme::BG,
                );
                let body = p
                    .dom()
                    .body()
                    .map(|b| p.dom().text_content(b))
                    .unwrap_or_default();
                let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
                for (i, line) in wrap(&flat, inner.w - 2)
                    .iter()
                    .take(inner.h.saturating_sub(2))
                    .enumerate()
                {
                    g.text_fit(
                        inner.x + 1,
                        inner.y + 1 + i,
                        inner.w - 2,
                        line,
                        theme::TEXT,
                        theme::BG,
                    );
                }
            }
        }
        if !self.message.is_empty() {
            g.text_fit(
                inner.x + 1,
                inner.y + inner.h - 1,
                inner.w - 2,
                &self.message,
                theme::WARN,
                theme::BG,
            );
        }
    }

    fn hints(&self) -> String {
        "Up/Down scroll  PgUp/PgDn page  Home/End  r reload  (shows the file open in Code)".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Canvas {
        Canvas {
            width: 300,
            height: 200,
            pixels: vec![0x00FF_FFFF; 300 * 200],
        }
    }

    fn drawn(s: &PreviewScreen, path: Option<&str>, text: &str) -> Canvas {
        let mut c = canvas();
        s.draw_pixels(&mut c, (0, 0, 300, 200), (path, text));
        c
    }

    fn ink(c: &Canvas) -> usize {
        c.pixels.iter().filter(|&&p| p != 0x00FF_FFFF).count()
    }

    #[test]
    fn the_kind_follows_the_file_name() {
        assert_eq!(kind_of(Some("index.html")), Kind::Html);
        assert_eq!(kind_of(Some("a/b/Page.HTM")), Kind::Html);
        assert_eq!(kind_of(Some("README.md")), Kind::Markdown);
        assert_eq!(kind_of(Some("notes.markdown")), Kind::Markdown);
        assert_eq!(kind_of(Some("main.rs")), Kind::Text);
        assert_eq!(kind_of(Some("noextension")), Kind::Text);
        assert_eq!(kind_of(None), Kind::Text);
    }

    #[test]
    fn html_markdown_and_text_are_all_drawn() {
        let s = PreviewScreen::default();
        let html = drawn(
            &s,
            Some("a.html"),
            "<body style=\"background:#00f\"><h1>Hi</h1></body>",
        );
        assert_eq!(
            html.get(290, 190),
            Some(0x0000_00FF),
            "the page background fills the window"
        );
        assert_eq!(s.kind(), Some(Kind::Html));
        let md = drawn(
            &PreviewScreen::default(),
            Some("a.md"),
            "# Title\n\nsome *text*\n",
        );
        assert!(ink(&md) > 300);
        let txt = drawn(&PreviewScreen::default(), Some("a.rs"), "fn main() {}\n");
        assert!(ink(&txt) > 50);
        let empty = drawn(&PreviewScreen::default(), None, "");
        assert!(ink(&empty) > 100, "an empty preview explains itself");
    }

    #[test]
    fn scripts_on_a_page_react_to_clicks_and_timers() {
        let html = "<body style=\"margin:0;background:#f00\"><div style=\"height:200px\"></div><script>\
            document.body.addEventListener('click', () => { document.body.style.background = '#00f'; });\
            setTimeout(() => { document.body.style.background = '#0f0'; }, 200);\
            </script></body>";
        let mut s = PreviewScreen::default();
        let a = drawn(&s, Some("a.html"), html);
        assert_eq!(
            a.get(5, 5),
            Some(0x00FF_0000),
            "scripts ran and the first picture is the start state"
        );
        assert!(s.wants_ticks());
        s.click(50.0, 50.0);
        let b = drawn(&s, Some("a.html"), html);
        assert_eq!(
            b.get(5, 5),
            Some(0x0000_00FF),
            "a click reached the page's listener"
        );
        // Time passing runs the timer (the real clock is used, so wait a little longer than it).
        std::thread::sleep(std::time::Duration::from_millis(260));
        assert!(s.tick(), "the timer changed the picture");
        let c = drawn(&s, Some("a.html"), html);
        assert_eq!(c.get(5, 5), Some(0x0000_FF00));
        assert!(!s.wants_ticks());
        assert!(s.console().is_empty());
        // A page without scripts has none of this.
        let plain = PreviewScreen::default();
        drawn(&plain, Some("b.html"), "<p>x</p>");
        assert!(!plain.wants_ticks() && !plain.tick());
        // A script error is kept for the person, not thrown away.
        let bad = PreviewScreen::default();
        drawn(&bad, Some("c.html"), "<p>x</p><script>nope()</script>");
        assert!(bad
            .console()
            .iter()
            .any(|l| l.contains("nope is not defined")));
    }

    #[test]
    fn the_preview_follows_edits_and_files() {
        let s = PreviewScreen::default();
        let a = drawn(
            &s,
            Some("a.html"),
            "<body style=\"background:#f00\"></body>",
        );
        assert_eq!(a.get(5, 5), Some(0x00FF_0000));
        let b = drawn(
            &s,
            Some("a.html"),
            "<body style=\"background:#0f0\"></body>",
        );
        assert_eq!(
            b.get(5, 5),
            Some(0x0000_FF00),
            "changed text is laid out again"
        );
        let c = drawn(
            &s,
            Some("b.html"),
            "<body style=\"background:#00f\"></body>",
        );
        assert_eq!(c.get(5, 5), Some(0x0000_00FF), "another file");
    }

    #[test]
    fn scrolling_is_limited_and_resets_for_a_new_file() {
        let mut s = PreviewScreen::default();
        let tall = "<body style=\"margin:0\"><div style=\"height:1000px\"></div></body>";
        drawn(&s, Some("t.html"), tall);
        s.on_key(Key::Down);
        assert_eq!(s.scroll(), 40.0);
        s.on_key(Key::PageDown);
        assert_eq!(s.scroll(), 40.0 + 160.0);
        s.on_key(Key::End);
        assert_eq!(s.scroll(), 800.0, "limited to the end of the page");
        s.on_key(Key::Down);
        assert_eq!(s.scroll(), 800.0);
        s.on_key(Key::Home);
        assert_eq!(s.scroll(), 0.0);
        s.on_key(Key::Up);
        assert_eq!(s.scroll(), 0.0);
        s.on_key(Key::End);
        drawn(&s, Some("other.html"), tall);
        assert_eq!(s.scroll(), 0.0, "a different file starts at the top");
        // Reload keeps working and unknown keys do nothing.
        assert_eq!(s.on_key(Key::Char('r')), Action::None);
        assert_eq!(s.on_key(Key::Char('z')), Action::None);
        drawn(&s, Some("other.html"), tall);
    }

    #[test]
    fn anchor_links_scroll_and_other_links_are_reported() {
        let html = "<body style=\"margin:0;font-size:16px\"><p style=\"margin:0\"><a href=\"#end\">jump</a> and <a href=\"https://example.com/x\">out</a></p><div style=\"height:600px\"></div><h2 id=\"end\" style=\"margin:0\">End</h2></body>";
        let mut s = PreviewScreen::default();
        drawn(&s, Some("l.html"), html);
        s.click(5.0, 8.0);
        assert!(s.scroll() > 100.0, "scrolled to the anchor: {}", s.scroll());
        let mut s = PreviewScreen::default();
        drawn(&s, Some("l.html"), html);
        // "jump and " is 9 characters; click on "out".
        s.click(80.0, 8.0);
        assert!(
            s.message.starts_with("Link: https://example.com"),
            "{:?}",
            s.message
        );
        s.click(250.0, 150.0);
        assert!(
            s.message.is_empty(),
            "clicking elsewhere clears the message"
        );
    }

    #[test]
    fn a_resized_window_gets_a_new_layout_and_tiny_windows_are_ignored() {
        let s = PreviewScreen::default();
        let text = "<body style=\"margin:0\">aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk llll</body>";
        let mut c = canvas();
        s.draw_pixels(&mut c, (0, 0, 300, 200), (Some("a.html"), text));
        let wide = s.state.borrow().page.as_ref().unwrap().height();
        s.draw_pixels(&mut c, (0, 0, 60, 200), (Some("a.html"), text));
        assert!(s.state.borrow().page.as_ref().unwrap().height() > wide);
        let before = c.clone();
        s.draw_pixels(&mut c, (0, 0, 5, 5), (Some("a.html"), text));
        assert_eq!(
            c, before,
            "a window too small to show anything draws nothing"
        );
    }

    #[test]
    fn the_text_mode_fallback_shows_the_title_and_text() {
        let s = PreviewScreen::default();
        drawn(
            &s,
            Some("p.html"),
            "<html><head><title>My page</title></head><body><p>hello world</p></body></html>",
        );
        let mut g = Grid::new(60, 8);
        let a = g.area();
        s.draw(&mut g, a);
        let t = g.to_text();
        assert!(t.contains("My page") && t.contains("hello world"), "{t}");
        let empty = PreviewScreen::default();
        let mut g = Grid::new(60, 8);
        let a = g.area();
        empty.draw(&mut g, a);
        assert!(g.to_text().contains("Open an HTML or Markdown file"));
        assert!(s.hints().contains("scroll"));
    }
}
