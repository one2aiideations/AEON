//! Canonical compaction for replay delta delivery journals.
//!
//! Compaction first performs physical and semantic recovery, then writes a
//! canonical event stream to a new file and atomically publishes it with rename.
//! Complete source records are retained; M060 may truncate an incomplete final
//! frame during recovery. A high-watermark event preserves IDs allocated by
//! records that have already been pruned.

use crate::x059::{DeliveryJournal, JournalError, JournalEvent, JournalLimits};
use crate::x060::{recover_journal, RecoveredState, RecoveryError, RecoveryLimits};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactionReport {
    pub source_valid_bytes: u64,
    pub source_truncated_bytes: u64,
    pub destination_bytes: u64,
    pub source_event_count: usize,
    pub compacted_event_count: usize,
    pub retained_deliveries: usize,
    pub retained_inbound_keys: usize,
    pub last_delivery_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompactionError {
    Recovery(RecoveryError),
    Journal(JournalError),
    DestinationExists,
    SameSourceAndDestination,
    InvalidDestination,
    EventCountExceeded,
    Io(String),
}

impl From<RecoveryError> for CompactionError {
    fn from(value: RecoveryError) -> Self {
        Self::Recovery(value)
    }
}

impl From<JournalError> for CompactionError {
    fn from(value: JournalError) -> Self {
        Self::Journal(value)
    }
}

impl From<std::io::Error> for CompactionError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

/// Produces a deterministic minimal event history representing the recovered state.
pub fn canonical_events(
    snapshot: &crate::x060::RecoverySnapshot,
) -> Result<Vec<JournalEvent>, CompactionError> {
    let mut events = Vec::new();
    events.push(JournalEvent::DeliveryIdHighWatermark {
        last_delivery_id: snapshot.last_delivery_id,
    });

    for delivery in snapshot.deliveries.values() {
        events.push(JournalEvent::Enqueued {
            delivery_id: delivery.delivery_id,
            tenant_id: delivery.peer.tenant_id.clone(),
            peer_id: delivery.peer.peer_id.clone(),
            frame: delivery.frame.clone(),
        });

        match delivery.state {
            RecoveredState::Pending { available_at_ms } => {
                append_prior_attempts(
                    &mut events,
                    delivery.delivery_id,
                    delivery.attempts,
                    Some(available_at_ms),
                    None,
                );
            }
            RecoveredState::Leased {
                attempt,
                lease_until_ms,
            } => {
                if attempt != delivery.attempts {
                    return Err(CompactionError::Recovery(RecoveryError::InvalidAttempt {
                        delivery_id: delivery.delivery_id,
                        expected: delivery.attempts,
                        actual: attempt,
                    }));
                }
                append_prior_attempts(
                    &mut events,
                    delivery.delivery_id,
                    delivery.attempts,
                    None,
                    Some(lease_until_ms),
                );
            }
            RecoveredState::Delivered => {
                if delivery.attempts == 0 {
                    return Err(CompactionError::InvalidDestination);
                }
                append_prior_attempts(
                    &mut events,
                    delivery.delivery_id,
                    delivery.attempts,
                    None,
                    Some(0),
                );
                events.push(JournalEvent::Acknowledged {
                    delivery_id: delivery.delivery_id,
                });
            }
            RecoveredState::Failed => {
                if delivery.attempts == 0 {
                    events.push(JournalEvent::Failed {
                        delivery_id: delivery.delivery_id,
                    });
                } else {
                    append_prior_attempts(
                        &mut events,
                        delivery.delivery_id,
                        delivery.attempts,
                        None,
                        Some(0),
                    );
                    events.push(JournalEvent::Failed {
                        delivery_id: delivery.delivery_id,
                    });
                }
            }
        }
    }

    for (peer, frame_ids) in &snapshot.inbound {
        for frame_id in frame_ids {
            events.push(JournalEvent::InboundAccepted {
                tenant_id: peer.tenant_id.clone(),
                peer_id: peer.peer_id.clone(),
                frame_id: *frame_id,
            });
        }
    }
    Ok(events)
}

/// Adds the historical attempts needed to reconstruct a current delivery state.
/// `pending_at` selects a final RetryScheduled; `leased_until` selects a final
/// Leased event. Earlier attempts are represented as lease/retry pairs.
fn append_prior_attempts(
    events: &mut Vec<JournalEvent>,
    delivery_id: u64,
    attempts: u32,
    pending_at: Option<u64>,
    leased_until: Option<u64>,
) {
    if attempts == 0 {
        return;
    }
    for attempt in 1..=attempts {
        let is_last = attempt == attempts;
        let lease_until_ms = if is_last {
            leased_until.unwrap_or(0)
        } else {
            0
        };
        events.push(JournalEvent::Leased {
            delivery_id,
            attempt,
            lease_until_ms,
        });
        if !is_last || pending_at.is_some() {
            events.push(JournalEvent::RetryScheduled {
                delivery_id,
                available_at_ms: if is_last { pending_at.unwrap_or(0) } else { 0 },
            });
        }
    }
}

/// Compacts `source` into a new `destination`.
///
/// Destination must not already exist. A temporary sibling is fully written
/// and synced before an atomic hard-link publication, which will not overwrite
/// an existing destination. The source's valid prefix is preserved; recovery
/// may truncate an incomplete final frame. Do not use the same path for both.
pub fn compact_journal(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    journal_limits: JournalLimits,
    recovery_limits: &RecoveryLimits,
) -> Result<CompactionReport, CompactionError> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    if source == destination {
        return Err(CompactionError::SameSourceAndDestination);
    }
    if destination.exists() {
        return Err(CompactionError::DestinationExists);
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(CompactionError::InvalidDestination);
    }

    let recovered = recover_journal(source, journal_limits.clone(), recovery_limits)?;
    let events = canonical_events(&recovered.state)?;
    if events.len() > journal_limits.max_events {
        return Err(CompactionError::EventCountExceeded);
    }

    let temp = temporary_sibling(destination);
    if temp.exists() {
        fs::remove_file(&temp)?;
    }

    let write_result = (|| -> Result<u64, CompactionError> {
        let opened = DeliveryJournal::open(&temp, journal_limits)?;
        let (mut journal, recovery) = opened;
        if !recovery.events.is_empty() {
            return Err(CompactionError::InvalidDestination);
        }
        for event in &events {
            journal.append(event)?;
        }
        journal.sync()?;
        Ok(journal.len_bytes())
    })();

    let destination_bytes = match write_result {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
    };

    if destination.exists() {
        let _ = fs::remove_file(&temp);
        return Err(CompactionError::DestinationExists);
    }
    if let Err(error) = fs::hard_link(&temp, destination) {
        let _ = fs::remove_file(&temp);
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            CompactionError::DestinationExists
        } else {
            CompactionError::Io(error.to_string())
        });
    }
    fs::remove_file(&temp)?;

    Ok(CompactionReport {
        source_valid_bytes: recovered.valid_bytes,
        source_truncated_bytes: recovered.truncated_bytes,
        destination_bytes,
        source_event_count: recovered.state.event_count,
        compacted_event_count: events.len(),
        retained_deliveries: recovered.state.deliveries.len(),
        retained_inbound_keys: recovered.state.inbound.values().map(|set| set.len()).sum(),
        last_delivery_id: recovered.state.last_delivery_id,
    })
}

