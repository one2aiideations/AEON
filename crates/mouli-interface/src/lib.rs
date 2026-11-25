//! MOULI main interface: the governed conversation workspace that every MOULI application and
//! agent goes through.
//!
//! Ported from the contracts and product scope of LibreOne (a Python stub package plus README),
//! rebuilt in Rust and tightened where the originals were wrong (shared default identifiers, an
//! approval that anyone could decide or reuse, string-prefix secret redaction only). What is here:
//!
//! - [`message`]: roles, messages and tenant-scoped sessions.
//! - [`intent`]: one line of input becomes chat or a capability call.
//! - [`capability`]: the single action interface applications implement.
//! - [`policy`]: roles, resource rules and the allow / approve / deny verdict.
//! - [`approval`]: four-eyes, single-use approvals.
//! - [`wash`]: secret, email and card detection with redaction and reversible pseudonyms.
//! - [`routing`]: local and hosted model choice by capability, privacy, context and cost.
//! - [`artifact`]: immutable, content-hashed artifact versions.
//! - [`audit`]: hash-chained audit log.
//! - [`workflow`]: state machine, limits and retry backoff.
//! - [`workspace`]: [`Workspace`], which ties them together.
//!
//! Not here: the UI itself (terminal, kiosk or web), running the model (see `mouli-llm`),
//! persistence, real identity and encryption (`mouli-identity`), and document format converters.
//! Nothing here has been used by a real application yet.

pub mod approval;
pub mod artifact;
pub mod audit;
pub mod capability;
pub mod error;
pub mod ids;
pub mod intent;
pub mod message;
pub mod policy;
pub mod routing;
pub mod wash;
pub mod workflow;
pub mod workspace;

pub use capability::{Application, Call, CapabilitySpec, Output, Resource, Risk};
pub use error::{Error, Result};
pub use policy::{Policy, Role as Permissions, Verdict};
pub use workspace::{PendingCall, Reply, Workspace};
