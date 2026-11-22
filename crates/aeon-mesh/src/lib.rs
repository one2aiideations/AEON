//! AEON device mesh.
//!
//! A user's devices (laptop, phone, desktop) form a small mesh for sharing
//! files and offloading work.
//!
//! * [`nodes`]: membership by heartbeat, capability and load.
//! * [`dispatch`]: task assignment with checkpoints and reassignment when a
//!   node disappears.
//! * [`files`]: content-addressed chunking, replica placement and version
//!   vectors that detect concurrent edits instead of silently dropping one.
//!
//! Transport is out of scope here; these types are the state machines the
//! network layer drives. Time is passed in (seconds) for deterministic tests.
#![forbid(unsafe_code)]

pub mod dispatch;
pub mod files;
pub mod nodes;