fn temporary_sibling(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".agentos-compacting");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x060::{recover_events, PeerKey, RecoveryLimits};
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|v| v.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("agentos-m061-{label}-{nonce}.journal"))
    }

    fn recovery_limits() -> RecoveryLimits {
        RecoveryLimits {
            max_deliveries: 16,
            max_frame_bytes: 1024,
            max_inbound_keys_per_peer: 16,
            max_events: 256,
        }
    }

    fn populated_snapshot() -> crate::x060::RecoverySnapshot {
        let events = vec![
            JournalEvent::DeliveryIdHighWatermark {
                last_delivery_id: 12,
            },
            JournalEvent::Enqueued {
                delivery_id: 4,
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame: vec![1, 2],
            },
            JournalEvent::Leased {
                delivery_id: 4,
                attempt: 1,
                lease_until_ms: 50,
            },
            JournalEvent::RetryScheduled {
                delivery_id: 4,
                available_at_ms: 60,
            },
            JournalEvent::Leased {
                delivery_id: 4,
                attempt: 2,
                lease_until_ms: 90,
            },
            JournalEvent::Enqueued {
                delivery_id: 9,
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame: vec![3, 4],
            },
            JournalEvent::InboundAccepted {
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame_id: 20,
            },
            JournalEvent::InboundAccepted {
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame_id: 21,
            },
        ];
        match recover_events(&events, &recovery_limits()) {
            Ok(value) => value,
            Err(_) => crate::x060::RecoverySnapshot {
                deliveries: BTreeMap::new(),
                inbound: BTreeMap::<PeerKey, BTreeSet<u64>>::new(),
                last_delivery_id: 0,
                event_count: 0,
            },
        }
    }

    #[test]
    fn canonical_events_round_trip_to_same_logical_state() {
        let snapshot = populated_snapshot();
        let canonical = canonical_events(&snapshot);
        assert!(canonical.is_ok());
        if let Ok(events) = canonical {
            let recovered = recover_events(&events, &recovery_limits());
            assert!(recovered.is_ok());
            if let Ok(recovered) = recovered {
                assert_eq!(recovered.deliveries, snapshot.deliveries);
                assert_eq!(recovered.inbound, snapshot.inbound);
                assert_eq!(recovered.last_delivery_id, snapshot.last_delivery_id);
            }
        }
    }

    #[test]
    fn compaction_writes_new_file_and_preserves_source() {
        let source = temp_path("source");
        let destination = temp_path("destination");
        let limits = JournalLimits::default();
        {
            let opened = DeliveryJournal::open(&source, limits.clone());
            assert!(opened.is_ok());
            if let Ok((mut journal, _)) = opened {
                let event = JournalEvent::Enqueued {
                    delivery_id: 5,
                    tenant_id: "tenant".into(),
                    peer_id: "peer".into(),
                    frame: vec![7, 8, 9],
                };
                assert!(journal.append(&event).is_ok());
            }
        }
        let before = fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
        let result = compact_journal(&source, &destination, limits, &recovery_limits());
        assert!(result.is_ok());
        if let Ok(report) = result {
            assert_eq!(report.source_event_count, 1);
            assert!(report.compacted_event_count >= 2);
            assert_eq!(fs::metadata(&source).map(|m| m.len()).unwrap_or(0), before);
            let reopened =
                recover_journal(&destination, JournalLimits::default(), &recovery_limits());
            assert!(reopened.is_ok());
        }
        let _ = fs::remove_file(source);
        let _ = fs::remove_file(destination);
    }

    #[test]
    fn rejects_existing_destination_and_same_path() {
        let source = temp_path("same");
        let _ = fs::write(&source, []);
        assert_eq!(
            compact_journal(
                &source,
                &source,
                JournalLimits::default(),
                &recovery_limits()
            ),
            Err(CompactionError::SameSourceAndDestination)
        );
        let destination = temp_path("exists");
        let _ = fs::write(&destination, [1]);
        assert_eq!(
            compact_journal(
                &source,
                &destination,
                JournalLimits::default(),
                &recovery_limits()
            ),
            Err(CompactionError::DestinationExists)
        );
        let _ = fs::remove_file(source);
        let _ = fs::remove_file(destination);
    }
}
