//! Applies a reviewed M062 retention plan by producing a compacted journal.
//!
//! The source journal is never rewritten. The retained semantic snapshot is
//! canonicalized with M061, written and synced to a unique sibling temporary
//! file, then published with a no-overwrite hard link.

use crate::x059::{DeliveryJournal, JournalError, JournalLimits};
use crate::x060::{recover_journal, RecoveryError, RecoveryLimits, RecoverySnapshot};
use crate::x061::{canonical_events, CompactionError};
use crate::x062::{plan_retention, RetentionError, RetentionInput, RetentionPlan, RetentionPolicy};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionCompactionReport {
    pub source_valid_bytes: u64,
    pub source_truncated_bytes: u64,
    pub destination_bytes: u64,
    pub source_events: usize,
    pub destination_events: usize,
    pub removed_deliveries: usize,
    pub removed_inbound_ids: usize,
    pub retained_deliveries: usize,
    pub retained_inbound_ids: usize,
    pub last_delivery_id: u64,
    pub plan: RetentionPlan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyError {
    Recovery(RecoveryError),
    Retention(RetentionError),
    Compaction(CompactionError),
    Journal(JournalError),
    SameSourceAndDestination,
    DestinationExists,
    InvalidDestination,
    EventCountExceeded,
    TemporaryFileCollision,
    Io(String),
}

impl From<RecoveryError> for ApplyError {
    fn from(value: RecoveryError) -> Self {
        Self::Recovery(value)
    }
}
impl From<RetentionError> for ApplyError {
    fn from(value: RetentionError) -> Self {
        Self::Retention(value)
    }
}
impl From<JournalError> for ApplyError {
    fn from(value: JournalError) -> Self {
        Self::Journal(value)
    }
}
impl From<CompactionError> for ApplyError {
    fn from(value: CompactionError) -> Self {
        Self::Compaction(value)
    }
}
impl From<std::io::Error> for ApplyError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

/// Recovers `source`, computes a retention plan, applies it to a cloned
/// snapshot, and atomically publishes a new compacted journal at `destination`.
///
/// `destination` must not exist and must be on a filesystem supporting hard
/// links. The source remains intact (apart from M059/M060's permitted truncation
/// of an incomplete trailing frame). Missing retention timestamps are retained.
pub fn retain_and_compact(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    policy: &RetentionPolicy,
    input: &RetentionInput,
    journal_limits: JournalLimits,
    recovery_limits: &RecoveryLimits,
) -> Result<RetentionCompactionReport, ApplyError> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    if source == destination {
        return Err(ApplyError::SameSourceAndDestination);
    }
    if destination.exists() {
        return Err(ApplyError::DestinationExists);
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(ApplyError::InvalidDestination);
    }

    let recovered = recover_journal(source, journal_limits.clone(), recovery_limits)?;
    let plan = plan_retention(&recovered.state, policy, input)?;
    let mut retained = recovered.state.clone();
    apply_plan(&mut retained, &plan)?;

    let events = canonical_events(&retained)?;
    if events.len() > journal_limits.max_events {
        return Err(ApplyError::EventCountExceeded);
    }

    let temp = reserve_temporary_sibling(destination)?;
    let write_result = (|| -> Result<u64, ApplyError> {
        let (mut journal, recovery) = DeliveryJournal::open(&temp, journal_limits)?;
        if !recovery.events.is_empty() {
            return Err(ApplyError::TemporaryFileCollision);
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
        return Err(ApplyError::DestinationExists);
    }
    if let Err(error) = fs::hard_link(&temp, destination) {
        let _ = fs::remove_file(&temp);
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            ApplyError::DestinationExists
        } else {
            ApplyError::Io(error.to_string())
        });
    }
    fs::remove_file(&temp)?;

    let removed_deliveries = plan.prune_delivery_ids.len();
    let removed_inbound_ids = plan.forget_inbound.len();
    let retained_deliveries = retained.deliveries.len();
    let retained_inbound_ids = retained.inbound.values().map(|ids| ids.len()).sum();

    Ok(RetentionCompactionReport {
        source_valid_bytes: recovered.valid_bytes,
        source_truncated_bytes: recovered.truncated_bytes,
        destination_bytes,
        source_events: recovered.state.event_count,
        destination_events: events.len(),
        removed_deliveries,
        removed_inbound_ids,
        retained_deliveries,
        retained_inbound_ids,
        last_delivery_id: retained.last_delivery_id,
        plan,
    })
}

