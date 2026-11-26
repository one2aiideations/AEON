//! Robustness tests: pages, style sheets, scripts, pictures, regular expressions and addresses
//! are mutated at random (the same sequence every run) and fed to the engine. The only thing
//! checked is that nothing panics, hangs or blows its limits; hostile input must end in an error
//! or a picture, never a crash.

use crate::browser;
use crate::css::{State, Stylesheet};
use crate::display::paint;
use crate::dom::Dom;
use crate::image;
use crate::layout::{layout, FontSpec, Fonts, Rect};
use crate::regex::Regex;
use crate::scripting::Scripting;
use crate::style::{compute, Viewport};

/// How many times to repeat each round: 1 normally, more with `MOULI_FUZZ=20` for a longer hunt.
fn scale() -> usize {
    std::env::var("MOULI_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=1000).contains(n))
        .unwrap_or(1)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const POOL: [&str; 40] = [
    "<",
    ">",
    "</",
    "/>",
    "<div>",
    "</div>",
    "<p",
    "<script>",
    "</script>",
    "<style>",
    "</style>",
    "<table>",
    "<td>",
    "<img src=",
    "<a href=\"",
    "\"",
    "'",
    "`",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ";",
    ":",
    "&amp;",
    "&#",
    "!important",
    "calc(",
    "var(--x)",
    "@media",
    "url(",
    "float:left",
    "display:grid",
    "grid-template-columns:",
    "repeat(",
    "\\",
    "\u{0}",
    "é\u{1F600}",
];

fn mutate(rng: &mut Rng, seed: &str) -> String {
    let mut chars: Vec<char> = seed.chars().collect();
    for _ in 0..1 + rng.below(6) {
        let at = rng.below(chars.len() + 1);
        match rng.below(4) {
            0 => {
                let piece: Vec<char> = POOL[rng.below(POOL.len())].chars().collect();
                chars.splice(at..at, piece);
            }
            1 => {
                let end = (at + rng.below(30)).min(chars.len());
                chars.drain(at.min(end)..end);
            }
            2 => {
                let end = (at + rng.below(60)).min(chars.len());
                let dup: Vec<char> = chars[at.min(end)..end].to_vec();
                let to = rng.below(chars.len() + 1);
                chars.splice(to..to, dup);
            }
            _ => {
                if !chars.is_empty() {
                    let k = rng.below(chars.len());
                    chars[k] = char::from_u32(rng.next() as u32 % 0x250).unwrap_or('?');
                }
            }
        }
        if chars.len() > 6000 {
            chars.truncate(6000);
        }
    }
    chars.into_iter().collect()
}

struct F;

impl Fonts for F {
    fn text_width(&self, text: &str, f: &FontSpec) -> f32 {
        text.chars().count() as f32 * f.size / 2.0
    }
    fn ascent(&self, f: &FontSpec) -> f32 {
        f.size * 0.8
    }
    fn descent(&self, f: &FontSpec) -> f32 {
        f.size * 0.2
    }
}

const PAGE: &str = r#"<!doctype html><html><head><title>t</title><style>
body{margin:8px;font:14px sans-serif} .g{display:grid;grid-template-columns:repeat(auto-fill,minmax(80px,1fr));gap:4px}
.f{float:left;width:40%;margin:0 8px 4px 0} .c{display:flex;flex-wrap:wrap;gap:2px} table{border-collapse:collapse} td{border:1px solid #888;padding:2px}
@media (max-width:300px){.g{display:block}} a:hover{color:red} p::first-line{color:blue} .x{position:absolute;top:10px;left:calc(50% - 5px)}
</style></head><body><h1>Title &amp; more</h1><div class=g><div>a</div><div style="grid-column:1/-1">b</div></div>
<img class=f src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" width=40 height=20><p>Some text <b>bold</b> <i>italic</i> and a <a href="/next?q=1#top">link</a> that wraps around the float.</p>
<form action="/s"><input name=q value="x"><select name=s><option>1<option selected>2</select><textarea name=t>hi</textarea><button>Go</button></form>
<table><tr><th>h<td colspan=2>c<tr><td>1<td>2<td>3</table><ul><li>one<li>two<ol><li>nested</ol></ul>
<div class=c><span>f1</span><span>f2</span></div><div class=x>abs</div>
<script>
const g = document.querySelector('.g'); const d = document.createElement('div'); d.textContent = [1,2,3].map(n => n * n).join('-');
g.appendChild(d); document.title = 'x'.repeat(3) + new Date(0).toISOString(); /(\d+)-(\d+)/.exec('12-34');
setTimeout(() => { d.style.color = 'red'; d.classList.add('k'); }, 5); Promise.resolve(1).then(v => d.setAttribute('data-v', v));
</script></body></html>"#;

const JS: &str = r#"class A { #x = 1; constructor(n) { this.n = n } get v() { return this.n * 2 } static s() { return 'S' } }
class B extends A { constructor() { super(3); this.m = [..."ab", ...[1, 2]] } toString() { return `B(${this.v})` } }
const {a, b: [c, ...d], ...rest} = {a: 1, b: [2, 3, 4], e: 5, f: 6}; let s = 0;
outer: for (let i = 0; i < 5; i++) { for (const k in {x: 1, y: 2}) { if (i == 3) break outer; s += i; } }
try { null.x } catch ({message}) { s += message.length } finally { s++ }
const m = new Map([[1, 'a']]); const st = new Set([1, 2, 2]); const re = /a(?<n>b+)?c|[^x-z]\b/gi;
async function f(x) { const v = await Promise.all([1, Promise.resolve(2)]); return v.reduce((p, q) => p + q, x) }
f(1).then(v => s += v); [3, 1, 2].sort((p, q) => p - q).map(String).join(); JSON.parse(JSON.stringify({a: [1, {b: 2}], c: 'x'}));
'a-b_c'.replace(/[-_](.)/g, (_, ch) => ch.toUpperCase()) + new B() + (s ?? 0) + (typeof undefined) + (1 << 3 >>> 1) + 2 ** 3;"#;

const CSS: &str = "@media (min-width:100px) and (max-width:900px){.a>.b + .c ~ .d:not(.e):nth-child(2n+1)::before{content:'x';margin:calc(1px + 2em) -3px}}\n:root{--c:#abc} .x{color:var(--c);background:linear-gradient(90deg,red 0%,blue 100%);border:1px solid rgba(0,0,0,.5);font:italic bold 12px/1.5 serif;grid-template-columns:repeat(3,1fr) minmax(10px,2fr);transform:rotate(3deg)}";

fn viewport() -> Viewport {
    Viewport { w: 400.0, h: 300.0 }
}

fn run_page(html: &str) {
    let dom = Dom::parse(html);
    let sheets: Vec<Stylesheet> = dom
        .style_sheets()
        .iter()
        .map(|s| Stylesheet::parse(s))
        .collect();
    let styles = compute(&dom, &sheets, viewport(), &State::default());
    let l = layout(&dom, &styles, &F, viewport());
    assert!(l.height.is_finite() || l.height.is_nan() || l.height.is_infinite());
    let _ = paint(
        &l,
        0.0,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        },
    );
    let _ = dom.inner_html(0).len();
    let _ = browser::subresources(html, "https://a.com/x");
    let _ = browser::image_refs(html, "https://a.com/x");
    let _ = browser::reader_html(html);
}

fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn mutated_pages_never_crash_the_engine() {
    big_stack(|| {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..120 * scale() {
            let html = mutate(&mut rng, PAGE);
            run_page(&html);
        }
        // Whole pages of nothing but markup debris.
        for _ in 0..40 * scale() {
            let n = 50 + rng.below(400);
            let html: String = (0..n).map(|_| POOL[rng.below(POOL.len())]).collect();
            run_page(&html);
        }
    });
}

#[test]
fn mutated_scripts_and_scripted_pages_stay_inside_their_limits() {
    big_stack(|| {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        for _ in 0..150 * scale() {
            let src = mutate(&mut rng, JS);
            let mut i = crate::interp::Interp::new();
            i.step_limit = 30_000;
            i.object_limit = 20_000;
            let _ = i.run(&src);
            i.advance(1000.0);
        }
        for _ in 0..25 * scale() {
            let html = mutate(&mut rng, PAGE);
            let mut s = Scripting::with_budget(&html, 20_000);
            let _ = s.take_dirty();
            s.advance(100.0);
            let n = s.snapshot().len();
            assert!(n > 0);
            let _ = s.dispatch(0, "click");
            let _ = s.eval("document.body.innerHTML.length");
        }
    });
}

#[test]
fn mutated_style_sheets_regular_expressions_and_addresses_are_harmless() {
    big_stack(|| {
        let mut rng = Rng(0xdead_beef_cafe_f00d);
        for _ in 0..300 * scale() {
            let css = mutate(&mut rng, CSS);
            let sheet = Stylesheet::parse(&css);
            let _ = sheet;
            let _ = crate::css::parse_declarations(&css);
            let _ = crate::css::parse_selector(&css);
            let _ = crate::css::parse_color(&css);
            let _ = crate::css::parse_len(&css);
        }
        for _ in 0..400 * scale() {
            let p = mutate(&mut rng, "(?<y>\\d{4})-(a|b)+?[^\\]x-z]*\\1(?=q)(?!r)\\b$");
            if let Ok(re) = Regex::new(&p, "gi") {
                let text: Vec<char> = "2024-abab q rrr\n\u{1F600}x".chars().collect();
                let _ = re.exec(&text, 0);
            }
            let u = mutate(
                &mut rng,
                "https://user@Example.COM:8080/a/../b/./c?x=1&y=%zz#frag",
            );
            let _ = browser::Address::parse(&u);
            let _ = browser::resolve("https://a.com/dir/page.html?q=1", &u);
            let _ = browser::from_typed(&u);
            let _ = browser::is_local_host(&u);
            let _ = browser::decode_data_uri(&u);
            let _ = browser::base64_decode(&u);
        }
    });
}

#[test]
fn random_bytes_and_cut_or_flipped_pictures_never_crash_the_decoders() {
    big_stack(|| {
        let mut rng = Rng(0x0bad_c0de_1234_5678);
        // Random bytes behind each format's signature, so the decoders get past the first check.
        for sig in [
            &[0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..],
            b"GIF89a",
            &[0xFF, 0xD8, 0xFF][..],
            b"",
        ] {
            for _ in 0..150 * scale() {
                let n = rng.below(400);
                let mut bytes = sig.to_vec();
                bytes.extend((0..n).map(|_| rng.next() as u8));
                let _ = image::decode(&bytes);
            }
        }
        // Real pictures with bytes flipped and with pieces cut out.
        let samples: Vec<Vec<u8>> = crate::image_fixtures_for_fuzz();
        for s in &samples {
            for _ in 0..60 * scale() {
                let mut b = s.clone();
                for _ in 0..1 + rng.below(4) {
                    let k = rng.below(b.len());
                    b[k] = rng.next() as u8;
                }
                if rng.below(3) == 0 {
                    let cut = rng.below(b.len());
                    b.truncate(cut);
                }
                let _ = image::decode(&b);
            }
        }
    });
}
