//! The browser: an address bar and a web page.
//!
//! This screen holds what a person sees and does (typing an address, back, forward, reload,
//! clicking links); it does no network work. Opening an address returns [`Action::Navigate`], the
//! host fetches the page and reports back with [`BrowserScreen::loaded`] or
//! [`BrowserScreen::failed`]. The page itself is drawn by the same engine as the Preview, scripts
//! included.

use crate::grid::{theme, Grid, Rect};
use crate::key::Key;
use crate::raster::Canvas;
use crate::screen::{Action, Screen};
use crate::screens::preview::PreviewScreen;
use mouli_web::browser::{self, History};

/// Longest address that can be typed.
const MAX_TYPED: usize = 2000;

/// The browser screen.
#[derive(Debug, Clone, Default)]
pub struct BrowserScreen {
    /// What the address bar shows.
    pub address: String,
    editing: bool,
    fresh: bool,
    history: History,
    /// A line about what is happening (loading, errors).
    pub status: String,
    page: Option<(String, String)>,
    loading: Option<String>,
    record: bool,
    /// The page view (the same engine as the Preview).
    pub view: PreviewScreen,
}

impl BrowserScreen {
    /// Opens what was typed in the address bar.
    pub fn go(&mut self, input: &str) -> Action {
        match browser::from_typed(input) {
            Some(a) => self.start(a.to_string(), true),
            None => {
                self.status = format!(
                    "\"{}\" is not a web address",
                    input.chars().take(60).collect::<String>()
                );
                Action::None
            }
        }
    }

    fn start(&mut self, url: String, record: bool) -> Action {
        self.status = format!("Loading {}", url.chars().take(100).collect::<String>());
        self.loading = Some(url.clone());
        self.record = record;
        self.address.clone_from(&url);
        self.editing = false;
        Action::Navigate(url)
    }

    /// Goes back one page (nothing if there is none).
    pub fn back(&mut self) -> Action {
        match self.history.back().map(str::to_string) {
            Some(u) => self.start(u, false),
            None => Action::None,
        }
    }

    /// Goes forward one page.
    pub fn forward(&mut self) -> Action {
        match self.history.forward().map(str::to_string) {
            Some(u) => self.start(u, false),
            None => Action::None,
        }
    }

    /// Loads the current page again.
    pub fn reload(&mut self) -> Action {
        match self
            .page
            .as_ref()
            .map(|(u, _)| u.clone())
            .or_else(|| self.loading.clone())
        {
            Some(u) => self.start(u, false),
            None => Action::None,
        }
    }

    /// The host has the page asked for as `requested`; `url` is where it came from after
    /// redirects.
    pub fn loaded(&mut self, requested: &str, url: &str, html: String) {
        if self.loading.as_deref() != Some(requested) {
            return; // an older request that was replaced
        }
        self.loading = None;
        if self.record {
            self.history.visit(url);
        }
        self.address = url.to_string();
        self.status = "Done".into();
        self.page = Some((url.to_string(), html));
        self.view.reset();
    }

    /// The host could not load `requested`; `page` is a page that explains it.
    pub fn failed(&mut self, requested: &str, message: &str, page: String) {
        if self.loading.as_deref() != Some(requested) {
            return;
        }
        self.loading = None;
        self.status = message.chars().take(200).collect();
        self.address = requested.to_string();
        self.page = Some((requested.to_string(), page));
        self.view.reset();
    }

