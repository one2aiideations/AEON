//! MOULI-PLAYER: the core of the video and YouTube player.
//!
//! * [`source`]: what can be played and how to read an address safely.
//! * [`queue`]: the play queue with shuffle, repeat and history.
//! * [`engine`]: the player state machine, including hands-free Shorts autoplay.
//! * [`mpv`]: the control protocol of the `mpv` media engine, for local files and direct URLs.
//!
//! YouTube items are never fetched or decoded here. They are only ever played by YouTube's own
//! web player (with its ads and rules), and mpv is started with its YouTube extraction hook
//! switched off, so nothing in this crate downloads or re-streams YouTube video.
//!
//! Nothing here opens a window or touches a sound card; the backends that do are added by the
//! OS image. Everything in this crate is testable without a display.
#![forbid(unsafe_code)]

pub mod engine;
pub mod mpv;
pub mod queue;
pub mod source;

pub use engine::{Backend, Command, Event, Feedback, Player, State};
pub use queue::{Item, Queue, Repeat};
pub use source::{parse, ParseError, Source, YtKind};
