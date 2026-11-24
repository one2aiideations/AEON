//! AEON-BUSINESS operations engine: sales, procurement, inventory, manufacturing, HR and
//! payroll, projects and service, on SQLite with a tenant-scoped schema, a hash-chained audit
//! log, idempotent writes and an accounting hand-off trait.
//!
//! Money is integer minor units, quantities are integer thousandths, time is always passed in.
#![forbid(unsafe_code)]
// Store operations take the connection, context, identifiers and `now` explicitly.
#![allow(clippy::too_many_arguments)]
// Parameter structs are documented by their field names; see each module's header.
#![allow(missing_docs)]

pub mod admin_reports;
pub mod api;
pub mod common;
pub mod demo;
pub mod hr;
pub mod inventory;
pub mod manufacturing;
pub mod procurement;
pub mod projects_service;
pub mod sales;
pub mod schema;
pub mod selftest;
pub mod server;