    /// The address of the page being shown.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.page.as_ref().map(|(u, _)| u.as_str())
    }

    /// The title of the page being shown.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        let (_, html) = self.page.as_ref()?;
        mouli_web::dom::Dom::parse(html).title()
    }

    /// Whether the address bar is being edited.
    #[must_use]
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// Draws the address bar into one row of the grid.
    pub fn draw_bar(&self, g: &mut Grid) {
        let w = g.cols();
        g.fill(Rect::new(0, 0, w, 1), theme::PANEL);
        let on = |b: bool| if b { theme::TEXT } else { theme::DIM };
        g.text(1, 0, "<", on(self.history.can_back()), theme::PANEL);
        g.text(3, 0, ">", on(self.history.can_forward()), theme::PANEL);
        g.text(5, 0, "r", on(self.page.is_some()), theme::PANEL);
        if w <= 8 {
            return;
        }
        let field = Rect::new(7, 0, w - 8, 1);
        let bg = if self.editing {
            theme::BG
        } else {
            theme::PANEL
        };
        g.fill(field, bg);
        let mut shown: String = self.address.chars().collect();
        if self.editing {
            shown.push('_');
        }
        let skip = shown.chars().count().saturating_sub(field.w);
        let shown: String = shown.chars().skip(skip).collect();
        g.text_fit(
            field.x,
            0,
            field.w,
            &shown,
            if self.editing {
                theme::TEXT
            } else {
                theme::ACCENT
            },
            bg,
        );
    }

    /// Draws the page into `area` of `canvas`.
    pub fn draw_pixels(&self, canvas: &mut Canvas, area: (i32, i32, i32, i32)) {
        let (x, y, w, h) = area;
        if w < 20 || h < 20 {
            return;
        }
        match &self.page {
            Some((url, html)) => {
                let path = format!("{url}.html");
                self.view.draw_pixels(canvas, area, (Some(&path), html));
            }
            None => {
                self.view.draw_pixels(
                    canvas,
                    area,
                    (
                        Some("start.html"),
                        "<body style=\"margin:0;background:#fafafa\"><div style=\"padding:32px;font-size:14px;color:#212121\"><h2 style=\"margin:0 0 12px 0\">Browser</h2><p style=\"margin:0\">Press l and type an address, for example example.com, then Enter.</p></div></body>",
                    ),
                );
                let _ = (x, y);
            }
        }
    }

    /// A click in the address bar, at character column `col`.
    pub fn click_bar(&mut self, col: usize) -> Action {
        match col {
            0..=2 => self.back(),
            3..=4 => self.forward(),
            5..=6 => self.reload(),
            _ => {
                self.editing = true;
                self.fresh = true;
                Action::None
            }
        }
    }

    /// A click on the page at (`x`, `y`) in page pixels. Links open as new pages.
    pub fn click(&mut self, x: f32, y: f32) -> Action {
        self.view.click(x, y);
        self.follow()
    }

    /// Opens the link the page view last reported (a clicked link or a submitted form).
    fn follow(&mut self) -> Action {
        let Some(href) = self.view.last_link.take() else {
            return Action::None;
        };
        let base = self.current().unwrap_or("").to_string();
        match browser::resolve(&base, &href) {
            Some(a) => self.start(a.to_string(), true),
            None => {
                self.status = "That link cannot be opened".into();
                Action::None
            }
        }
    }
}

