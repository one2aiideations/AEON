//! MOULI-DESK: a classic desktop for the MOULI device, in the style of Mac OS on PowerPC.
//!
//! It draws windows with title bars, a menu bar, icons and a mouse pointer into a pixel canvas,
//! in software, with no windowing system and no GPU. The six MOULI screens run unchanged inside
//! the windows. Mouse input comes in as [`input::MouseEvent`] values decoded from the kernel's
//! mouse packets; the decoders are plain functions over bytes and are tested without hardware.
#![forbid(unsafe_code)]

pub mod art;
pub mod desktop;
pub mod geom;
pub mod input;
pub mod menu;
pub mod paint;
mod render;
pub mod theme;
pub mod wm;
