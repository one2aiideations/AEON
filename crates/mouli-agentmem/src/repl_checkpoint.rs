//! Tenant-scoped replication checkpoint tracking.
//!
//! Tracks the highest contiguous source sequence acknowledged by each
//! destination replica. This is an in-memory coordinator primitive: durable
//! checkpoint persistence, peer authentication, and transport are external.

use std::collections::BTreeMap;

pub type TenantId = u64;
pub type ReplicaId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CheckpointKey {
    pub tenant_id: TenantId,
    pub source_replica: ReplicaId,
    pub destination_replica: ReplicaId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub key: CheckpointKey,
    pub applied_sequence: u64,
    pub updated_at_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointError {
    ZeroCapacity,
    CapacityReached,
    SequenceRegression,
    TimestampRegression,
    UnknownCheckpoint,
    ZeroBatchSize,
}

#[derive(Clone, Debug)]
pub struct CheckpointConfig {
    pub max_checkpoints: usize,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            max_checkpoints: 100_000,
        }
    }
}

pub struct CheckpointManager {
    max_checkpoints: usize,
    checkpoints: BTreeMap<CheckpointKey, Checkpoint>,
}

impl CheckpointManager {
    pub fn new(config: CheckpointConfig) -> Result<Self, CheckpointError> {
        if config.max_checkpoints == 0 {
            return Err(CheckpointError::ZeroCapacity);
        }
        Ok(Self {
            max_checkpoints: config.max_checkpoints,
            checkpoints: BTreeMap::new(),
        })
    }

    /// Record an acknowledgement. Sequence and timestamp may stay equal but
    /// may never move backwards.
    pub fn acknowledge(
        &mut self,
        key: CheckpointKey,
        applied_sequence: u64,
        updated_at_ns: u64,
    ) -> Result<&Checkpoint, CheckpointError> {
        if let Some(current) = self.checkpoints.get(&key) {
            if applied_sequence < current.applied_sequence {
                return Err(CheckpointError::SequenceRegression);
            }
            if updated_at_ns < current.updated_at_ns {
                return Err(CheckpointError::TimestampRegression);
            }
        } else if self.checkpoints.len() >= self.max_checkpoints {
            return Err(CheckpointError::CapacityReached);
        }

        let checkpoint = self.checkpoints.entry(key).or_insert(Checkpoint {
            key,
            applied_sequence: 0,
            updated_at_ns,
        });
        checkpoint.applied_sequence = applied_sequence;
        checkpoint.updated_at_ns = updated_at_ns;
        Ok(checkpoint)
    }

    pub fn get(&self, key: CheckpointKey) -> Option<&Checkpoint> {
        self.checkpoints.get(&key)
    }

    /// Calculate the next bounded source sequence interval to send.
    /// Returns `None` when the destination has caught up.
    pub fn next_batch(
        &self,
        key: CheckpointKey,
        source_latest_sequence: u64,
        max_batch_size: u64,
    ) -> Result<Option<(u64, u64)>, CheckpointError> {
        if max_batch_size == 0 {
            return Err(CheckpointError::ZeroBatchSize);
        }
        let checkpoint = self
            .checkpoints
            .get(&key)
            .ok_or(CheckpointError::UnknownCheckpoint)?;
        if checkpoint.applied_sequence >= source_latest_sequence {
            return Ok(None);
        }
        let start = checkpoint.applied_sequence.saturating_add(1);
        let end = start
            .saturating_add(max_batch_size.saturating_sub(1))
            .min(source_latest_sequence);
        Ok(Some((start, end)))
    }

    /// Minimum acknowledged sequence across a tenant/source's destinations.
    /// `None` means no destination checkpoint has been registered.
    pub fn minimum_acknowledged(
        &self,
        tenant_id: TenantId,
        source_replica: ReplicaId,
    ) -> Option<u64> {
        self.checkpoints
            .values()
            .filter(|checkpoint| {
                checkpoint.key.tenant_id == tenant_id
                    && checkpoint.key.source_replica == source_replica
            })
            .map(|checkpoint| checkpoint.applied_sequence)
            .min()
    }

    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(destination: u64) -> CheckpointKey {
        CheckpointKey {
            tenant_id: 7,
            source_replica: 1,
            destination_replica: destination,
        }
    }

    fn manager(max: usize) -> CheckpointManager {
        CheckpointManager::new(CheckpointConfig {
            max_checkpoints: max,
        })
        .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn acknowledgements_advance_and_batch_ranges_are_bounded() {
        let mut manager = manager(4);
        assert!(manager.acknowledge(key(2), 10, 100).is_ok());
        assert_eq!(manager.next_batch(key(2), 30, 5), Ok(Some((11, 15))));
        assert!(manager.acknowledge(key(2), 30, 200).is_ok());
        assert_eq!(manager.next_batch(key(2), 30, 5), Ok(None));
    }

    #[test]
    fn rejects_regressions_and_calculates_minimum_ack() {
        let mut manager = manager(4);
        assert!(manager.acknowledge(key(2), 20, 100).is_ok());
        assert!(manager.acknowledge(key(3), 12, 100).is_ok());
        assert_eq!(manager.minimum_acknowledged(7, 1), Some(12));
        assert_eq!(
            manager.acknowledge(key(2), 19, 101).map(|_| ()),
            Err(CheckpointError::SequenceRegression)
        );
        assert_eq!(
            manager.acknowledge(key(2), 20, 99).map(|_| ()),
            Err(CheckpointError::TimestampRegression)
        );
    }

    #[test]
    fn enforces_capacity_and_tenant_scoping() {
        let mut manager = manager(1);
        assert!(manager.acknowledge(key(2), 5, 10).is_ok());
        let other_tenant = CheckpointKey {
            tenant_id: 8,
            ..key(3)
        };
        assert_eq!(
            manager.acknowledge(other_tenant, 1, 1).map(|_| ()),
            Err(CheckpointError::CapacityReached)
        );
        assert_eq!(manager.minimum_acknowledged(8, 1), None);
    }
}
