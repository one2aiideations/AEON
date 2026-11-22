//! AEON-PROJECT: local-first project management.
//!
//! * [`model`]: projects, tasks, milestones, sprints, documents, resources,
//!   time entries, risks, audit trail, and the [`model::Store`] that keeps
//!   them consistent (validated statuses, same-project acyclic dependencies).
//! * [`schedule`]: critical-path method and kanban grouping.
//! * [`db`]: SQLite persistence (one JSON snapshot row, written atomically).
//! * [`api`]: local REST API (axum); every change is saved.
//! * [`ai`]: OpenAI-compatible assistant call with an explicit remote-consent gate.
//! * `gui` (feature `gui`): native desktop UI.
#![forbid(unsafe_code)]

pub mod ai;
pub mod api;
pub mod db;
pub mod model;
pub mod schedule;

#[cfg(feature = "gui")]
pub mod gui;

/// Application name.
pub const APP_NAME: &str = "AEON-PROJECT";

/// Errors from the project core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectError {
    /// Referenced item does not exist.
    NotFound(String),
    /// Input rejected.
    Invalid(String),
    /// Dependency would create a cycle.
    Cycle,
    /// Storage failure.
    Storage(String),
    /// AI call refused or failed.
    Ai(String),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::Invalid(s) => write!(f, "invalid: {s}"),
            Self::Cycle => f.write_str("circular dependency"),
            Self::Storage(s) => write!(f, "storage error: {s}"),
            Self::Ai(s) => write!(f, "AI error: {s}"),
        }
    }
}

impl std::error::Error for ProjectError {}
