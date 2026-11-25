//! Bounded per-tenant/per-peer replay protection for replication sequence numbers.
//!
//! The guard accepts out-of-order messages within a configurable sliding window,
//! rejects duplicates and stale sequence numbers, and bounds the number of tracked
//! streams. It is an in-memory primitive; durable replay state and peer authentication
//! must be provided by the integrating transport/session layer.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StreamKey {
    pub tenant_id: String,
    pub peer_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayDecision {
    Accepted,
    Duplicate,
    TooOld,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayError {
    EmptyIdentity,
    ZeroSequence,
    InvalidWindow,
    StreamCapacityReached,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamState {
    highest_sequence: u64,
    accepted: BTreeSet<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayGuard {
    window_size: u64,
    max_streams: usize,
    streams: BTreeMap<StreamKey, StreamState>,
}

impl ReplayGuard {
    pub fn new(window_size: u64, max_streams: usize) -> Result<Self, ReplayError> {
        if window_size == 0 || max_streams == 0 {
            return Err(ReplayError::InvalidWindow);
        }
        Ok(Self {
            window_size,
            max_streams,
            streams: BTreeMap::new(),
        })
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn highest_sequence(&self, tenant_id: &str, peer_id: &str) -> Option<u64> {
        let key = StreamKey {
            tenant_id: tenant_id.to_owned(),
            peer_id: peer_id.to_owned(),
        };
        self.streams.get(&key).map(|state| state.highest_sequence)
    }

    /// Records a sequence number for one tenant/peer stream.
    ///
    /// `Accepted` means this sequence was not previously observed and is inside the
    /// sliding window. `Duplicate` means it was already accepted. `TooOld` means it
    /// falls outside the retained window. These decisions do not authenticate sender.
    pub fn check_and_record(
        &mut self,
        tenant_id: &str,
        peer_id: &str,
        sequence: u64,
    ) -> Result<ReplayDecision, ReplayError> {
        if tenant_id.trim().is_empty() || peer_id.trim().is_empty() {
            return Err(ReplayError::EmptyIdentity);
        }
        if sequence == 0 {
            return Err(ReplayError::ZeroSequence);
        }

        let key = StreamKey {
            tenant_id: tenant_id.to_owned(),
            peer_id: peer_id.to_owned(),
        };

        if !self.streams.contains_key(&key) {
            if self.streams.len() >= self.max_streams {
                return Err(ReplayError::StreamCapacityReached);
            }
            let mut accepted = BTreeSet::new();
            accepted.insert(sequence);
            self.streams.insert(
                key,
                StreamState {
                    highest_sequence: sequence,
                    accepted,
                },
            );
            return Ok(ReplayDecision::Accepted);
        }

        let state = match self.streams.get_mut(&key) {
            Some(value) => value,
            None => return Err(ReplayError::StreamCapacityReached),
        };

        if state.accepted.contains(&sequence) {
            return Ok(ReplayDecision::Duplicate);
        }

        if sequence <= state.highest_sequence {
            let oldest_retained = state
                .highest_sequence
                .saturating_sub(self.window_size.saturating_sub(1));
            if sequence < oldest_retained {
                return Ok(ReplayDecision::TooOld);
            }
        }

        if sequence > state.highest_sequence {
            state.highest_sequence = sequence;
            let oldest_retained = state
                .highest_sequence
                .saturating_sub(self.window_size.saturating_sub(1));
            state.accepted.retain(|seen| *seen >= oldest_retained);
        }

        state.accepted.insert(sequence);
        Ok(ReplayDecision::Accepted)
    }

    /// Removes replay history for one stream, for example after authenticated peer
    /// revocation or an explicit session reset.
    pub fn remove_stream(&mut self, tenant_id: &str, peer_id: &str) -> bool {
        let key = StreamKey {
            tenant_id: tenant_id.to_owned(),
            peer_id: peer_id.to_owned(),
        };
        self.streams.remove(&key).is_some()
    }

    /// Clears all in-memory replay history.
    pub fn clear(&mut self) {
        self.streams.clear();
    }

    /// Returns the number of retained sequence numbers for a stream.
    pub fn retained_sequence_count(&self, tenant_id: &str, peer_id: &str) -> usize {
        let key = StreamKey {
            tenant_id: tenant_id.to_owned(),
            peer_id: peer_id.to_owned(),
        };
        self.streams
            .get(&key)
            .map_or(0, |state| state.accepted.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_out_of_order_values_inside_window_and_rejects_duplicates() {
        let mut guard = match ReplayGuard::new(8, 4) {
            Ok(value) => value,
            Err(_) => panic!("valid guard configuration rejected"),
        };
        assert_eq!(
            guard.check_and_record("tenant-a", "peer-1", 10),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(
            guard.check_and_record("tenant-a", "peer-1", 8),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(
            guard.check_and_record("tenant-a", "peer-1", 10),
            Ok(ReplayDecision::Duplicate)
        );
        assert_eq!(guard.highest_sequence("tenant-a", "peer-1"), Some(10));
    }

    #[test]
    fn rejects_values_outside_sliding_window() {
        let mut guard = match ReplayGuard::new(4, 2) {
            Ok(value) => value,
            Err(_) => panic!("valid guard configuration rejected"),
        };
        assert_eq!(
            guard.check_and_record("t", "p", 5),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(
            guard.check_and_record("t", "p", 9),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(
            guard.check_and_record("t", "p", 5),
            Ok(ReplayDecision::TooOld)
        );
        assert_eq!(guard.retained_sequence_count("t", "p"), 1);
    }

    #[test]
    fn isolates_tenants_and_peers() {
        let mut guard = match ReplayGuard::new(16, 4) {
            Ok(value) => value,
            Err(_) => panic!("valid guard configuration rejected"),
        };
        assert_eq!(
            guard.check_and_record("tenant-a", "peer", 1),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(
            guard.check_and_record("tenant-b", "peer", 1),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(
            guard.check_and_record("tenant-a", "other-peer", 1),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(guard.stream_count(), 3);
    }

    #[test]
    fn enforces_stream_capacity_and_supports_explicit_removal() {
        let mut guard = match ReplayGuard::new(8, 1) {
            Ok(value) => value,
            Err(_) => panic!("valid guard configuration rejected"),
        };
        assert_eq!(
            guard.check_and_record("t", "p1", 1),
            Ok(ReplayDecision::Accepted)
        );
        assert_eq!(
            guard.check_and_record("t", "p2", 1),
            Err(ReplayError::StreamCapacityReached)
        );
        assert!(guard.remove_stream("t", "p1"));
        assert_eq!(
            guard.check_and_record("t", "p2", 1),
            Ok(ReplayDecision::Accepted)
        );
    }

    #[test]
    fn validates_identity_and_nonzero_sequence() {
        let mut guard = match ReplayGuard::new(8, 2) {
            Ok(value) => value,
            Err(_) => panic!("valid guard configuration rejected"),
        };
        assert_eq!(
            guard.check_and_record("", "p", 1),
            Err(ReplayError::EmptyIdentity)
        );
        assert_eq!(
            guard.check_and_record("t", "p", 0),
            Err(ReplayError::ZeroSequence)
        );
        assert_eq!(guard.stream_count(), 0);
    }

    #[test]
    fn clear_removes_all_history() {
        let mut guard = match ReplayGuard::new(8, 4) {
            Ok(value) => value,
            Err(_) => panic!("valid guard configuration rejected"),
        };
        assert_eq!(
            guard.check_and_record("t", "p", 2),
            Ok(ReplayDecision::Accepted)
        );
        guard.clear();
        assert_eq!(guard.stream_count(), 0);
        assert_eq!(guard.highest_sequence("t", "p"), None);
    }
}
