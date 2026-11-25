//! Renders a sample page (or an HTML file given as the first argument) to a PNG, to see what the
//! web view looks like: `cargo run -p mouli-screen --example web_demo -- page.html out.png`.

use mouli_screen::raster::{to_png, Canvas};
use mouli_screen::webview::Page;

const SAMPLE: &str = r##"<!doctype html><html><head><title>MOULI preview</title><style>
  :root { --brand: #1565c0; --soft: #e3f2fd; }
  body { font-family: sans-serif; margin: 0; background: #fafafa; color: #212121; }
  header { background: linear-gradient(135deg, var(--brand), #42a5f5); color: white; padding: 18px 24px; }
  header h1 { margin: 0; font-size: 26px; }
  header p { margin: 4px 0 0; opacity: .85; }
  nav { padding: 8px 24px; background: white; border-bottom: 1px solid #ddd; }
  nav a { margin-right: 16px; color: var(--brand); }
  .cols { display: flex; gap: 16px; padding: 16px 24px; }
  .card { flex: 1; background: white; border: 1px solid #e0e0e0; border-radius: 8px; padding: 14px; }
  .card h2 { margin: 0 0 8px; font-size: 18px; color: var(--brand); }
  .tag { display: inline-block; background: var(--soft); color: var(--brand); border-radius: 10px; padding: 1px 8px; font-size: 12px; }
  table { border-collapse: collapse; margin: 0 24px 16px; }
  td, th { border: 1px solid #ccc; padding: 4px 10px; }
  th { background: #eee; }
  code { background: #eee; padding: 0 4px; border-radius: 3px; }
  .btn { background: var(--brand); color: white; border: 0; border-radius: 4px; padding: 6px 14px; }
  footer { text-align: center; color: #777; padding: 12px; font-size: 13px; }
</style></head><body>
<header><h1>Hello from MOULI</h1><p>HTML and CSS rendered by our own engine</p></header>
<nav><a href="#a">Home</a><a href="#b">Docs</a><a href="#c">About</a></nav>
<div class="cols">
  <div class="card"><h2>Layout</h2><p>Flexbox, <b>bold</b>, <i>italic</i> and <a href="#x">links</a>. <span class="tag">new</span></p>
    <ul><li>Blocks and inline text</li><li>Tables and lists</li><li>Gradients &amp; borders</li></ul></div>
  <div class="card"><h2>Code</h2><p>Use <code>display:flex</code> with a gap.</p><p><button class="btn">Run</button> <input value="text field" size="12"></p></div>
</div>
<table><tr><th>Feature</th><th>Status</th></tr><tr><td>HTML</td><td>works</td></tr><tr><td>CSS</td><td>works</td></tr><tr><td>JavaScript</td><td>next</td></tr></table>
<footer>MOULI preview</footer></body></html>"##;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let html = args.get(1).map_or_else(
        || SAMPLE.to_string(),
        |p| std::fs::read_to_string(p).expect("read the HTML file"),
    );
    let out = args.get(2).map_or("web_demo.png", String::as_str);
    let (w, h) = (820usize, 640usize);
    let page = Page::load(&html, w as f32, h as f32);
    let mut c = Canvas {
        width: w,
        height: h,
        pixels: vec![0x00FF_FFFF; w * h],
    };
    page.draw(&mut c, 0, 0, w as i32, h as i32, 0.0);
    std::fs::write(out, to_png(&c)).expect("write the PNG");
    println!(
        "wrote {out}: page height {:.0}px, title {:?}",
        page.height(),
        page.title()
    );
}
