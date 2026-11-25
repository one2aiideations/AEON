//! MOULI's own web engine, and what lets the assistant read and act on pages safely.
//!
//! Everything here is written from scratch in safe Rust, with no third-party crates. A page goes
//! through these stages:
//!
//! * [`html`], [`dom`]: a tolerant tokenizer and the document tree (with editing for scripts).
//! * [`css`], [`style`]: stylesheets, selectors, media queries, the cascade and inheritance.
//! * [`layout`], [`display`]: block, inline, flex and table layout, then a list of paint
//!   operations for the desktop to rasterise (floats, grid and images are not done yet).
//! * [`js`], [`interp`], [`jslib`]: a JavaScript lexer and parser, a tree-walking interpreter
//!   with hard limits, and the standard library.
//! * [`scripting`]: `document`, elements, events and timers, so page scripts can change the page.
//! * [`markdown`]: Markdown to HTML (all raw HTML is escaped).
//!
//! For the assistant:
//!
//! * [`readable`]: title and main text, skipping navigation and chrome.
//! * [`intents`]: actions a page offers (forms), classified and flagged when they need consent.
//! * [`gate`]: dry-run by default; sensitive actions need explicit consent; secrets are never
//!   auto-filled.
//! * [`rate`]: per-host token bucket so agents stay polite.
#![forbid(unsafe_code)]

pub mod css;
pub mod display;
pub mod dom;
pub mod gate;
pub mod html;
pub mod intents;
pub mod interp;
pub mod js;
pub mod jslib;
pub mod layout;
pub mod markdown;
pub mod rate;
pub mod readable;
pub mod scripting;
pub mod style;