impl Screen for BrowserScreen {
    fn on_key(&mut self, key: Key) -> Action {
        if self.editing {
            match key {
                Key::Enter => {
                    let text = self.address.clone();
                    self.editing = false;
                    return self.go(&text);
                }
                Key::Esc => {
                    self.editing = false;
                    self.address = self.current().unwrap_or("").to_string();
                }
                Key::Backspace => {
                    if self.fresh {
                        self.address.clear();
                    } else {
                        self.address.pop();
                    }
                    self.fresh = false;
                }
                Key::Ctrl('u') => self.address.clear(),
                Key::Char(c) => {
                    if self.fresh {
                        self.address.clear();
                        self.fresh = false;
                    }
                    if self.address.chars().count() < MAX_TYPED {
                        self.address.push(c);
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        if self.view.has_focus() {
            // Typing into a field on the page.
            if key == Key::Ctrl('l') {
                self.editing = true;
                self.fresh = true;
                return Action::None;
            }
            self.view.on_key(key);
            return self.follow();
        }
        match key {
            Key::Ctrl('l') | Key::Char('l' | '/') => {
                self.editing = true;
                self.fresh = true;
                Action::None
            }
            Key::Char('[') => self.back(),
            Key::Char(']') => self.forward(),
            Key::Char('r') => self.reload(),
            other => {
                self.view.on_key(other);
                self.follow()
            }
        }
    }

    fn draw(&self, g: &mut Grid, area: Rect) {
        g.boxed(area, Some("Browser"), theme::ACCENT, theme::BG);
        let inner = area.inset(1);
        if inner.w < 10 || inner.h < 3 {
            return;
        }
        g.text_fit(
            inner.x + 1,
            inner.y,
            inner.w - 2,
            &self.address,
            theme::ACCENT,
            theme::BG,
        );
        let line = match &self.page {
            Some(_) => self.title().unwrap_or_else(|| "page".into()),
            None => "Press l, type an address, Enter.".into(),
        };
        g.text_fit(
            inner.x + 1,
            inner.y + 1,
            inner.w - 2,
            &line,
            theme::TEXT,
            theme::BG,
        );
        g.text_fit(
            inner.x + 1,
            inner.y + 2,
            inner.w - 2,
            &self.status,
            theme::DIM,
            theme::BG,
        );
    }

    fn hints(&self) -> String {
        if self.editing {
            "Type an address  Enter open  Esc cancel".into()
        } else {
            format!(
                "l address  [ back  ] forward  r reload  Up/Down scroll    {}",
                self.status
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &mut BrowserScreen, text: &str) {
        for c in text.chars() {
            s.on_key(Key::Char(c));
        }
    }

    #[test]
    fn typing_an_address_opens_it_and_the_page_comes_back_later() {
        let mut s = BrowserScreen::default();
        assert_eq!(s.on_key(Key::Char('l')), Action::None);
        assert!(s.editing());
        typed(&mut s, "example.com/a");
        assert_eq!(
            s.on_key(Key::Enter),
            Action::Navigate("https://example.com/a".into())
        );
        assert!(s.status.starts_with("Loading"));
        s.loaded(
            "https://example.com/a",
            "https://example.com/a/",
            "<h1>Hi</h1>".into(),
        );
        assert_eq!(
            s.current(),
            Some("https://example.com/a/"),
            "redirects change the shown address"
        );
        assert_eq!(s.address, "https://example.com/a/");
        assert_eq!(s.status, "Done");
        // A reply for a request that was replaced is ignored.
        s.on_key(Key::Ctrl('l'));
        typed(&mut s, "second.org");
        s.on_key(Key::Enter);
        s.loaded(
            "https://example.com/a",
            "https://example.com/a",
            "<p>old</p>".into(),
        );
        assert_eq!(s.current(), Some("https://example.com/a/"));
        s.loaded(
            "https://second.org/",
            "https://second.org/",
            "<title>Two</title>".into(),
        );
        assert_eq!(s.title().as_deref(), Some("Two"));
    }

    #[test]
    fn not_an_address_is_refused_with_a_message() {
        let mut s = BrowserScreen::default();
        assert_eq!(s.go("hello world"), Action::None);
        assert!(s.status.contains("not a web address"));
        assert_eq!(s.go("javascript:alert(1)"), Action::None);
        assert_eq!(s.go(""), Action::None);
    }

    #[test]
    fn back_forward_and_reload_walk_the_history() {
        let mut s = BrowserScreen::default();
        for u in ["https://a.com/", "https://b.com/", "https://c.com/"] {
            assert_eq!(s.go(u), Action::Navigate(u.into()));
            s.loaded(u, u, "<p>x</p>".into());
        }
        assert_eq!(s.back(), Action::Navigate("https://b.com/".into()));
        s.loaded("https://b.com/", "https://b.com/", "<p>b</p>".into());
        assert_eq!(s.back(), Action::Navigate("https://a.com/".into()));
        s.loaded("https://a.com/", "https://a.com/", "<p>a</p>".into());
        assert_eq!(s.back(), Action::None);
        assert_eq!(s.forward(), Action::Navigate("https://b.com/".into()));
        s.loaded("https://b.com/", "https://b.com/", "<p>b</p>".into());
        assert_eq!(s.reload(), Action::Navigate("https://b.com/".into()));
        s.loaded("https://b.com/", "https://b.com/", "<p>b2</p>".into());
        // A new page after going back drops the forward pages.
        s.go("https://d.com/");
        s.loaded("https://d.com/", "https://d.com/", "<p>d</p>".into());
        assert_eq!(s.forward(), Action::None);
        assert_eq!(
            s.on_key(Key::Char('[')),
            Action::Navigate("https://b.com/".into())
        );
    }

    #[test]
    fn a_failure_shows_its_explanation_and_keeps_the_history() {
        let mut s = BrowserScreen::default();
        s.go("https://z.com/");
        s.loaded("https://z.com/", "https://z.com/", "<p>z</p>".into());
        s.go("https://a.com/");
        s.loaded("https://a.com/", "https://a.com/", "<p>a</p>".into());
        s.go("https://down.com/");
        s.failed(
            "https://down.com/",
            "connection failed",
            "<h2>This page cannot be shown</h2>".into(),
        );
        assert_eq!(s.status, "connection failed");
        assert_eq!(
            s.back(),
            Action::Navigate("https://z.com/".into()),
            "the failed page was never added to the history"
        );
    }

    #[test]
    fn editing_keys_work_and_escape_restores_the_address() {
        let mut s = BrowserScreen::default();
        s.go("https://a.com/");
        s.loaded("https://a.com/", "https://a.com/", String::new());
        s.on_key(Key::Char('/'));
        typed(&mut s, "xy");
        assert_eq!(s.address, "xy", "the first key replaces the old address");
        s.on_key(Key::Backspace);
        assert_eq!(s.address, "x");
        s.on_key(Key::Esc);
        assert_eq!((s.address.as_str(), s.editing()), ("https://a.com/", false));
        // Clicking the bar buttons and the field.
        assert_eq!(s.click_bar(10), Action::None);
        assert!(s.editing());
        s.on_key(Key::Esc);
        assert_eq!(s.click_bar(5), Action::Navigate("https://a.com/".into()));
        let mut long = BrowserScreen::default();
        long.on_key(Key::Char('l'));
        for _ in 0..(MAX_TYPED + 50) {
            long.on_key(Key::Char('a'));
        }
        assert_eq!(long.address.chars().count(), MAX_TYPED);
    }

    #[test]
    fn the_bar_and_the_text_fallback_draw() {
        let mut s = BrowserScreen::default();
        let mut g = Grid::new(60, 1);
        s.draw_bar(&mut g);
        s.go("https://a.com/");
        s.loaded(
            "https://a.com/",
            "https://a.com/",
            "<title>T</title>".into(),
        );
        let mut g = Grid::new(60, 1);
        s.draw_bar(&mut g);
        assert!(g.row_text(0).contains("https://a.com/"));
        let mut g = Grid::new(60, 8);
        let a = g.area();
        s.draw(&mut g, a);
        assert!(g.to_text().contains("T"));
        let mut tiny = Grid::new(6, 1);
        s.draw_bar(&mut tiny);
    }

    #[test]
    fn a_search_form_on_a_page_is_filled_in_and_sent_as_a_new_address() {
        let mut s = BrowserScreen::default();
        s.go("https://site.org/home");
        s.loaded(
            "https://site.org/home",
            "https://site.org/home",
            "<body style=\"margin:0\"><form action=\"/search\"><input name=q style=\"display:block;width:200px;height:30px\"><label><input type=checkbox name=safe checked></label><input type=submit value=Go style=\"display:block;height:30px\"></form></body>".into(),
        );
        // Lay the page out (a draw does this), then click in the text field and type.
        let mut c = Canvas {
            width: 400,
            height: 300,
            pixels: vec![0; 400 * 300],
        };
        s.draw_pixels(&mut c, (0, 0, 400, 300));
        assert_eq!(s.click(20.0, 15.0), Action::None);
        assert!(s.view.has_focus());
        for ch in "rust os".chars() {
            s.on_key(Key::Char(ch));
        }
        s.on_key(Key::Backspace);
        s.on_key(Key::Char('s'));
        assert_eq!(
            s.on_key(Key::Enter),
            Action::Navigate("https://site.org/search?q=rust+os&safe=on".into())
        );
        s.loaded(
            "https://site.org/search?q=rust+os&safe=on",
            "https://site.org/search?q=rust+os&safe=on",
            "<p>results</p>".into(),
        );
        assert!(!s.view.has_focus(), "a new page has no focused field");
        // Keys go to the bar again, and Escape leaves a field.
        s.on_key(Key::Char('l'));
        assert!(s.editing());
    }
}
