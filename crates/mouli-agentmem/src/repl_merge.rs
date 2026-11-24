//! Deterministic, tenant-scoped merge primitives for replicated memory changes.
//!
//! The engine tracks per-origin sequence watermarks for idempotency and resolves
//! concurrent versions using a deterministic last-writer-wins ordering.
//! It is a merge policy, not a consensus protocol or durable replication store.

use std::collections::BTreeMap;

pub type TenantId = u64;
pub type ReplicaId = u64;
pub type MemoryId = u128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationKind {
    Upsert,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicatedMutation {
    pub tenant_id: TenantId,
    pub memory_id: MemoryId,
    pub origin_replica: ReplicaId,
    pub origin_sequence: u64,
    pub logical_timestamp_ns: u64,
    pub kind: MutationKind,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredVersion {
    pub tenant_id: TenantId,
    pub memory_id: MemoryId,
    pub origin_replica: ReplicaId,
    pub origin_sequence: u64,
    pub logical_timestamp_ns: u64,
    pub kind: MutationKind,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    Replaced,
    IgnoredStale,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeError {
    ZeroCapacity,
    PayloadTooLarge,
    InvalidSequence,
    CapacityReached,
}

#[derive(Clone, Debug)]
pub struct MergeConfig {
    pub max_records: usize,
    pub max_payload_bytes: usize,
    pub max_origins: usize,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            max_records: 100_000,
            max_payload_bytes: 1024 * 1024,
            max_origins: 10_000,
        }
    }
}

pub struct ReplicationMerge {
    config: MergeConfig,
    records: BTreeMap<(TenantId, MemoryId), StoredVersion>,
    watermarks: BTreeMap<(TenantId, ReplicaId), u64>,
}

impl ReplicationMerge {
    pub fn new(config: MergeConfig) -> Result<Self, MergeError> {
        if config.max_records == 0 || config.max_origins == 0 {
            return Err(MergeError::ZeroCapacity);
        }
        Ok(Self {
            config,
            records: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        })
    }

    /// Apply one mutation. Sequence numbers are monotonic per tenant and origin.
    /// A repeated or older origin sequence is treated as duplicate.
    pub fn apply(&mut self, mutation: ReplicatedMutation) -> Result<ApplyOutcome, MergeError> {
        if mutation.origin_sequence == 0 {
            return Err(MergeError::InvalidSequence);
        }
        if mutation.payload.len() > self.config.max_payload_bytes {
            return Err(MergeError::PayloadTooLarge);
        }

        let watermark_key = (mutation.tenant_id, mutation.origin_replica);
        let previous_watermark = self.watermarks.get(&watermark_key).copied().unwrap_or(0);
        if mutation.origin_sequence <= previous_watermark {
            return Ok(ApplyOutcome::Duplicate);
        }

        if !self.watermarks.contains_key(&watermark_key)
            && self.watermarks.len() >= self.config.max_origins
        {
            return Err(MergeError::CapacityReached);
        }

        let record_key = (mutation.tenant_id, mutation.memory_id);
        let existing = self.records.get(&record_key);

        let outcome = match existing {
            None => {
                if self.records.len() >= self.config.max_records {
                    return Err(MergeError::CapacityReached);
                }
                self.records
                    .insert(record_key, StoredVersion::from(mutation.clone()));
                ApplyOutcome::Applied
            }
            Some(current) if wins(&mutation, current) => {
                self.records
                    .insert(record_key, StoredVersion::from(mutation.clone()));
                ApplyOutcome::Replaced
            }
            Some(_) => ApplyOutcome::IgnoredStale,
        };

        // Advance only after the mutation has passed validation and capacity checks.
        self.watermarks
            .insert(watermark_key, mutation.origin_sequence);
        Ok(outcome)
    }

    pub fn get(&self, tenant_id: TenantId, memory_id: MemoryId) -> Option<&StoredVersion> {
        self.records.get(&(tenant_id, memory_id))
    }

    pub fn watermark(&self, tenant_id: TenantId, replica_id: ReplicaId) -> u64 {
        self.watermarks
            .get(&(tenant_id, replica_id))
            .copied()
            .unwrap_or(0)
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }
}

impl From<ReplicatedMutation> for StoredVersion {
    fn from(value: ReplicatedMutation) -> Self {
        Self {
            tenant_id: value.tenant_id,
            memory_id: value.memory_id,
            origin_replica: value.origin_replica,
            origin_sequence: value.origin_sequence,
            logical_timestamp_ns: value.logical_timestamp_ns,
            kind: value.kind,
            payload: value.payload,
        }
    }
}

/// Order by logical timestamp, then delete-wins, then replica ID and sequence.
/// The final tie-breaks ensure every peer chooses the same winner.
fn wins(candidate: &ReplicatedMutation, current: &StoredVersion) -> bool {
    candidate.logical_timestamp_ns > current.logical_timestamp_ns
        || (candidate.logical_timestamp_ns == current.logical_timestamp_ns
            && candidate.kind == MutationKind::Delete
            && current.kind != MutationKind::Delete)
        || (candidate.logical_timestamp_ns == current.logical_timestamp_ns
            && candidate.kind == current.kind
            && (candidate.origin_replica, candidate.origin_sequence)
                > (current.origin_replica, current.origin_sequence))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge() -> ReplicationMerge {
        ReplicationMerge::new(MergeConfig {
            max_records: 10,
            max_payload_bytes: 32,
            max_origins: 4,
        })
        .unwrap_or_else(|_| unreachable!())
    }

    fn mutation(
        tenant: u64,
        id: u128,
        replica: u64,
        seq: u64,
        ts: u64,
        kind: MutationKind,
        payload: &[u8],
    ) -> ReplicatedMutation {
        ReplicatedMutation {
            tenant_id: tenant,
            memory_id: id,
            origin_replica: replica,
            origin_sequence: seq,
            logical_timestamp_ns: ts,
            kind,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn applies_once_and_tracks_per_origin_watermark() {
        let mut engine = merge();
        let first = mutation(1, 10, 7, 1, 100, MutationKind::Upsert, b"v1");
        assert_eq!(engine.apply(first.clone()), Ok(ApplyOutcome::Applied));
        assert_eq!(engine.apply(first), Ok(ApplyOutcome::Duplicate));
        assert_eq!(engine.watermark(1, 7), 1);
    }

    #[test]
    fn resolves_conflicts_deterministically_with_delete_wins_ties() {
        let mut engine = merge();
        assert_eq!(
            engine.apply(mutation(1, 10, 1, 1, 50, MutationKind::Upsert, b"value")),
            Ok(ApplyOutcome::Applied)
        );
        assert_eq!(
            engine.apply(mutation(1, 10, 2, 1, 50, MutationKind::Delete, b"")),
            Ok(ApplyOutcome::Replaced)
        );
        assert_eq!(
            engine.get(1, 10).map(|v| v.kind),
            Some(MutationKind::Delete)
        );
    }

    #[test]
    fn isolates_tenants_and_rejects_invalid_or_oversized_changes() {
        let mut engine = merge();
        assert_eq!(
            engine.apply(mutation(1, 10, 1, 1, 1, MutationKind::Upsert, b"x")),
            Ok(ApplyOutcome::Applied)
        );
        assert!(engine.get(2, 10).is_none());
        assert_eq!(
            engine.apply(mutation(1, 11, 1, 0, 2, MutationKind::Upsert, b"x")),
            Err(MergeError::InvalidSequence)
        );
        assert_eq!(
            engine.apply(mutation(1, 12, 2, 1, 2, MutationKind::Upsert, &[0; 33])),
            Err(MergeError::PayloadTooLarge)
        );
    }
}
