//! MOULI's own web engine, and what lets the assistant read and act on pages safely.
//!
//! Everything here is written from scratch in safe Rust, with no third-party crates. A page goes
//! through these stages:
//!
//! * [`html`], [`dom`]: a tolerant tokenizer and the document tree (with editing for scripts).
//! * [`css`], [`style`]: stylesheets, selectors, media queries, the cascade and inheritance.
//! * [`layout`], [`display`]: block, inline, flex and table layout, then a list of paint
//!   operations for the desktop to rasterise (floats, grid and images are not done yet).
//! * [`regex`]: the regular-expression matcher behind JavaScript's `RegExp`.
//! * [`image`]: PNG, JPEG (baseline and progressive) and GIF decoders.
//! * [`js`], [`interp`], [`jslib`]: a JavaScript lexer and parser, a tree-walking interpreter
//!   with hard limits, and the standard library.
//! * [`scripting`]: `document`, elements, events and timers, so page scripts can change the page.
//! * [`markdown`]: Markdown to HTML (all raw HTML is escaped).
//!
//! * [`browser`]: web addresses, history, and fetching what a page needs (no I/O of its own).
//!
//! For the assistant:
//!
//! * [`readable`]: title and main text, skipping navigation and chrome.
//! * [`intents`]: actions a page offers (forms), classified and flagged when they need consent.
//! * [`gate`]: dry-run by default; sensitive actions need explicit consent; secrets are never
//!   auto-filled.
//! * [`rate`]: per-host token bucket so agents stay polite.
#![forbid(unsafe_code)]

pub mod browser;
pub mod css;
pub mod display;
pub mod dom;
#[cfg(test)]
mod fuzz;
pub mod gate;
pub mod html;
pub mod image;
#[cfg(test)]
mod image_fixtures;

/// The test pictures as bytes (for the robustness tests).
#[cfg(test)]
fn image_fixtures_for_fuzz() -> Vec<Vec<u8>> {
    use image_fixtures as fx;
    let unhex = |s: &str| -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap_or(0))
            .collect()
    };
    [
        fx::PNG_RGB,
        fx::PNG_RGBA,
        fx::PNG_PAL,
        fx::JPG_RGB,
        fx::JPG_420,
        fx::JPG_PROG,
        fx::JPG_PROG444,
        fx::GIF,
    ]
    .iter()
    .map(|h| unhex(h))
    .collect()
}
pub mod intents;
pub mod interp;
pub mod js;
pub mod jslib;
pub mod layout;
pub mod markdown;
pub mod rate;
pub mod readable;
pub mod regex;
pub mod scripting;
pub mod style;
