//! End-to-end replay checkpoint delta exchange over the M053–M056 primitives.
//!
//! This module creates portable delta frames and applies received frames to a
//! revisioned M054 store. Applying is transactional at the store boundary:
//! frame decoding, base matching, delta application, and checkpoint validation
//! all complete before the store is changed.

use crate::checkpoint_codec::{CodecLimits, ReplayCheckpoint};
use crate::x054::{CheckpointStore, StoreError};
use crate::x055::{apply, diff, DeltaError};
use crate::x056::{decode, encode, DeltaCodecError, DeltaCodecLimits};

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ExchangeLimits {
    pub checkpoint: CodecLimits,
    pub delta_frame: DeltaCodecLimits,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExchangeError {
    Delta(DeltaError),
    Frame(DeltaCodecError),
    Store(StoreError),
    RevisionMismatch { expected: u64, actual: u64 },
}

impl From<DeltaError> for ExchangeError {
    fn from(value: DeltaError) -> Self {
        Self::Delta(value)
    }
}

impl From<DeltaCodecError> for ExchangeError {
    fn from(value: DeltaCodecError) -> Self {
        Self::Frame(value)
    }
}

impl From<StoreError> for ExchangeError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

/// Builds a deterministic encoded delta from `base` to `target`.
///
/// Both checkpoints are validated by the M055 diff path, and the resulting
/// frame is bounded and checksummed by the M056 codec.
pub fn create_delta_frame(
    base: &ReplayCheckpoint,
    target: &ReplayCheckpoint,
    limits: &ExchangeLimits,
) -> Result<Vec<u8>, ExchangeError> {
    let delta = diff(base, target, &limits.checkpoint)?;
    Ok(encode(&delta, &limits.delta_frame)?)
}

/// Decodes and applies a delta without mutating a store.
///
/// Useful for validation, inspection, or staging before an explicit commit.
pub fn preview_delta_frame(
    base: &ReplayCheckpoint,
    frame: &[u8],
    limits: &ExchangeLimits,
) -> Result<ReplayCheckpoint, ExchangeError> {
    let delta = decode(frame, &limits.delta_frame)?;
    Ok(apply(base, &delta, &limits.checkpoint)?)
}

/// Applies a received delta to a revisioned checkpoint store.
///
/// The expected revision is checked before processing. The M055 base digest
/// then binds the delta to the exact checkpoint currently stored. The final
/// checkpoint is committed with M054 compare-and-swap, so every error leaves
/// the store unchanged.
pub fn apply_delta_frame(
    store: &mut CheckpointStore,
    expected_revision: u64,
    frame: &[u8],
    limits: &ExchangeLimits,
) -> Result<u64, ExchangeError> {
    let current = store.load()?;
    if current.revision != expected_revision {
        return Err(ExchangeError::RevisionMismatch {
            expected: expected_revision,
            actual: current.revision,
        });
    }

    let candidate = preview_delta_frame(&current.checkpoint, frame, limits)?;
    Ok(store.compare_and_swap(expected_revision, &candidate)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_codec::StreamCheckpoint;
    use std::collections::BTreeSet;

    fn checkpoint(highest: u64) -> ReplayCheckpoint {
        let mut accepted_sequences = BTreeSet::new();
        if highest > 0 {
            accepted_sequences.insert(highest);
        }
        ReplayCheckpoint {
            window_size: 16,
            streams: vec![StreamCheckpoint {
                tenant_id: "tenant-a".to_owned(),
                peer_id: "peer-a".to_owned(),
                highest_sequence: highest,
                accepted_sequences,
            }],
        }
    }

    fn limits() -> ExchangeLimits {
        ExchangeLimits::default()
    }

    fn initialized_store(value: &ReplayCheckpoint) -> CheckpointStore {
        let mut store = CheckpointStore::new(CodecLimits::default());
        let result = store.initialize(value);
        assert!(result.is_ok());
        store
    }

    #[test]
    fn creates_frame_and_applies_it_to_revisioned_store() {
        let base = checkpoint(4);
        let target = checkpoint(9);
        let frame = create_delta_frame(&base, &target, &limits());
        assert!(frame.is_ok());
        let mut store = initialized_store(&base);
        let revision = apply_delta_frame(&mut store, 1, &frame.unwrap_or_default(), &limits());
        assert_eq!(revision, Ok(2));
        assert_eq!(store.load().map(|snapshot| snapshot.checkpoint), Ok(target));
    }

    #[test]
    fn preview_does_not_mutate_any_store() {
        let base = checkpoint(4);
        let target = checkpoint(7);
        let frame = create_delta_frame(&base, &target, &limits());
        assert!(frame.is_ok());
        assert_eq!(
            preview_delta_frame(&base, &frame.unwrap_or_default(), &limits()),
            Ok(target)
        );
    }

    #[test]
    fn rejects_stale_revision_without_mutation() {
        let base = checkpoint(4);
        let target = checkpoint(7);
        let frame = create_delta_frame(&base, &target, &limits());
        assert!(frame.is_ok());
        let mut store = initialized_store(&base);
        let before = store.export_bytes();
        assert_eq!(
            apply_delta_frame(&mut store, 8, &frame.unwrap_or_default(), &limits()),
            Err(ExchangeError::RevisionMismatch {
                expected: 8,
                actual: 1
            })
        );
        assert_eq!(store.export_bytes(), before);
        assert_eq!(store.revision(), 1);
    }

    #[test]
    fn rejects_wrong_base_and_preserves_store() {
        let base = checkpoint(4);
        let target = checkpoint(7);
        let unrelated = checkpoint(5);
        let frame = create_delta_frame(&base, &target, &limits());
        assert!(frame.is_ok());
        let mut store = initialized_store(&unrelated);
        let before = store.export_bytes();
        let result = apply_delta_frame(&mut store, 1, &frame.unwrap_or_default(), &limits());
        assert!(matches!(
            result,
            Err(ExchangeError::Delta(DeltaError::BaseMismatch { .. }))
        ));
        assert_eq!(store.export_bytes(), before);
        assert_eq!(store.revision(), 1);
    }

    #[test]
    fn rejects_corrupt_frame_without_committing() {
        let base = checkpoint(4);
        let target = checkpoint(7);
        let frame = create_delta_frame(&base, &target, &limits());
        assert!(frame.is_ok());
        let mut damaged = frame.unwrap_or_default();
        if let Some(byte) = damaged.get_mut(12) {
            *byte ^= 0x40;
        }
        let mut store = initialized_store(&base);
        let before = store.export_bytes();
        assert!(apply_delta_frame(&mut store, 1, &damaged, &limits()).is_err());
        assert_eq!(store.export_bytes(), before);
        assert_eq!(store.revision(), 1);
    }

    #[test]
    fn empty_change_frame_advances_revision_without_changing_checkpoint() {
        let base = checkpoint(4);
        let frame = create_delta_frame(&base, &base, &limits());
        assert!(frame.is_ok());
        let mut store = initialized_store(&base);
        assert_eq!(
            apply_delta_frame(&mut store, 1, &frame.unwrap_or_default(), &limits()),
            Ok(2)
        );
        assert_eq!(store.load().map(|snapshot| snapshot.checkpoint), Ok(base));
    }
}
