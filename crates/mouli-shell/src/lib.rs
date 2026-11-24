//! MOULI-SHELL: the device layer under the screens in `mouli-screen`.
//!
//! * [`fb`]: reading a framebuffer's geometry from sysfs and writing pixels to `/dev/fb0` with
//!   plain file writes (no memory mapping, so no `unsafe`), sending only the rows that changed.
//! * [`console`]: stopping the text console from drawing over the screen, and putting it back.
//! * [`tty`]: raw keyboard input from the terminal, as keys.
//! * [`runner`]: the loop that draws, waits for keys and passes what the person chose back out.
//!
//! The loop is generic over a display and a key source, so it is tested here without any
//! hardware; the real framebuffer and keyboard are checked on a pseudo-terminal and a plain file.
#![forbid(unsafe_code)]

pub mod console;
pub mod fb;
pub mod runner;
#[cfg(target_os = "linux")]
pub mod tty;
