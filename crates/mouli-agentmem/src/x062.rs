//! Retention planning and safe event selection for replay delta delivery journals.
//!
//! This module separates policy from mutation: it computes a deterministic plan
//! from a recovered snapshot and a caller-supplied time. It never deletes journal
//! records itself. The plan can be reviewed and then applied by producing a new
//! canonical journal using M061 compaction.

use crate::x059::JournalEvent;
use crate::x060::{PeerKey, RecoveredDelivery, RecoveredState, RecoverySnapshot};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionPolicy {
    /// Terminal deliveries older than this age are eligible for pruning.
    pub terminal_retention_ms: u64,
    /// Inbound replay IDs older than this age are eligible for forgetting.
    pub inbound_retention_ms: u64,
    /// Maximum number of retained terminal deliveries, newest first.
    pub max_terminal_deliveries: usize,
    /// Maximum number of retained inbound IDs per tenant/peer.
    pub max_inbound_ids_per_peer: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            terminal_retention_ms: 7 * 24 * 60 * 60 * 1000,
            inbound_retention_ms: 24 * 60 * 60 * 1000,
            max_terminal_deliveries: 4096,
            max_inbound_ids_per_peer: 8192,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryAge {
    pub delivery_id: u64,
    pub terminal_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct InboundAge {
    pub peer: PeerKey,
    pub frame_id: u64,
    pub accepted_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionInput {
    pub now_ms: u64,
    pub terminal_times: BTreeMap<u64, u64>,
    pub inbound_times: BTreeMap<(PeerKey, u64), u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionPlan {
    pub generated_at_ms: u64,
    pub prune_delivery_ids: Vec<u64>,
    pub forget_inbound: Vec<(PeerKey, u64)>,
    pub retained_terminal_count: usize,
    pub retained_inbound_counts: BTreeMap<PeerKey, usize>,
    pub missing_terminal_timestamps: Vec<u64>,
    pub missing_inbound_timestamps: Vec<(PeerKey, u64)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetentionError {
    InvalidPolicy,
    FutureTimestamp { timestamp_ms: u64, now_ms: u64 },
    UnknownDelivery(u64),
    NonTerminalDelivery(u64),
    UnknownInbound { peer: PeerKey, frame_id: u64 },
}

/// Builds a deterministic retention plan.
///
/// Missing timestamps are conservatively retained and reported. Age-based
/// eligibility uses `now_ms.saturating_sub(timestamp)`. Count limits are applied
/// after age limits, retaining the newest terminal records and newest inbound IDs.
pub fn plan_retention(
    snapshot: &RecoverySnapshot,
    policy: &RetentionPolicy,
    input: &RetentionInput,
) -> Result<RetentionPlan, RetentionError> {
    if policy.max_terminal_deliveries == 0 || policy.max_inbound_ids_per_peer == 0 {
        return Err(RetentionError::InvalidPolicy);
    }

    let mut terminal = Vec::<(u64, u64)>::new();
    let mut missing_terminal_timestamps = Vec::new();

    for (id, timestamp) in &input.terminal_times {
        let delivery = snapshot
            .deliveries
            .get(id)
            .ok_or(RetentionError::UnknownDelivery(*id))?;
        if !matches!(
            delivery.state,
            RecoveredState::Delivered | RecoveredState::Failed
        ) {
            return Err(RetentionError::NonTerminalDelivery(*id));
        }
        if *timestamp > input.now_ms {
            return Err(RetentionError::FutureTimestamp {
                timestamp_ms: *timestamp,
                now_ms: input.now_ms,
            });
        }
        terminal.push((*timestamp, *id));
    }

    for (id, delivery) in &snapshot.deliveries {
        if matches!(
            delivery.state,
            RecoveredState::Delivered | RecoveredState::Failed
        ) && !input.terminal_times.contains_key(id)
        {
            missing_terminal_timestamps.push(*id);
        }
    }

    // Oldest first for pruning; deterministic tie-break by ID.
    terminal.sort_by_key(|(timestamp, id)| (*timestamp, *id));
    let mut prune_delivery_ids = Vec::new();
    let mut retained_terminal = Vec::<(u64, u64)>::new();

    for (timestamp, id) in terminal {
        let age = input.now_ms.saturating_sub(timestamp);
        if age >= policy.terminal_retention_ms {
            prune_delivery_ids.push(id);
        } else {
            retained_terminal.push((timestamp, id));
        }
    }

    // Retain newest by timestamp, then larger delivery ID on ties.
    retained_terminal
        .sort_by_key(|(timestamp, id)| (std::cmp::Reverse(*timestamp), std::cmp::Reverse(*id)));
    if retained_terminal.len() > policy.max_terminal_deliveries {
        for (_, id) in retained_terminal.drain(policy.max_terminal_deliveries..) {
            prune_delivery_ids.push(id);
        }
    }
    prune_delivery_ids.sort_unstable();

    let mut inbound_candidates = BTreeMap::<PeerKey, Vec<(u64, u64)>>::new();
    let mut missing_inbound_timestamps = Vec::new();

    for (peer, ids) in &snapshot.inbound {
        for frame_id in ids {
            let key = (peer.clone(), *frame_id);
            match input.inbound_times.get(&key) {
                Some(timestamp) => {
                    if *timestamp > input.now_ms {
                        return Err(RetentionError::FutureTimestamp {
                            timestamp_ms: *timestamp,
                            now_ms: input.now_ms,
                        });
                    }
                    inbound_candidates
                        .entry(peer.clone())
                        .or_default()
                        .push((*timestamp, *frame_id));
                }
                None => missing_inbound_timestamps.push(key),
            }
        }
    }

    let mut forget_inbound = Vec::new();
    let mut retained_inbound_counts = BTreeMap::new();

    for (peer, mut candidates) in inbound_candidates {
        candidates.sort_by_key(|(timestamp, id)| (*timestamp, *id));
        let mut retained = Vec::new();
        for (timestamp, id) in candidates {
            if input.now_ms.saturating_sub(timestamp) >= policy.inbound_retention_ms {
                forget_inbound.push((peer.clone(), id));
            } else {
                retained.push((timestamp, id));
            }
        }
        retained
            .sort_by_key(|(timestamp, id)| (std::cmp::Reverse(*timestamp), std::cmp::Reverse(*id)));
        if retained.len() > policy.max_inbound_ids_per_peer {
            for (_, id) in retained.drain(policy.max_inbound_ids_per_peer..) {
                forget_inbound.push((peer.clone(), id));
            }
        }
        let retained_count = snapshot
            .inbound
            .get(&peer)
            .map(|ids| ids.len())
            .unwrap_or(0)
            .saturating_sub(forget_inbound.iter().filter(|(p, _)| p == &peer).count());
        retained_inbound_counts.insert(peer, retained_count);
    }

    // Peers whose every timestamp is missing are conservatively retained too.
    // Include them in the summary rather than silently omitting their counts.
    for (peer, ids) in &snapshot.inbound {
        retained_inbound_counts
            .entry(peer.clone())
            .or_insert(ids.len());
    }

    forget_inbound.sort();
    missing_terminal_timestamps.sort_unstable();
    missing_inbound_timestamps.sort();

    let pruned_count = prune_delivery_ids.len();
    Ok(RetentionPlan {
        generated_at_ms: input.now_ms,
        prune_delivery_ids,
        forget_inbound,
        retained_terminal_count: snapshot
            .deliveries
            .values()
            .filter(|delivery| {
                matches!(
                    delivery.state,
                    RecoveredState::Delivered | RecoveredState::Failed
                )
            })
            .count()
            .saturating_sub(pruned_count),
        retained_inbound_counts,
        missing_terminal_timestamps,
        missing_inbound_timestamps,
    })
}

/// Converts a retention plan into explicit journal events. The caller must
/// append these events to a new journal or otherwise apply them transactionally.
pub fn retention_events(plan: &RetentionPlan) -> Vec<JournalEvent> {
    let mut events = Vec::new();
    for delivery_id in &plan.prune_delivery_ids {
        events.push(JournalEvent::TerminalPruned {
            delivery_id: *delivery_id,
        });
    }
    for (peer, frame_id) in &plan.forget_inbound {
        events.push(JournalEvent::InboundForgotten {
            tenant_id: peer.tenant_id.clone(),
            peer_id: peer.peer_id.clone(),
            frame_id: *frame_id,
        });
    }
    events
}

/// Returns the state class for a delivery; useful to keep active deliveries
/// excluded from retention workflows even when the caller supplies timestamps.
pub fn is_active(delivery: &RecoveredDelivery) -> bool {
    matches!(
        delivery.state,
        RecoveredState::Pending { .. } | RecoveredState::Leased { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn snapshot() -> RecoverySnapshot {
        let peer = PeerKey {
            tenant_id: "t".into(),
            peer_id: "p".into(),
        };
        let mut deliveries = BTreeMap::new();
        deliveries.insert(
            1,
            RecoveredDelivery {
                delivery_id: 1,
                peer: peer.clone(),
                frame: vec![1],
                attempts: 1,
                state: RecoveredState::Delivered,
            },
        );
        deliveries.insert(
            2,
            RecoveredDelivery {
                delivery_id: 2,
                peer: peer.clone(),
                frame: vec![2],
                attempts: 0,
                state: RecoveredState::Pending {
                    available_at_ms: 100,
                },
            },
        );
        let mut inbound = BTreeMap::new();
        inbound.insert(peer, BTreeSet::from([10, 11, 12]));
        RecoverySnapshot {
            deliveries,
            inbound,
            last_delivery_id: 2,
            event_count: 4,
        }
    }

    #[test]
    fn prunes_old_terminal_and_inbound_entries() {
        let state = snapshot();
        let peer = state.inbound.keys().next().cloned().unwrap_or(PeerKey {
            tenant_id: "t".into(),
            peer_id: "p".into(),
        });
        let policy = RetentionPolicy {
            terminal_retention_ms: 100,
            inbound_retention_ms: 50,
            ..RetentionPolicy::default()
        };
        let input = RetentionInput {
            now_ms: 1000,
            terminal_times: BTreeMap::from([(1, 800)]),
            inbound_times: BTreeMap::from([
                ((peer.clone(), 10), 900),
                ((peer.clone(), 11), 980),
                ((peer, 12), 990),
            ]),
        };
        let plan = plan_retention(&state, &policy, &input);
        assert!(plan.is_ok());
        if let Ok(plan) = plan {
            assert_eq!(plan.prune_delivery_ids, vec![1]);
            assert_eq!(
                plan.forget_inbound,
                vec![(
                    PeerKey {
                        tenant_id: "t".into(),
                        peer_id: "p".into()
                    },
                    10
                )]
            );
        }
    }

    #[test]
    fn count_limits_retain_newest_and_preserve_unknown_age() {
        let state = snapshot();
        let peer = state.inbound.keys().next().cloned().unwrap_or(PeerKey {
            tenant_id: "t".into(),
            peer_id: "p".into(),
        });
        let policy = RetentionPolicy {
            terminal_retention_ms: 10_000,
            inbound_retention_ms: 10_000,
            max_inbound_ids_per_peer: 1,
            ..RetentionPolicy::default()
        };
        let input = RetentionInput {
            now_ms: 1000,
            terminal_times: BTreeMap::from([(1, 999)]),
            inbound_times: BTreeMap::from([((peer.clone(), 10), 900), ((peer.clone(), 11), 950)]),
        };
        let plan = plan_retention(&state, &policy, &input);
        assert!(plan.is_ok());
        if let Ok(plan) = plan {
            assert_eq!(
                plan.forget_inbound,
                vec![(
                    PeerKey {
                        tenant_id: "t".into(),
                        peer_id: "p".into()
                    },
                    10
                )]
            );
            assert_eq!(
                plan.missing_inbound_timestamps,
                vec![(
                    PeerKey {
                        tenant_id: "t".into(),
                        peer_id: "p".into()
                    },
                    12
                )]
            );
        }
    }

    #[test]
    fn rejects_future_and_nonterminal_timestamps() {
        let state = snapshot();
        let future = RetentionInput {
            now_ms: 10,
            terminal_times: BTreeMap::from([(1, 11)]),
            inbound_times: BTreeMap::new(),
        };
        assert!(matches!(
            plan_retention(&state, &RetentionPolicy::default(), &future),
            Err(RetentionError::FutureTimestamp { .. })
        ));
        let nonterminal = RetentionInput {
            now_ms: 10,
            terminal_times: BTreeMap::from([(2, 1)]),
            inbound_times: BTreeMap::new(),
        };
        assert_eq!(
            plan_retention(&state, &RetentionPolicy::default(), &nonterminal),
            Err(RetentionError::NonTerminalDelivery(2))
        );
    }
}
