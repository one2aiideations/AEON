//! MOULI multi-agent orchestration.
//!
//! Autonomous agents are only useful on a personal machine if they are
//! bounded. This crate is the control plane that keeps them bounded:
//!
//! * [`registry`]: agents, their capabilities and a trust score learned
//!   from outcomes.
//! * [`select`]: choose a model or agent for a capability given urgency,
//!   device, cost and risk, with fallback chains.
//! * [`consensus`]: trust-weighted voting between agent proposals.
//! * [`governance`]: constitution rules (allow / deny / needs approval),
//!   spending budgets and rate quotas.
//! * [`resilience`]: circuit breakers, retry backoff, a leased job queue
//!   and leader leases.
//! * [`evolve`]: self-repair that only accepts a change when it passes
//!   validation and does not regress, with rollback history.
//! * [`orchestrator`]: ties the pieces together for one objective.
//!
//! Time is passed in explicitly (milliseconds or seconds as documented) so
//! every policy is deterministic and testable.
#![forbid(unsafe_code)]

pub mod consensus;
pub mod evolve;
pub mod governance;
pub mod orchestrator;
pub mod registry;
pub mod resilience;
pub mod select;
