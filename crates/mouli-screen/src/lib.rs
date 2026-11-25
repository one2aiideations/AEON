//! MOULI-SCREEN: the on-device screens.
//!
//! A screen is a plain Rust state machine: it reacts to [`key::Key`] presses and draws itself into
//! a [`grid::Grid`] of character cells. Because the grid is just data, the same screen can be
//!
//! * drawn to a framebuffer by [`raster`] (pixels, no GPU, no windowing system),
//! * saved as a PNG for review ([`raster::to_png`]),
//! * written out as text, which is what the tests assert on.
//!
//! Nothing here depends on another crate. Real data (models, messages, approvals, audit records,
//! security findings, patches) reaches the screens as plain view-model structs, so the screens
//! can be built and tested without the rest of MOULI.
#![forbid(unsafe_code)]

pub mod font;
mod font_data;
pub mod grid;
pub mod key;
pub mod raster;
pub mod screen;
pub mod screens;
pub mod shell;
pub mod syntax;
pub mod typeface;
