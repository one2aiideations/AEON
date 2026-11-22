//! AEON adapters to sibling systems.
//!
//! Each adapter is AEON's own view of the other system's messages. None of
//! them claims wire or ABI compatibility until a wire profile is written and
//! tested against the real peer.
//!
//! * [`topstar`]: validation of TOPSTAR protocol messages.
#![forbid(unsafe_code)]

pub mod topstar;
