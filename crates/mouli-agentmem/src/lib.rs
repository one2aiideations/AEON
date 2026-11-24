//! Agent-memory primitives. Each module is self-contained and std-only.
//! Adapted from external sketches: linted, tested and fixed here; not load or hardware tested.
//!
//! The `xNNN` modules are the replay-checkpoint chain; the module list below gives each one's purpose.

/// Tenant- and principal-scoped access control for agent memory records.
pub mod access;
/// Bounded anti-entropy comparison primitives for replicated agent memories.
pub mod anti_entropy;
/// M030: incremental SQLite WAL commit-frame parser and bounded CDC fan-out.
pub mod cdc;
/// Deterministic, bounded snapshot codec for replication replay-window checkpoints.
pub mod checkpoint_codec;
/// Deterministic memory consolidation and retention planning.
pub mod consolidation;
/// Deterministic temporal scoring and expiry planning for agent memories.
pub mod decay;
/// Lease-based dispatch coordination for replication repair tasks.
pub mod dispatcher;
/// M022 — bounded, tenant-isolated HNSW vector memory with per-vector SQ8.
pub mod hnsw;
/// Bounded, append-only-in-process audit journal for agent memory operations.
pub mod journal;
/// M029: deterministic schema diff planner and guarded migration runner.
pub mod migrator;
/// M033 — Agent Capability Policy Engine.
pub mod policy;
/// Provenance and conflict-resolution primitives for agent memories.
pub mod provenance;
/// Bounded repair planning for divergent replicated memory inventories.
pub mod repair;
/// Tenant-scoped replication checkpoint tracking.
pub mod repl_checkpoint;
/// Bounded per-tenant memory change log for incremental replication.
pub mod repl_log;
/// Deterministic, tenant-scoped merge primitives for replicated memory changes.
pub mod repl_merge;
/// Bounded per-tenant/per-peer replay protection for replication sequence numbers.
pub mod replay_guard;
/// Bounded, deterministic scheduling for agent-memory replication repair work.
pub mod scheduler;
/// Versioned, bounded binary snapshot codec for agent memory records.
pub mod snapshot;
/// M026 — tenant-isolated time-series metrics engine.
pub mod timeseries;
/// Bounded binary wire protocol for AgentOS agent-memory replication.
pub mod wire;
/// Replication lag and freshness monitoring for AgentOS memory replicas.
pub mod x045;
/// Incremental bounded stream framing for the AgentOS replication wire protocol.
pub mod x050;
/// Stateful, tenant-bound replication protocol session coordinator.
pub mod x051;
/// Revisioned checkpoint store with validated, atomic in-memory replacement.
pub mod x054;
/// Deterministic diff and application for replay checkpoint snapshots.
pub mod x055;
/// Deterministic bounded binary codec for M055 replay checkpoint deltas.
pub mod x056;
/// End-to-end replay checkpoint delta exchange over the M053–M056 primitives.
pub mod x057;
/// Bounded, lease-based delivery tracking for replay checkpoint delta frames.
pub mod x058;
/// Durable append-only journal for replay delta delivery state transitions.
pub mod x059;
/// Deterministic semantic recovery for the M059 replay-delta delivery journal.
pub mod x060;
/// Canonical compaction for replay delta delivery journals.
pub mod x061;
/// Retention planning and safe event selection for replay delta delivery journals.
pub mod x062;
/// Applies a reviewed M062 retention plan by producing a compacted journal.
pub mod x063;
/// Independent validation of M062 delivery-retention plans.
pub mod x064;
/// Deterministic bounded binary codec for M062 delivery-retention plans.
pub mod x065;
/// Deterministic bounded checkpoint codec for M079 audit-report replay watermarks.
pub mod x080;
/// Durable atomic file storage for M080 audit-report replay checkpoints.
pub mod x081;
/// Restore policy validation for audit-report replay checkpoints.
pub mod x082;
/// Policy-checked, atomic in-memory restoration from a durable replay checkpoint.
pub mod x083;
/// Deterministic, bounded codec for non-secret checkpoint-keyring audit events.
pub mod x088;
/// Durable append-only journal for M088 checkpoint-keyring lifecycle audit events.
pub mod x089;
/// Read-only structural and semantic verifier for the authenticated-checkpoint
pub mod x090;
/// Deterministic bounded codec for M090 keyring-audit verification reports.
pub mod x091;
