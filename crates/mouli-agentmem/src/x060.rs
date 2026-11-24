//! Deterministic semantic recovery for the M059 replay-delta delivery journal.
//!
//! M059 verifies and decodes the journal's physical frames. This module validates
//! the event history and reconstructs the logical outbox and inbound deduplication
//! ledger. A semantically invalid history is rejected as a whole; no partial
//! recovered state is returned.

use crate::x059::{DeliveryJournal, JournalError, JournalEvent, JournalLimits};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PeerKey {
    pub tenant_id: String,
    pub peer_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveredState {
    Pending { available_at_ms: u64 },
    Leased { attempt: u32, lease_until_ms: u64 },
    Delivered,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredDelivery {
    pub delivery_id: u64,
    pub peer: PeerKey,
    pub frame: Vec<u8>,
    pub attempts: u32,
    pub state: RecoveredState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryLimits {
    pub max_deliveries: usize,
    pub max_frame_bytes: usize,
    pub max_inbound_keys_per_peer: usize,
    pub max_events: usize,
}

impl Default for RecoveryLimits {
    fn default() -> Self {
        Self {
            max_deliveries: 4096,
            max_frame_bytes: 4 * 1024 * 1024,
            max_inbound_keys_per_peer: 8192,
            max_events: 1_000_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryError {
    InvalidLimits,
    Journal(JournalError),
    TooManyEvents,
    TooManyDeliveries,
    FrameTooLarge,
    InvalidIdentity,
    EmptyFrame,
    DuplicateDelivery(u64),
    UnknownDelivery(u64),
    InvalidTransition {
        delivery_id: u64,
        event: &'static str,
    },
    InvalidAttempt {
        delivery_id: u64,
        expected: u32,
        actual: u32,
    },
    DuplicateInbound {
        tenant_id: String,
        peer_id: String,
        frame_id: u64,
    },
    UnknownInbound {
        tenant_id: String,
        peer_id: String,
        frame_id: u64,
    },
    InboundCapacity {
        tenant_id: String,
        peer_id: String,
    },
    DuplicatePrune(u64),
    DeliveryIdRegression {
        previous: u64,
        next: u64,
    },
    SequenceExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRecoverySnapshot {
    pub state: RecoverySnapshot,
    pub valid_bytes: u64,
    pub truncated_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoverySnapshot {
    pub deliveries: BTreeMap<u64, RecoveredDelivery>,
    pub inbound: BTreeMap<PeerKey, BTreeSet<u64>>,
    pub last_delivery_id: u64,
    pub event_count: usize,
}

impl RecoverySnapshot {
    pub fn pending_count(&self) -> usize {
        self.deliveries
            .values()
            .filter(|d| matches!(d.state, RecoveredState::Pending { .. }))
            .count()
    }

    pub fn leased_count(&self) -> usize {
        self.deliveries
            .values()
            .filter(|d| matches!(d.state, RecoveredState::Leased { .. }))
            .count()
    }

    pub fn delivered_count(&self) -> usize {
        self.deliveries
            .values()
            .filter(|d| matches!(d.state, RecoveredState::Delivered))
            .count()
    }

    pub fn failed_count(&self) -> usize {
        self.deliveries
            .values()
            .filter(|d| matches!(d.state, RecoveredState::Failed))
            .count()
    }

    /// The next ID is strictly greater than every ID observed in the journal.
    pub fn next_delivery_id(&self) -> Result<u64, RecoveryError> {
        self.last_delivery_id
            .checked_add(1)
            .ok_or(RecoveryError::SequenceExhausted)
    }
}

/// Opens a physical M059 journal, repairs only an incomplete trailing frame,
/// then validates and reconstructs the complete semantic delivery history.
pub fn recover_journal(
    path: impl AsRef<Path>,
    journal_limits: JournalLimits,
    recovery_limits: &RecoveryLimits,
) -> Result<JournalRecoverySnapshot, RecoveryError> {
    let opened = DeliveryJournal::open(path, journal_limits).map_err(RecoveryError::Journal)?;
    let (_journal, physical) = opened;
    let state = recover_events(&physical.events, recovery_limits)?;
    Ok(JournalRecoverySnapshot {
        state,
        valid_bytes: physical.valid_bytes,
        truncated_bytes: physical.truncated_bytes,
    })
}

pub fn recover_events(
    events: &[JournalEvent],
    limits: &RecoveryLimits,
) -> Result<RecoverySnapshot, RecoveryError> {
    if limits.max_deliveries == 0
        || limits.max_frame_bytes == 0
        || limits.max_inbound_keys_per_peer == 0
        || limits.max_events == 0
    {
        return Err(RecoveryError::InvalidLimits);
    }
    if events.len() > limits.max_events {
        return Err(RecoveryError::TooManyEvents);
    }

    let mut deliveries: BTreeMap<u64, RecoveredDelivery> = BTreeMap::new();
    let mut inbound: BTreeMap<PeerKey, BTreeSet<u64>> = BTreeMap::new();
    let mut last_delivery_id = 0_u64;
    let mut last_enqueued_id = 0_u64;
    let mut saw_high_watermark = false;
    let mut pruned = BTreeSet::new();

    for (event_index, event) in events.iter().enumerate() {
        match event {
            JournalEvent::Enqueued {
                delivery_id,
                tenant_id,
                peer_id,
                frame,
            } => {
                validate_identity(tenant_id, peer_id)?;
                if frame.is_empty() {
                    return Err(RecoveryError::EmptyFrame);
                }
                if frame.len() > limits.max_frame_bytes {
                    return Err(RecoveryError::FrameTooLarge);
                }
                if deliveries.contains_key(delivery_id) || pruned.contains(delivery_id) {
                    return Err(RecoveryError::DuplicateDelivery(*delivery_id));
                }
                if *delivery_id <= last_enqueued_id {
                    return Err(RecoveryError::DeliveryIdRegression {
                        previous: last_enqueued_id,
                        next: *delivery_id,
                    });
                }
                if deliveries.len() >= limits.max_deliveries {
                    return Err(RecoveryError::TooManyDeliveries);
                }
                last_enqueued_id = *delivery_id;
                last_delivery_id = last_delivery_id.max(*delivery_id);
                deliveries.insert(
                    *delivery_id,
                    RecoveredDelivery {
                        delivery_id: *delivery_id,
                        peer: PeerKey {
                            tenant_id: tenant_id.clone(),
                            peer_id: peer_id.clone(),
                        },
                        frame: frame.clone(),
                        attempts: 0,
                        state: RecoveredState::Pending { available_at_ms: 0 },
                    },
                );
            }
            JournalEvent::Leased {
                delivery_id,
                attempt,
                lease_until_ms,
            } => {
                let record = delivery_mut(&mut deliveries, *delivery_id)?;
                if !matches!(record.state, RecoveredState::Pending { .. }) {
                    return Err(transition(*delivery_id, "Leased"));
                }
                let expected = record
                    .attempts
                    .checked_add(1)
                    .ok_or(RecoveryError::SequenceExhausted)?;
                if *attempt != expected {
                    return Err(RecoveryError::InvalidAttempt {
                        delivery_id: *delivery_id,
                        expected,
                        actual: *attempt,
                    });
                }
                record.attempts = *attempt;
                record.state = RecoveredState::Leased {
                    attempt: *attempt,
                    lease_until_ms: *lease_until_ms,
                };
            }
            JournalEvent::Acknowledged { delivery_id } => {
                let record = delivery_mut(&mut deliveries, *delivery_id)?;
                if !matches!(record.state, RecoveredState::Leased { .. }) {
                    return Err(transition(*delivery_id, "Acknowledged"));
                }
                record.state = RecoveredState::Delivered;
            }
            JournalEvent::RetryScheduled {
                delivery_id,
                available_at_ms,
            } => {
                let record = delivery_mut(&mut deliveries, *delivery_id)?;
                if !matches!(record.state, RecoveredState::Leased { .. }) {
                    return Err(transition(*delivery_id, "RetryScheduled"));
                }
                record.state = RecoveredState::Pending {
                    available_at_ms: *available_at_ms,
                };
            }
            JournalEvent::Failed { delivery_id } => {
                let record = delivery_mut(&mut deliveries, *delivery_id)?;
                if !matches!(
                    record.state,
                    RecoveredState::Leased { .. } | RecoveredState::Pending { .. }
                ) {
                    return Err(transition(*delivery_id, "Failed"));
                }
                record.state = RecoveredState::Failed;
            }
            JournalEvent::InboundAccepted {
                tenant_id,
                peer_id,
                frame_id,
            } => {
                validate_identity(tenant_id, peer_id)?;
                let key = PeerKey {
                    tenant_id: tenant_id.clone(),
                    peer_id: peer_id.clone(),
                };
                let entries = inbound.entry(key.clone()).or_default();
                if entries.contains(frame_id) {
                    return Err(RecoveryError::DuplicateInbound {
                        tenant_id: key.tenant_id,
                        peer_id: key.peer_id,
                        frame_id: *frame_id,
                    });
                }
                if entries.len() >= limits.max_inbound_keys_per_peer {
                    return Err(RecoveryError::InboundCapacity {
                        tenant_id: key.tenant_id,
                        peer_id: key.peer_id,
                    });
                }
                entries.insert(*frame_id);
            }
            JournalEvent::InboundForgotten {
                tenant_id,
                peer_id,
                frame_id,
            } => {
                validate_identity(tenant_id, peer_id)?;
                let key = PeerKey {
                    tenant_id: tenant_id.clone(),
                    peer_id: peer_id.clone(),
                };
                let entries =
                    inbound
                        .get_mut(&key)
                        .ok_or_else(|| RecoveryError::UnknownInbound {
                            tenant_id: key.tenant_id.clone(),
                            peer_id: key.peer_id.clone(),
                            frame_id: *frame_id,
                        })?;
                if !entries.remove(frame_id) {
                    return Err(RecoveryError::UnknownInbound {
                        tenant_id: key.tenant_id,
                        peer_id: key.peer_id,
                        frame_id: *frame_id,
                    });
                }
                if entries.is_empty() {
                    inbound.remove(&key);
                }
            }
            JournalEvent::TerminalPruned { delivery_id } => {
                let record = deliveries
                    .get(delivery_id)
                    .ok_or(RecoveryError::UnknownDelivery(*delivery_id))?;
                if !matches!(
                    record.state,
                    RecoveredState::Delivered | RecoveredState::Failed
                ) {
                    return Err(transition(*delivery_id, "TerminalPruned"));
                }
                deliveries.remove(delivery_id);
                pruned.insert(*delivery_id);
            }
            JournalEvent::DeliveryIdHighWatermark {
                last_delivery_id: watermark,
            } => {
                if event_index != 0 || saw_high_watermark {
                    return Err(transition(0, "DeliveryIdHighWatermark"));
                }
                saw_high_watermark = true;
                last_delivery_id = last_delivery_id.max(*watermark);
            }
        }
    }

    Ok(RecoverySnapshot {
        deliveries,
        inbound,
        last_delivery_id,
        event_count: events.len(),
    })
}

fn validate_identity(tenant_id: &str, peer_id: &str) -> Result<(), RecoveryError> {
    if tenant_id.trim().is_empty() || peer_id.trim().is_empty() {
        return Err(RecoveryError::InvalidIdentity);
    }
    Ok(())
}

fn delivery_mut(
    deliveries: &mut BTreeMap<u64, RecoveredDelivery>,
    id: u64,
) -> Result<&mut RecoveredDelivery, RecoveryError> {
    deliveries
        .get_mut(&id)
        .ok_or(RecoveryError::UnknownDelivery(id))
}

fn transition(id: u64, event: &'static str) -> RecoveryError {
    RecoveryError::InvalidTransition {
        delivery_id: id,
        event,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enqueue(id: u64) -> JournalEvent {
        JournalEvent::Enqueued {
            delivery_id: id,
            tenant_id: "tenant-a".into(),
            peer_id: "peer-b".into(),
            frame: vec![1, 2, 3],
        }
    }

    fn limits() -> RecoveryLimits {
        RecoveryLimits {
            max_deliveries: 8,
            max_frame_bytes: 64,
            max_inbound_keys_per_peer: 4,
            max_events: 32,
        }
    }

    #[test]
    fn reconstructs_delivery_states_and_attempt_counts() {
        let events = vec![
            enqueue(1),
            JournalEvent::Leased {
                delivery_id: 1,
                attempt: 1,
                lease_until_ms: 50,
            },
            JournalEvent::RetryScheduled {
                delivery_id: 1,
                available_at_ms: 60,
            },
            JournalEvent::Leased {
                delivery_id: 1,
                attempt: 2,
                lease_until_ms: 90,
            },
            JournalEvent::Acknowledged { delivery_id: 1 },
            enqueue(2),
            JournalEvent::Leased {
                delivery_id: 2,
                attempt: 1,
                lease_until_ms: 80,
            },
            JournalEvent::Failed { delivery_id: 2 },
        ];
        let snapshot = recover_events(&events, &limits());
        assert!(snapshot.is_ok());
        if let Ok(snapshot) = snapshot {
            assert_eq!(snapshot.delivered_count(), 1);
            assert_eq!(snapshot.failed_count(), 1);
            assert_eq!(snapshot.deliveries.get(&1).map(|d| d.attempts), Some(2));
            assert_eq!(snapshot.next_delivery_id(), Ok(3));
        }
    }

    #[test]
    fn rejects_invalid_transition_and_attempt_jump() {
        assert_eq!(
            recover_events(
                &[enqueue(1), JournalEvent::Acknowledged { delivery_id: 1 }],
                &limits()
            ),
            Err(RecoveryError::InvalidTransition {
                delivery_id: 1,
                event: "Acknowledged"
            })
        );
        assert_eq!(
            recover_events(
                &[
                    enqueue(1),
                    JournalEvent::Leased {
                        delivery_id: 1,
                        attempt: 3,
                        lease_until_ms: 20
                    },
                ],
                &limits()
            ),
            Err(RecoveryError::InvalidAttempt {
                delivery_id: 1,
                expected: 1,
                actual: 3
            })
        );
    }

    #[test]
    fn rebuilds_inbound_deduplication_and_forgotten_entries() {
        let events = vec![
            JournalEvent::InboundAccepted {
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame_id: 5,
            },
            JournalEvent::InboundAccepted {
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame_id: 6,
            },
            JournalEvent::InboundForgotten {
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame_id: 5,
            },
        ];
        let snapshot = recover_events(&events, &limits());
        assert!(snapshot.is_ok());
        if let Ok(snapshot) = snapshot {
            let key = PeerKey {
                tenant_id: "t".into(),
                peer_id: "p".into(),
            };
            assert_eq!(
                snapshot.inbound.get(&key).cloned(),
                Some(BTreeSet::from([6]))
            );
        }
    }

    #[test]
    fn prunes_only_terminal_deliveries_and_preserves_id_high_watermark() {
        let events = vec![
            enqueue(1),
            JournalEvent::Leased {
                delivery_id: 1,
                attempt: 1,
                lease_until_ms: 5,
            },
            JournalEvent::Acknowledged { delivery_id: 1 },
            JournalEvent::TerminalPruned { delivery_id: 1 },
        ];
        let snapshot = recover_events(&events, &limits());
        assert!(snapshot.is_ok());
        if let Ok(snapshot) = snapshot {
            assert!(snapshot.deliveries.is_empty());
            assert_eq!(snapshot.last_delivery_id, 1);
            assert_eq!(snapshot.next_delivery_id(), Ok(2));
        }
    }

    #[test]
    fn enforces_history_and_resource_limits() {
        let small = RecoveryLimits {
            max_events: 1,
            ..limits()
        };
        assert_eq!(
            recover_events(&[enqueue(1), enqueue(2)], &small),
            Err(RecoveryError::TooManyEvents)
        );
        let small_frames = RecoveryLimits {
            max_frame_bytes: 2,
            ..limits()
        };
        assert_eq!(
            recover_events(&[enqueue(1)], &small_frames),
            Err(RecoveryError::FrameTooLarge)
        );
    }
}
