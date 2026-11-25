//! MOULI browser agent layer.
//!
//! This crate holds MOULI's own HTML and CSS rendering engine and what lets the assistant read and act on
//! pages safely. The second part
//! is:
//!
//! * [`html`]: tolerant HTML tokenizer.
//! * [`css`], [`style`], [`layout`]: a small rendering engine: stylesheets, the cascade, and block, inline,
//!   flex and table layout (no scripting yet).
//! * [`dom`]: the document tree built from it.
//! * [`readable`]: title and main text, skipping navigation and chrome.
//! * [`intents`]: actions a page offers (forms), classified and flagged
//!   when they need the user's consent.
//! * [`gate`]: dry-run by default; sensitive actions need explicit consent;
//!   secrets are never auto-filled.
//! * [`rate`]: per-host token bucket so agents stay polite.
#![forbid(unsafe_code)]

pub mod css;
pub mod display;
pub mod dom;
pub mod gate;
pub mod html;
pub mod intents;
pub mod layout;
pub mod markdown;
pub mod rate;
pub mod readable;
pub mod style;
