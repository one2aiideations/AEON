//! MOULI browser agent layer.
//!
//! The MOULI browser embeds an existing web engine for rendering. This crate
//! is what lets the assistant read and act on pages safely:
//!
//! * [`html`]: tolerant HTML tokenizer (no DOM, no scripts).
//! * [`readable`]: title and main text, skipping navigation and chrome.
//! * [`intents`]: actions a page offers (forms), classified and flagged
//!   when they need the user's consent.
//! * [`gate`]: dry-run by default; sensitive actions need explicit consent;
//!   secrets are never auto-filled.
//! * [`rate`]: per-host token bucket so agents stay polite.
#![forbid(unsafe_code)]

pub mod gate;
pub mod html;
pub mod intents;
pub mod rate;
pub mod readable;
