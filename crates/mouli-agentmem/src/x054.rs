//! Revisioned checkpoint store with validated, atomic in-memory replacement.
//!
//! The store keeps the canonical encoded checkpoint as its committed representation.
//! Candidate snapshots are fully encoded/validated before the committed bytes and
//! revision are changed. This is process-local storage, not durable disk persistence.

use crate::checkpoint_codec::{decode, encode, CheckpointError, CodecLimits, ReplayCheckpoint};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreSnapshot {
    pub revision: u64,
    pub checkpoint: ReplayCheckpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    Codec(CheckpointError),
    RevisionConflict { expected: u64, actual: u64 },
    RevisionExhausted,
    EmptyStore,
}

impl From<CheckpointError> for StoreError {
    fn from(value: CheckpointError) -> Self {
        Self::Codec(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointStore {
    limits: CodecLimits,
    revision: u64,
    committed: Option<Vec<u8>>,
}

impl CheckpointStore {
    pub fn new(limits: CodecLimits) -> Self {
        Self {
            limits,
            revision: 0,
            committed: None,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_initialized(&self) -> bool {
        self.committed.is_some()
    }

    /// Initializes an empty store. The checkpoint is validated and encoded before
    /// any state is committed. A second initialization is rejected.
    pub fn initialize(&mut self, checkpoint: &ReplayCheckpoint) -> Result<u64, StoreError> {
        if self.committed.is_some() {
            return Err(StoreError::RevisionConflict {
                expected: 0,
                actual: self.revision,
            });
        }
        let encoded = encode(checkpoint, &self.limits)?;
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)?;
        self.committed = Some(encoded);
        self.revision = next_revision;
        Ok(self.revision)
    }

    /// Loads and decodes the committed snapshot. No partially decoded state escapes.
    pub fn load(&self) -> Result<StoreSnapshot, StoreError> {
        let bytes = self.committed.as_deref().ok_or(StoreError::EmptyStore)?;
        let checkpoint = decode(bytes, &self.limits)?;
        Ok(StoreSnapshot {
            revision: self.revision,
            checkpoint,
        })
    }

    /// Exports the exact canonical bytes currently committed in the store.
    pub fn export_bytes(&self) -> Result<Vec<u8>, StoreError> {
        self.committed.clone().ok_or(StoreError::EmptyStore)
    }

    /// Atomically replaces the committed checkpoint if the caller's revision is current.
    ///
    /// Validation and encoding happen before mutation. A validation failure, revision
    /// conflict, or revision overflow leaves both revision and stored bytes unchanged.
    pub fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        checkpoint: &ReplayCheckpoint,
    ) -> Result<u64, StoreError> {
        if self.committed.is_none() {
            return Err(StoreError::EmptyStore);
        }
        if expected_revision != self.revision {
            return Err(StoreError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }

        let encoded = encode(checkpoint, &self.limits)?;
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)?;

        self.committed = Some(encoded);
        self.revision = next_revision;
        Ok(self.revision)
    }

    /// Replaces the store from externally supplied bytes after full decode validation.
    /// The current store is untouched if decoding fails or the expected revision is stale.
    pub fn import_bytes(
        &mut self,
        expected_revision: u64,
        bytes: &[u8],
    ) -> Result<u64, StoreError> {
        if self.committed.is_none() {
            return Err(StoreError::EmptyStore);
        }
        if expected_revision != self.revision {
            return Err(StoreError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }

        let checkpoint = decode(bytes, &self.limits)?;
        let canonical = encode(&checkpoint, &self.limits)?;
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)?;

        self.committed = Some(canonical);
        self.revision = next_revision;
        Ok(self.revision)
    }

    /// Clears the current checkpoint using optimistic revision checking.
    pub fn clear(&mut self, expected_revision: u64) -> Result<u64, StoreError> {
        if expected_revision != self.revision {
            return Err(StoreError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        if self.committed.is_none() {
            return Err(StoreError::EmptyStore);
        }
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)?;
        self.committed = None;
        self.revision = next_revision;
        Ok(self.revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn checkpoint(highest: u64) -> ReplayCheckpoint {
        let mut accepted = BTreeSet::new();
        if highest > 0 {
            accepted.insert(highest);
        }
        ReplayCheckpoint {
            window_size: 16,
            streams: vec![crate::checkpoint_codec::StreamCheckpoint {
                tenant_id: "tenant-a".to_owned(),
                peer_id: "peer-a".to_owned(),
                highest_sequence: highest,
                accepted_sequences: accepted,
            }],
        }
    }

    fn store() -> CheckpointStore {
        CheckpointStore::new(CodecLimits::default())
    }

    #[test]
    fn initializes_and_loads_committed_checkpoint() {
        let mut store = store();
        assert_eq!(store.initialize(&checkpoint(8)), Ok(1));
        assert_eq!(
            store.load(),
            Ok(StoreSnapshot {
                revision: 1,
                checkpoint: checkpoint(8)
            })
        );
    }

    #[test]
    fn compare_and_swap_updates_only_matching_revision() {
        let mut store = store();
        assert_eq!(store.initialize(&checkpoint(8)), Ok(1));
        assert_eq!(store.compare_and_swap(1, &checkpoint(9)), Ok(2));
        assert_eq!(
            store.compare_and_swap(1, &checkpoint(10)),
            Err(StoreError::RevisionConflict {
                expected: 1,
                actual: 2
            })
        );
        assert_eq!(
            store.load().map(|snapshot| snapshot.checkpoint),
            Ok(checkpoint(9))
        );
    }

    #[test]
    fn invalid_candidate_leaves_committed_state_unchanged() {
        let mut store = store();
        assert_eq!(store.initialize(&checkpoint(8)), Ok(1));
        let invalid = ReplayCheckpoint {
            window_size: 0,
            streams: Vec::new(),
        };
        assert!(matches!(
            store.compare_and_swap(1, &invalid),
            Err(StoreError::Codec(_))
        ));
        assert_eq!(store.revision(), 1);
        assert_eq!(
            store.load().map(|snapshot| snapshot.checkpoint),
            Ok(checkpoint(8))
        );
    }

    #[test]
    fn imports_validated_bytes_and_rejects_corruption_without_mutation() {
        let limits = CodecLimits::default();
        let external = match encode(&checkpoint(12), &limits) {
            Ok(bytes) => bytes,
            Err(_) => return,
        };
        let mut store = store();
        assert_eq!(store.initialize(&checkpoint(8)), Ok(1));
        assert_eq!(store.import_bytes(1, &external), Ok(2));
        let before = store.export_bytes();
        let mut damaged = external.clone();
        if let Some(byte) = damaged.get_mut(10) {
            *byte ^= 0x20;
        }
        assert!(matches!(
            store.import_bytes(2, &damaged),
            Err(StoreError::Codec(_))
        ));
        assert_eq!(store.revision(), 2);
        assert_eq!(store.export_bytes(), before);
    }

    #[test]
    fn export_import_round_trip_and_clear_are_revisioned() {
        let mut source = store();
        assert_eq!(source.initialize(&checkpoint(20)), Ok(1));
        let bytes = match source.export_bytes() {
            Ok(value) => value,
            Err(_) => return,
        };

        let mut target = store();
        assert_eq!(target.initialize(&checkpoint(1)), Ok(1));
        assert_eq!(target.import_bytes(1, &bytes), Ok(2));
        assert_eq!(
            target.load().map(|value| value.checkpoint),
            Ok(checkpoint(20))
        );
        assert_eq!(target.clear(2), Ok(3));
        assert_eq!(target.load(), Err(StoreError::EmptyStore));
    }
}
