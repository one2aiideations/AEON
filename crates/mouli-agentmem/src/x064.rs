//! Independent validation of M062 delivery-retention plans.
//!
//! The verifier is deliberately read-only. It checks that requested removals
//! refer to existing terminal deliveries and inbound replay IDs, that lists are
//! canonical and duplicate-free, and that reported retained counts agree with
//! the recovered snapshot.

use crate::x060::{PeerKey, RecoveredState, RecoverySnapshot};
use crate::x062::RetentionPlan;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationIssue {
    DeliveryRemovalNotStrictlySorted,
    DuplicateDeliveryRemoval(u64),
    UnknownDelivery(u64),
    NonTerminalDelivery(u64),
    InboundRemovalNotStrictlySorted,
    DuplicateInboundRemoval {
        peer: PeerKey,
        frame_id: u64,
    },
    UnknownInbound {
        peer: PeerKey,
        frame_id: u64,
    },
    RetainedTerminalCountMismatch {
        reported: usize,
        calculated: usize,
    },
    RetainedInboundCountMismatch {
        peer: PeerKey,
        reported: usize,
        calculated: usize,
    },
    MissingInboundPeerCount(PeerKey),
    UnexpectedInboundPeerCount(PeerKey),
    MissingTimestampListNotCanonical,
    MissingTerminalTimestampUnknown(u64),
    MissingTerminalTimestampNotTerminal(u64),
    MissingInboundTimestampUnknown {
        peer: PeerKey,
        frame_id: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationReport {
    pub valid: bool,
    pub issue_count: usize,
    pub issues: Vec<VerificationIssue>,
    pub deliveries_before: usize,
    pub deliveries_after: usize,
    pub inbound_ids_before: usize,
    pub inbound_ids_after: usize,
}

/// Checks a retention plan against the exact recovered state it was derived from.
/// A valid result means the plan is internally consistent; it does not prove the
/// retention policy itself is appropriate or that timestamps are authentic.
pub fn verify_retention_plan(
    snapshot: &RecoverySnapshot,
    plan: &RetentionPlan,
) -> VerificationReport {
    let mut issues = Vec::new();

    if !strictly_increasing(&plan.prune_delivery_ids) {
        issues.push(VerificationIssue::DeliveryRemovalNotStrictlySorted);
    }
    let mut seen_delivery = BTreeSet::new();
    for id in &plan.prune_delivery_ids {
        if !seen_delivery.insert(*id) {
            issues.push(VerificationIssue::DuplicateDeliveryRemoval(*id));
            continue;
        }
        match snapshot.deliveries.get(id) {
            None => issues.push(VerificationIssue::UnknownDelivery(*id)),
            Some(delivery)
                if !matches!(
                    delivery.state,
                    RecoveredState::Delivered | RecoveredState::Failed
                ) =>
            {
                issues.push(VerificationIssue::NonTerminalDelivery(*id))
            }
            Some(_) => {}
        }
    }

    if !strictly_increasing(&plan.forget_inbound) {
        issues.push(VerificationIssue::InboundRemovalNotStrictlySorted);
    }
    let mut seen_inbound = BTreeSet::new();
    for (peer, frame_id) in &plan.forget_inbound {
        let key = (peer.clone(), *frame_id);
        if !seen_inbound.insert(key) {
            issues.push(VerificationIssue::DuplicateInboundRemoval {
                peer: peer.clone(),
                frame_id: *frame_id,
            });
            continue;
        }
        if !snapshot
            .inbound
            .get(peer)
            .map(|ids| ids.contains(frame_id))
            .unwrap_or(false)
        {
            issues.push(VerificationIssue::UnknownInbound {
                peer: peer.clone(),
                frame_id: *frame_id,
            });
        }
    }

    let removed_delivery_ids: BTreeSet<u64> = plan.prune_delivery_ids.iter().copied().collect();
    let terminal_before = snapshot
        .deliveries
        .values()
        .filter(|delivery| {
            matches!(
                delivery.state,
                RecoveredState::Delivered | RecoveredState::Failed
            )
        })
        .count();
    let terminal_removed = removed_delivery_ids
        .iter()
        .filter(|id| {
            snapshot
                .deliveries
                .get(id)
                .map(|d| matches!(d.state, RecoveredState::Delivered | RecoveredState::Failed))
                .unwrap_or(false)
        })
        .count();
    let terminal_after = terminal_before.saturating_sub(terminal_removed);
    if plan.retained_terminal_count != terminal_after {
        issues.push(VerificationIssue::RetainedTerminalCountMismatch {
            reported: plan.retained_terminal_count,
            calculated: terminal_after,
        });
    }

    let removed_by_peer = count_removed_by_peer(&plan.forget_inbound);
    let mut expected_peers = BTreeSet::new();
    for peer in snapshot.inbound.keys() {
        expected_peers.insert(peer.clone());
    }
    for peer in plan.retained_inbound_counts.keys() {
        expected_peers.insert(peer.clone());
    }

    for peer in &expected_peers {
        let before = snapshot.inbound.get(peer).map(|ids| ids.len()).unwrap_or(0);
        let removed = removed_by_peer.get(peer).copied().unwrap_or(0);
        let calculated = before.saturating_sub(removed);
        match plan.retained_inbound_counts.get(peer) {
            Some(reported) if *reported != calculated => {
                issues.push(VerificationIssue::RetainedInboundCountMismatch {
                    peer: peer.clone(),
                    reported: *reported,
                    calculated,
                });
            }
            None if before > 0 => {
                issues.push(VerificationIssue::MissingInboundPeerCount(peer.clone()))
            }
            None => {}
            Some(_) => {}
        }
    }
    for peer in plan.retained_inbound_counts.keys() {
        if !snapshot.inbound.contains_key(peer) {
            issues.push(VerificationIssue::UnexpectedInboundPeerCount(peer.clone()));
        }
    }

    if !strictly_increasing(&plan.missing_terminal_timestamps) {
        issues.push(VerificationIssue::MissingTimestampListNotCanonical);
    }
    for id in &plan.missing_terminal_timestamps {
        match snapshot.deliveries.get(id) {
            None => issues.push(VerificationIssue::MissingTerminalTimestampUnknown(*id)),
            Some(delivery)
                if !matches!(
                    delivery.state,
                    RecoveredState::Delivered | RecoveredState::Failed
                ) =>
            {
                issues.push(VerificationIssue::MissingTerminalTimestampNotTerminal(*id))
            }
            Some(_) => {}
        }
    }

    if !strictly_increasing(&plan.missing_inbound_timestamps) {
        issues.push(VerificationIssue::MissingTimestampListNotCanonical);
    }
    for (peer, frame_id) in &plan.missing_inbound_timestamps {
        if !snapshot
            .inbound
            .get(peer)
            .map(|ids| ids.contains(frame_id))
            .unwrap_or(false)
        {
            issues.push(VerificationIssue::MissingInboundTimestampUnknown {
                peer: peer.clone(),
                frame_id: *frame_id,
            });
        }
    }

    let deliveries_before = snapshot.deliveries.len();
    let deliveries_after = deliveries_before.saturating_sub(
        removed_delivery_ids
            .iter()
            .filter(|id| snapshot.deliveries.contains_key(id))
            .count(),
    );
    let inbound_ids_before = snapshot
        .inbound
        .values()
        .map(|ids| ids.len())
        .sum::<usize>();
    let inbound_ids_after = inbound_ids_before.saturating_sub(
        seen_inbound
            .iter()
            .filter(|(peer, id)| {
                snapshot
                    .inbound
                    .get(peer)
                    .map(|ids| ids.contains(id))
                    .unwrap_or(false)
            })
            .count(),
    );

    VerificationReport {
        valid: issues.is_empty(),
        issue_count: issues.len(),
        issues,
        deliveries_before,
        deliveries_after,
        inbound_ids_before,
        inbound_ids_after,
    }
}

fn strictly_increasing<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn count_removed_by_peer(removals: &[(PeerKey, u64)]) -> BTreeMap<PeerKey, usize> {
    let mut counts = BTreeMap::new();
    for (peer, _) in removals {
        let current = counts.get(peer).copied().unwrap_or(0_usize);
        counts.insert(peer.clone(), current.saturating_add(1));
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x062::RetentionPlan;
    use std::collections::{BTreeMap, BTreeSet};

    fn fixture() -> (RecoverySnapshot, RetentionPlan) {
        let peer = PeerKey {
            tenant_id: "tenant-a".into(),
            peer_id: "peer-a".into(),
        };
        let mut deliveries = BTreeMap::new();
        deliveries.insert(
            1,
            crate::x060::RecoveredDelivery {
                delivery_id: 1,
                peer: peer.clone(),
                frame: vec![1],
                attempts: 1,
                state: RecoveredState::Delivered,
            },
        );
        deliveries.insert(
            2,
            crate::x060::RecoveredDelivery {
                delivery_id: 2,
                peer: peer.clone(),
                frame: vec![2],
                attempts: 0,
                state: RecoveredState::Pending {
                    available_at_ms: 50,
                },
            },
        );
        let inbound = BTreeMap::from([(peer.clone(), BTreeSet::from([10, 11]))]);
        let snapshot = RecoverySnapshot {
            deliveries,
            inbound,
            last_delivery_id: 2,
            event_count: 5,
        };
        let plan = RetentionPlan {
            generated_at_ms: 100,
            prune_delivery_ids: vec![1],
            forget_inbound: vec![(peer.clone(), 10)],
            retained_terminal_count: 0,
            retained_inbound_counts: BTreeMap::from([(peer, 1)]),
            missing_terminal_timestamps: Vec::new(),
            missing_inbound_timestamps: Vec::new(),
        };
        (snapshot, plan)
    }

    #[test]
    fn accepts_consistent_plan() {
        let (snapshot, plan) = fixture();
        let report = verify_retention_plan(&snapshot, &plan);
        assert!(report.valid);
        assert_eq!(report.deliveries_after, 1);
        assert_eq!(report.inbound_ids_after, 1);
    }

    #[test]
    fn detects_active_delivery_prune_and_bad_counts() {
        let (snapshot, mut plan) = fixture();
        plan.prune_delivery_ids = vec![2];
        plan.retained_terminal_count = 1;
        let report = verify_retention_plan(&snapshot, &plan);
        assert!(!report.valid);
        assert!(report
            .issues
            .contains(&VerificationIssue::NonTerminalDelivery(2)));
    }

    #[test]
    fn detects_duplicate_and_unknown_inbound_removals() {
        let (snapshot, mut plan) = fixture();
        let peer = snapshot.inbound.keys().next().cloned().unwrap_or(PeerKey {
            tenant_id: "tenant-a".into(),
            peer_id: "peer-a".into(),
        });
        plan.forget_inbound = vec![(peer.clone(), 10), (peer.clone(), 10), (peer, 99)];
        let report = verify_retention_plan(&snapshot, &plan);
        assert!(!report.valid);
        assert!(report.issues.iter().any(|issue| matches!(
            issue,
            VerificationIssue::DuplicateInboundRemoval { frame_id: 10, .. }
        )));
        assert!(report.issues.iter().any(|issue| matches!(
            issue,
            VerificationIssue::UnknownInbound { frame_id: 99, .. }
        )));
    }
}