fn apply_plan(snapshot: &mut RecoverySnapshot, plan: &RetentionPlan) -> Result<(), ApplyError> {
    for id in &plan.prune_delivery_ids {
        let delivery = snapshot
            .deliveries
            .get(id)
            .ok_or(ApplyError::Retention(RetentionError::UnknownDelivery(*id)))?;
        if !matches!(
            delivery.state,
            crate::x060::RecoveredState::Delivered | crate::x060::RecoveredState::Failed
        ) {
            return Err(ApplyError::Retention(RetentionError::NonTerminalDelivery(
                *id,
            )));
        }
    }
    for (peer, frame_id) in &plan.forget_inbound {
        if !snapshot
            .inbound
            .get(peer)
            .map(|ids| ids.contains(frame_id))
            .unwrap_or(false)
        {
            return Err(ApplyError::Retention(RetentionError::UnknownInbound {
                peer: peer.clone(),
                frame_id: *frame_id,
            }));
        }
    }

    for id in &plan.prune_delivery_ids {
        snapshot.deliveries.remove(id);
    }
    for (peer, frame_id) in &plan.forget_inbound {
        let remove_peer = if let Some(ids) = snapshot.inbound.get_mut(peer) {
            ids.remove(frame_id);
            ids.is_empty()
        } else {
            false
        };
        if remove_peer {
            snapshot.inbound.remove(peer);
        }
    }
    Ok(())
}

fn reserve_temporary_sibling(destination: &Path) -> Result<PathBuf, ApplyError> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = destination.as_os_str().to_os_string();
        name.push(format!(
            ".agentos-retention-{}-{sequence}.tmp",
            std::process::id()
        ));
        let candidate = PathBuf::from(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ApplyError::Io(error.to_string())),
        }
    }
    Err(ApplyError::TemporaryFileCollision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x059::JournalEvent;
    use crate::x060::{recover_events, PeerKey, RecoveredState, RecoverySnapshot};
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn limits() -> RecoveryLimits {
        RecoveryLimits {
            max_deliveries: 16,
            max_frame_bytes: 1024,
            max_inbound_keys_per_peer: 16,
            max_events: 256,
        }
    }

    fn snapshot() -> RecoverySnapshot {
        let events = vec![
            JournalEvent::DeliveryIdHighWatermark {
                last_delivery_id: 8,
            },
            JournalEvent::Enqueued {
                delivery_id: 2,
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame: vec![1],
            },
            JournalEvent::Leased {
                delivery_id: 2,
                attempt: 1,
                lease_until_ms: 5,
            },
            JournalEvent::Acknowledged { delivery_id: 2 },
            JournalEvent::Enqueued {
                delivery_id: 7,
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame: vec![7],
            },
            JournalEvent::InboundAccepted {
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame_id: 10,
            },
            JournalEvent::InboundAccepted {
                tenant_id: "t".into(),
                peer_id: "p".into(),
                frame_id: 11,
            },
        ];
        recover_events(&events, &limits()).unwrap_or(RecoverySnapshot {
            deliveries: BTreeMap::new(),
            inbound: BTreeMap::<PeerKey, BTreeSet<u64>>::new(),
            last_delivery_id: 0,
            event_count: 0,
        })
    }

    #[test]
    fn applying_plan_preserves_active_state_and_high_watermark() {
        let mut state = snapshot();
        let peer = PeerKey {
            tenant_id: "t".into(),
            peer_id: "p".into(),
        };
        let plan = RetentionPlan {
            generated_at_ms: 100,
            prune_delivery_ids: vec![2],
            forget_inbound: vec![(peer.clone(), 10)],
            retained_terminal_count: 0,
            retained_inbound_counts: BTreeMap::from([(peer.clone(), 1)]),
            missing_terminal_timestamps: Vec::new(),
            missing_inbound_timestamps: Vec::new(),
        };
        assert!(apply_plan(&mut state, &plan).is_ok());
        assert!(!state.deliveries.contains_key(&2));
        assert!(matches!(
            state.deliveries.get(&7).map(|d| &d.state),
            Some(RecoveredState::Pending { .. })
        ));
        assert_eq!(
            state.inbound.get(&peer).cloned(),
            Some(BTreeSet::from([11]))
        );
        assert_eq!(state.last_delivery_id, 8);
    }

    #[test]
    fn rejects_attempt_to_prune_active_delivery() {
        let mut state = snapshot();
        let plan = RetentionPlan {
            generated_at_ms: 100,
            prune_delivery_ids: vec![7],
            forget_inbound: Vec::new(),
            retained_terminal_count: 1,
            retained_inbound_counts: BTreeMap::new(),
            missing_terminal_timestamps: Vec::new(),
            missing_inbound_timestamps: Vec::new(),
        };
        assert!(matches!(
            apply_plan(&mut state, &plan),
            Err(ApplyError::Retention(RetentionError::NonTerminalDelivery(
                7
            )))
        ));
    }

    #[test]
    fn source_and_destination_must_differ() {
        let path = std::env::temp_dir().join(format!(
            "agentos-m063-same-{}.journal",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|v| v.as_nanos())
                .unwrap_or(0)
        ));
        let result = retain_and_compact(
            &path,
            &path,
            &RetentionPolicy::default(),
            &RetentionInput {
                now_ms: 1,
                terminal_times: BTreeMap::new(),
                inbound_times: BTreeMap::new(),
            },
            JournalLimits::default(),
            &limits(),
        );
        assert_eq!(result, Err(ApplyError::SameSourceAndDestination));
    }
}
