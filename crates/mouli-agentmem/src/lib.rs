//! Agent-memory primitives. Each module is self-contained and std-only.
//! Adapted from external sketches: reviewed, linted and tested here; not hardware or load tested.
pub mod access;
pub mod anti_entropy;
pub mod cdc;
pub mod checkpoint_codec;
pub mod consolidation;
pub mod decay;
pub mod dispatcher;
pub mod hnsw;
pub mod journal;
pub mod migrator;
pub mod policy;
pub mod provenance;
pub mod repair;
pub mod repl_checkpoint;
pub mod repl_log;
pub mod repl_merge;
pub mod replay_guard;
pub mod scheduler;
pub mod snapshot;
pub mod timeseries;
pub mod wire;
