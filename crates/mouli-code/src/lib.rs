//! MOULI-CODE core: the mechanical, verifiable parts of an AI code-repair loop.
//!
//! * [`diff`]: unified-diff types, strict parsing, rendering and reversal.
//! * [`apply`]: exact in-memory application (context verified line by line), atomic multi-file
//!   patch sets, and path safety.
//! * [`make`]: generating a unified diff from two texts.
//! * [`quality`]: patch statistics, test-file detection, hard-coding heuristics and a risk score.
//! * [`minimize`]: removing hunks that the tests do not need (delta debugging).
//! * [`budget`]: hard limits on iterations, model calls and repair attempts.
//! * [`workspace`]: applying a checked patch to a real directory atomically, and undoing it
//!   exactly.
//! * [`repair`]: the repair loop: propose, check, try in a copy, feed back the failure, within
//!   budgets, then shrink the fix with the minimiser.
//! * [`repo`]: a map of what a repository defines and which files refer to which, and a ranking of
//!   the files that matter for a question.
//! * [`sandbox`]: running a command (the tests) in a throwaway copy of a folder, with a time limit
//!   and an output cap. Not a security boundary: see its notes.
//!
//! Everything except [`workspace`], [`sandbox`] and [`repair`] works on text held in memory and never touches
//! the file system or runs a command, so an agent's proposed change can be checked completely
//! before anything is written.
//! The design follows an earlier Python prototype whose diff application, minimiser and quality
//! checks turned out to be placeholders (for example `basic_diff_apply` returned its input
//! unchanged and the real apply used `git apply --unsafe-paths`, which disables git's path
//! check); this is a working implementation of what it intended.
#![forbid(unsafe_code)]

pub mod apply;
pub mod budget;
pub mod diff;
pub mod make;
pub mod minimize;
pub mod quality;
pub mod repair;
pub mod repo;
pub mod sandbox;
pub mod workspace;
