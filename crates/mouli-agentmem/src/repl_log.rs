//! Bounded per-tenant memory change log for incremental replication.
//!
//! This module produces ordered change batches and detects cursors that have
//! fallen behind retained history. Transport, durable persistence, peer
//! authentication, conflict resolution, and applying changes are external.

use std::collections::{BTreeMap, VecDeque};

pub type TenantId = u64;
pub type MemoryId = u128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Upsert,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryChange {
    pub tenant_id: TenantId,
    pub sequence: u64,
    pub memory_id: MemoryId,
    pub kind: ChangeKind,
    /// Optional opaque payload; callers define its schema and protection.
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeBatch {
    pub tenant_id: TenantId,
    pub requested_after: u64,
    pub next_cursor: u64,
    pub has_more: bool,
    pub changes: Vec<MemoryChange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplicationError {
    ZeroCapacity,
    PayloadTooLarge,
    SequenceExhausted,
    CursorExpired { oldest_available: u64 },
    ZeroBatchLimit,
}

#[derive(Clone, Debug)]
pub struct ReplicationConfig {
    pub max_events_per_tenant: usize,
    pub max_payload_bytes: usize,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            max_events_per_tenant: 100_000,
            max_payload_bytes: 1024 * 1024,
        }
    }
}

#[derive(Default)]
struct TenantLog {
    events: VecDeque<MemoryChange>,
    next_sequence: u64,
}

pub struct ReplicationLog {
    config: ReplicationConfig,
    tenants: BTreeMap<TenantId, TenantLog>,
}

impl ReplicationLog {
    pub fn new(config: ReplicationConfig) -> Result<Self, ReplicationError> {
        if config.max_events_per_tenant == 0 {
            return Err(ReplicationError::ZeroCapacity);
        }
        Ok(Self {
            config,
            tenants: BTreeMap::new(),
        })
    }

    pub fn append(
        &mut self,
        tenant_id: TenantId,
        memory_id: MemoryId,
        kind: ChangeKind,
        payload: Vec<u8>,
    ) -> Result<u64, ReplicationError> {
        if payload.len() > self.config.max_payload_bytes {
            return Err(ReplicationError::PayloadTooLarge);
        }

        let log = self.tenants.entry(tenant_id).or_default();
        let sequence = if log.next_sequence == 0 {
            1
        } else {
            log.next_sequence
        };
        let next = sequence
            .checked_add(1)
            .ok_or(ReplicationError::SequenceExhausted)?;

        if log.events.len() == self.config.max_events_per_tenant {
            log.events.pop_front();
        }
        log.events.push_back(MemoryChange {
            tenant_id,
            sequence,
            memory_id,
            kind,
            payload,
        });
        log.next_sequence = next;
        Ok(sequence)
    }

    /// Read at most `limit` events strictly after `cursor`.
    /// A cursor before the retained history is rejected rather than silently
    /// returning an incomplete replication stream.
    pub fn read_after(
        &self,
        tenant_id: TenantId,
        cursor: u64,
        limit: usize,
    ) -> Result<ChangeBatch, ReplicationError> {
        if limit == 0 {
            return Err(ReplicationError::ZeroBatchLimit);
        }
        let Some(log) = self.tenants.get(&tenant_id) else {
            return Ok(ChangeBatch {
                tenant_id,
                requested_after: cursor,
                next_cursor: cursor,
                has_more: false,
                changes: Vec::new(),
            });
        };

        if let Some(oldest) = log.events.front() {
            if cursor.saturating_add(1) < oldest.sequence {
                return Err(ReplicationError::CursorExpired {
                    oldest_available: oldest.sequence,
                });
            }
        }

        let mut changes = Vec::new();
        let mut has_more = false;
        for event in &log.events {
            if event.sequence <= cursor {
                continue;
            }
            if changes.len() == limit {
                has_more = true;
                break;
            }
            changes.push(event.clone());
        }

        let next_cursor = changes.last().map(|event| event.sequence).unwrap_or(cursor);

        Ok(ChangeBatch {
            tenant_id,
            requested_after: cursor,
            next_cursor,
            has_more,
            changes,
        })
    }

    pub fn latest_sequence(&self, tenant_id: TenantId) -> u64 {
        self.tenants
            .get(&tenant_id)
            .map(|log| log.next_sequence.saturating_sub(1))
            .unwrap_or(0)
    }

    pub fn retained_count(&self, tenant_id: TenantId) -> usize {
        self.tenants
            .get(&tenant_id)
            .map(|log| log.events.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(capacity: usize) -> ReplicationLog {
        ReplicationLog::new(ReplicationConfig {
            max_events_per_tenant: capacity,
            max_payload_bytes: 64,
        })
        .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn emits_ordered_incremental_batches() {
        let mut log = log(8);
        assert_eq!(log.append(1, 10, ChangeKind::Upsert, vec![1]), Ok(1));
        assert_eq!(log.append(1, 11, ChangeKind::Delete, Vec::new()), Ok(2));
        let batch = log.read_after(1, 0, 1);
        assert!(batch.is_ok());
        let batch = batch.unwrap_or(ChangeBatch {
            tenant_id: 0,
            requested_after: 0,
            next_cursor: 0,
            has_more: false,
            changes: Vec::new(),
        });
        assert_eq!(batch.next_cursor, 1);
        assert!(batch.has_more);
    }

    #[test]
    fn tenant_logs_are_isolated_and_expired_cursors_are_detected() {
        let mut log = log(2);
        assert_eq!(log.append(7, 1, ChangeKind::Upsert, vec![]), Ok(1));
        assert_eq!(log.append(7, 2, ChangeKind::Upsert, vec![]), Ok(2));
        assert_eq!(log.append(7, 3, ChangeKind::Upsert, vec![]), Ok(3));
        assert_eq!(log.latest_sequence(8), 0);
        assert_eq!(
            log.read_after(7, 0, 10),
            Err(ReplicationError::CursorExpired {
                oldest_available: 2
            })
        );
        assert_eq!(log.read_after(8, 0, 10).map(|b| b.changes.len()), Ok(0));
    }

    #[test]
    fn rejects_oversized_payload_and_zero_batch_limit() {
        let mut log = log(2);
        assert_eq!(
            log.append(1, 1, ChangeKind::Upsert, vec![0; 65]),
            Err(ReplicationError::PayloadTooLarge)
        );
        assert_eq!(
            log.read_after(1, 0, 0),
            Err(ReplicationError::ZeroBatchLimit)
        );
    }
}
