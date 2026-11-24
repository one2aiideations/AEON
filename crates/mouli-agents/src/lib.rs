//! MOULI scheduled agents.
//!
//! Users and the system register agent jobs that run once, every N
//! seconds, or daily at a time. The [`scheduler::Scheduler`] hands out due
//! jobs; [`runner::Runner`] executes them through a caller-supplied
//! executor and writes every outcome to the hash-chained [`audit::AuditLog`].
//!
//! Time is passed in explicitly (Unix seconds, UTC) so the logic is
//! deterministic and testable; the OS service feeds it the real clock.
#![forbid(unsafe_code)]

pub mod audit;
pub mod runner;
pub mod schedule;
pub mod scheduler;

pub use schedule::Schedule;
