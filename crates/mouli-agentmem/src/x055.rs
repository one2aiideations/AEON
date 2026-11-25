//! Deterministic diff and application for replay checkpoint snapshots.
//!
//! Deltas operate at tenant/peer stream granularity. A delta is bound to the exact
//! base checkpoint through a stable digest; applying it validates the complete result
//! with the M053 codec before returning it. The digest detects accidental mismatch,
//! not malicious tampering or authenticated origin.

use std::collections::BTreeMap;

use crate::checkpoint_codec::{encode, CodecLimits, ReplayCheckpoint, StreamCheckpoint};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StreamKey {
    pub tenant_id: String,
    pub peer_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamChange {
    Remove { key: StreamKey },
    Replace { stream: StreamCheckpoint },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointDelta {
    pub base_digest: u64,
    pub target_window_size: u64,
    pub changes: Vec<StreamChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeltaError {
    InvalidBase,
    BaseMismatch { expected: u64, actual: u64 },
    DuplicateChange(StreamKey),
    ConflictingChange(StreamKey),
    InvalidIdentity,
    InvalidTargetWindow,
    Codec(String),
}

fn key_for(stream: &StreamCheckpoint) -> StreamKey {
    StreamKey {
        tenant_id: stream.tenant_id.clone(),
        peer_id: stream.peer_id.clone(),
    }
}

/// Stable FNV-1a digest of the canonical M053 checkpoint encoding.
pub fn checkpoint_digest(
    checkpoint: &ReplayCheckpoint,
    limits: &CodecLimits,
) -> Result<u64, DeltaError> {
    let bytes =
        encode(checkpoint, limits).map_err(|error| DeltaError::Codec(format!("{error:?}")))?;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(hash)
}

/// Produces a deterministic stream-level delta from `base` to `target`.
pub fn diff(
    base: &ReplayCheckpoint,
    target: &ReplayCheckpoint,
    limits: &CodecLimits,
) -> Result<CheckpointDelta, DeltaError> {
    let base_digest = checkpoint_digest(base, limits)?;
    // Validate target before constructing a delta.
    let _ = encode(target, limits).map_err(|error| DeltaError::Codec(format!("{error:?}")))?;

    let base_map: BTreeMap<StreamKey, &StreamCheckpoint> = base
        .streams
        .iter()
        .map(|stream| (key_for(stream), stream))
        .collect();
    let target_map: BTreeMap<StreamKey, &StreamCheckpoint> = target
        .streams
        .iter()
        .map(|stream| (key_for(stream), stream))
        .collect();

    let mut changes = Vec::new();

    for key in base_map.keys() {
        if !target_map.contains_key(key) {
            changes.push(StreamChange::Remove { key: key.clone() });
        }
    }

    for (key, target_stream) in &target_map {
        let needs_replace = match base_map.get(key) {
            Some(base_stream) => *base_stream != *target_stream,
            None => true,
        };
        if needs_replace {
            changes.push(StreamChange::Replace {
                stream: (*target_stream).clone(),
            });
        }
    }

    changes.sort_by_key(change_key);

    Ok(CheckpointDelta {
        base_digest,
        target_window_size: target.window_size,
        changes,
    })
}

fn change_key(change: &StreamChange) -> StreamKey {
    match change {
        StreamChange::Remove { key } => key.clone(),
        StreamChange::Replace { stream } => key_for(stream),
    }
}

/// Applies a delta only when its base digest matches. The entire resulting checkpoint
/// is validated by M053 before it is returned.
pub fn apply(
    base: &ReplayCheckpoint,
    delta: &CheckpointDelta,
    limits: &CodecLimits,
) -> Result<ReplayCheckpoint, DeltaError> {
    let actual_digest = checkpoint_digest(base, limits)?;
    if actual_digest != delta.base_digest {
        return Err(DeltaError::BaseMismatch {
            expected: delta.base_digest,
            actual: actual_digest,
        });
    }
    if delta.target_window_size == 0 {
        return Err(DeltaError::InvalidTargetWindow);
    }

    let mut streams: BTreeMap<StreamKey, StreamCheckpoint> = base
        .streams
        .iter()
        .cloned()
        .map(|stream| (key_for(&stream), stream))
        .collect();

    let mut seen = BTreeMap::<StreamKey, u8>::new();
    for change in &delta.changes {
        let key = change_key(change);
        if key.tenant_id.trim().is_empty() || key.peer_id.trim().is_empty() {
            return Err(DeltaError::InvalidIdentity);
        }
        let current_kind = match change {
            StreamChange::Remove { .. } => 1,
            StreamChange::Replace { .. } => 2,
        };
        if let Some(previous_kind) = seen.insert(key.clone(), current_kind) {
            return Err(if previous_kind == current_kind {
                DeltaError::DuplicateChange(key)
            } else {
                DeltaError::ConflictingChange(key)
            });
        }

        match change {
            StreamChange::Remove { .. } => {
                streams.remove(&key);
            }
            StreamChange::Replace { stream } => {
                streams.insert(key, stream.clone());
            }
        }
    }

    let result = ReplayCheckpoint {
        window_size: delta.target_window_size,
        streams: streams.into_values().collect(),
    };
    encode(&result, limits).map_err(|error| DeltaError::Codec(format!("{error:?}")))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn stream(tenant: &str, peer: &str, highest: u64) -> StreamCheckpoint {
        let mut accepted = BTreeSet::new();
        if highest > 0 {
            accepted.insert(highest);
        }
        StreamCheckpoint {
            tenant_id: tenant.to_owned(),
            peer_id: peer.to_owned(),
            highest_sequence: highest,
            accepted_sequences: accepted,
        }
    }

    fn limits() -> CodecLimits {
        CodecLimits::default()
    }

    #[test]
    fn diff_and_apply_reconstruct_target_exactly() {
        let base = ReplayCheckpoint {
            window_size: 8,
            streams: vec![stream("t1", "p1", 4), stream("t1", "p2", 9)],
        };
        let target = ReplayCheckpoint {
            window_size: 16,
            streams: vec![stream("t1", "p1", 7), stream("t2", "p3", 2)],
        };
        let delta = diff(&base, &target, &limits());
        assert!(delta.is_ok());
        let result = match delta {
            Ok(value) => apply(&base, &value, &limits()),
            Err(error) => Err(error),
        };
        assert_eq!(result, Ok(target));
    }

    #[test]
    fn rejects_delta_for_wrong_base() {
        let base = ReplayCheckpoint {
            window_size: 8,
            streams: vec![stream("t", "p", 4)],
        };
        let other = ReplayCheckpoint {
            window_size: 8,
            streams: vec![stream("t", "p", 5)],
        };
        let target = ReplayCheckpoint {
            window_size: 8,
            streams: vec![stream("t", "p", 6)],
        };
        let delta = match diff(&base, &target, &limits()) {
            Ok(value) => value,
            Err(_) => return,
        };
        assert!(matches!(
            apply(&other, &delta, &limits()),
            Err(DeltaError::BaseMismatch { .. })
        ));
    }

    #[test]
    fn empty_delta_preserves_base_when_window_is_unchanged() {
        let base = ReplayCheckpoint {
            window_size: 8,
            streams: vec![stream("t", "p", 4)],
        };
        let delta = diff(&base, &base, &limits());
        assert!(
            matches!(delta, Ok(CheckpointDelta { ref changes, .. }) if changes.is_empty()),
            "expected an empty delta"
        );
        let result = match delta {
            Ok(value) => apply(&base, &value, &limits()),
            Err(error) => Err(error),
        };
        assert_eq!(result, Ok(base));
    }

    #[test]
    fn rejects_duplicate_stream_changes() {
        let base = ReplayCheckpoint {
            window_size: 8,
            streams: Vec::new(),
        };
        let digest = match checkpoint_digest(&base, &limits()) {
            Ok(value) => value,
            Err(_) => return,
        };
        let replacement = stream("t", "p", 1);
        let delta = CheckpointDelta {
            base_digest: digest,
            target_window_size: 8,
            changes: vec![
                StreamChange::Replace {
                    stream: replacement.clone(),
                },
                StreamChange::Replace {
                    stream: replacement,
                },
            ],
        };
        assert!(matches!(
            apply(&base, &delta, &limits()),
            Err(DeltaError::DuplicateChange(_))
        ));
    }

    #[test]
    fn distinguishes_conflicting_remove_and_replace() {
        let base = ReplayCheckpoint {
            window_size: 8,
            streams: Vec::new(),
        };
        let digest = match checkpoint_digest(&base, &limits()) {
            Ok(value) => value,
            Err(_) => return,
        };
        let delta = CheckpointDelta {
            base_digest: digest,
            target_window_size: 8,
            changes: vec![
                StreamChange::Remove {
                    key: StreamKey {
                        tenant_id: "t".to_owned(),
                        peer_id: "p".to_owned(),
                    },
                },
                StreamChange::Replace {
                    stream: stream("t", "p", 1),
                },
            ],
        };
        assert!(matches!(
            apply(&base, &delta, &limits()),
            Err(DeltaError::ConflictingChange(_))
        ));
    }

    #[test]
    fn rejects_invalid_result_window_or_stream_state() {
        let base = ReplayCheckpoint {
            window_size: 8,
            streams: Vec::new(),
        };
        let digest = match checkpoint_digest(&base, &limits()) {
            Ok(value) => value,
            Err(_) => return,
        };
        let invalid_window = CheckpointDelta {
            base_digest: digest,
            target_window_size: 0,
            changes: Vec::new(),
        };
        assert_eq!(
            apply(&base, &invalid_window, &limits()),
            Err(DeltaError::InvalidTargetWindow)
        );

        let invalid_stream = CheckpointDelta {
            base_digest: digest,
            target_window_size: 8,
            changes: vec![StreamChange::Replace {
                stream: stream("", "p", 1),
            }],
        };
        assert_eq!(
            apply(&base, &invalid_stream, &limits()),
            Err(DeltaError::InvalidIdentity)
        );
    }
}
