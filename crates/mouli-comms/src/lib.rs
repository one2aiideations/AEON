//! MOULI native communication.
//!
//! * [`crypto`]: per-pair session keys (X25519 + HKDF-SHA256) and
//!   authenticated encryption (ChaCha20-Poly1305) with replay protection.
//! * [`conversation`]: message ordering (Lamport clocks), de-duplication
//!   and delivery receipts.
//! * [`call`]: call signalling state machine for voice/video (media itself
//!   goes over the transport the call negotiates).
//!
//! This is a first version: one static key pair per device, no forward
//! secrecy ratchet yet. Do not treat it as audited cryptography.
#![forbid(unsafe_code)]

pub mod call;
pub mod conversation;
pub mod crypto;
