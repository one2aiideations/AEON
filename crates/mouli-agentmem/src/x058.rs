//! Bounded, lease-based delivery tracking for replay checkpoint delta frames.
//!
//! The outbox provides deterministic claiming, acknowledgement, retry scheduling,
//! and terminal failure after a configured attempt limit. The inbound ledger
//! provides bounded idempotency keys per authenticated sender identity.
//! Authentication, durable persistence, and transport are caller responsibilities.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PeerKey {
    pub tenant_id: String,
    pub peer_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryConfig {
    pub max_pending: usize,
    pub max_frame_bytes: usize,
    pub max_attempts: u32,
    pub lease_ms: u64,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
    pub max_inbound_keys_per_peer: usize,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            max_pending: 4096,
            max_frame_bytes: 4 * 1024 * 1024,
            max_attempts: 8,
            lease_ms: 30_000,
            retry_base_ms: 1_000,
            retry_max_ms: 60_000,
            max_inbound_keys_per_peer: 8192,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryError {
    InvalidConfig,
    InvalidIdentity,
    EmptyFrame,
    FrameTooLarge,
    QueueFull,
    SequenceExhausted,
    UnknownDelivery,
    LeaseNotActive,
    LeaseExpired,
    AttemptLimitReached,
    InboundCapacity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryState {
    Pending { available_at_ms: u64 },
    Leased { lease_until_ms: u64 },
    Delivered,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryRecord {
    pub delivery_id: u64,
    pub peer: PeerKey,
    pub frame: Vec<u8>,
    pub attempts: u32,
    pub state: DeliveryState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryLease {
    pub delivery_id: u64,
    pub peer: PeerKey,
    pub frame: Vec<u8>,
    pub attempt: u32,
    pub lease_until_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryStats {
    pub pending: usize,
    pub leased: usize,
    pub delivered: usize,
    pub failed: usize,
    pub next_delivery_id: u64,
}

#[derive(Clone, Debug)]
pub struct DeltaDelivery {
    config: DeliveryConfig,
    next_delivery_id: u64,
    records: BTreeMap<u64, DeliveryRecord>,
    inbound: BTreeMap<PeerKey, BTreeSet<u64>>,
}

impl DeltaDelivery {
    pub fn new(config: DeliveryConfig) -> Result<Self, DeliveryError> {
        if config.max_pending == 0
            || config.max_frame_bytes == 0
            || config.max_attempts == 0
            || config.lease_ms == 0
            || config.retry_base_ms == 0
            || config.retry_max_ms < config.retry_base_ms
            || config.max_inbound_keys_per_peer == 0
        {
            return Err(DeliveryError::InvalidConfig);
        }
        Ok(Self {
            config,
            next_delivery_id: 1,
            records: BTreeMap::new(),
            inbound: BTreeMap::new(),
        })
    }

    fn validate_peer(peer: &PeerKey) -> Result<(), DeliveryError> {
        if peer.tenant_id.trim().is_empty() || peer.peer_id.trim().is_empty() {
            return Err(DeliveryError::InvalidIdentity);
        }
        Ok(())
    }

    /// Enqueues an opaque encoded delta frame for a tenant/peer destination.
    pub fn enqueue(
        &mut self,
        peer: PeerKey,
        frame: Vec<u8>,
        now_ms: u64,
    ) -> Result<u64, DeliveryError> {
        Self::validate_peer(&peer)?;
        if frame.is_empty() {
            return Err(DeliveryError::EmptyFrame);
        }
        if frame.len() > self.config.max_frame_bytes {
            return Err(DeliveryError::FrameTooLarge);
        }
        if self
            .records
            .values()
            .filter(|record| {
                matches!(
                    record.state,
                    DeliveryState::Pending { .. } | DeliveryState::Leased { .. }
                )
            })
            .count()
            >= self.config.max_pending
        {
            return Err(DeliveryError::QueueFull);
        }

        let id = self.next_delivery_id;
        let next = id.checked_add(1).ok_or(DeliveryError::SequenceExhausted)?;
        self.records.insert(
            id,
            DeliveryRecord {
                delivery_id: id,
                peer,
                frame,
                attempts: 0,
                state: DeliveryState::Pending {
                    available_at_ms: now_ms,
                },
            },
        );
        self.next_delivery_id = next;
        Ok(id)
    }

    /// Claims the lowest-ID eligible delivery and establishes a bounded lease.
    /// Expired leases become retryable; deliveries at their attempt limit fail.
    pub fn claim(&mut self, now_ms: u64) -> Option<DeliveryLease> {
        let ids: Vec<u64> = self.records.keys().copied().collect();
        for id in ids {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            if let DeliveryState::Leased { lease_until_ms } = record.state {
                if lease_until_ms > now_ms {
                    continue;
                }
                if record.attempts >= self.config.max_attempts {
                    record.state = DeliveryState::Failed;
                    continue;
                }
                let delay = retry_delay(
                    self.config.retry_base_ms,
                    self.config.retry_max_ms,
                    record.attempts,
                );
                let available = lease_until_ms.saturating_add(delay);
                record.state = DeliveryState::Pending {
                    available_at_ms: available,
                };
            }

            let available_at_ms = match record.state {
                DeliveryState::Pending { available_at_ms } => available_at_ms,
                _ => continue,
            };
            if available_at_ms > now_ms {
                continue;
            }
            if record.attempts >= self.config.max_attempts {
                record.state = DeliveryState::Failed;
                continue;
            }
            let attempt = match record.attempts.checked_add(1) {
                Some(value) => value,
                None => {
                    record.state = DeliveryState::Failed;
                    continue;
                }
            };
            let lease_until_ms = now_ms.saturating_add(self.config.lease_ms);
            record.attempts = attempt;
            record.state = DeliveryState::Leased { lease_until_ms };
            return Some(DeliveryLease {
                delivery_id: id,
                peer: record.peer.clone(),
                frame: record.frame.clone(),
                attempt,
                lease_until_ms,
            });
        }
        None
    }

    /// Acknowledges a currently active, unexpired lease.
    pub fn acknowledge(&mut self, delivery_id: u64, now_ms: u64) -> Result<(), DeliveryError> {
        let record = self
            .records
            .get_mut(&delivery_id)
            .ok_or(DeliveryError::UnknownDelivery)?;
        match record.state {
            DeliveryState::Leased { lease_until_ms } if lease_until_ms > now_ms => {
                record.state = DeliveryState::Delivered;
                Ok(())
            }
            DeliveryState::Leased { .. } => Err(DeliveryError::LeaseExpired),
            _ => Err(DeliveryError::LeaseNotActive),
        }
    }

    /// Releases a lease for retry, using capped exponential backoff.
    pub fn retry(&mut self, delivery_id: u64, now_ms: u64) -> Result<u64, DeliveryError> {
        let record = self
            .records
            .get_mut(&delivery_id)
            .ok_or(DeliveryError::UnknownDelivery)?;
        match record.state {
            DeliveryState::Leased { lease_until_ms } if lease_until_ms > now_ms => {}
            DeliveryState::Leased { .. } => return Err(DeliveryError::LeaseExpired),
            _ => return Err(DeliveryError::LeaseNotActive),
        }
        if record.attempts >= self.config.max_attempts {
            record.state = DeliveryState::Failed;
            return Err(DeliveryError::AttemptLimitReached);
        }
        let delay = retry_delay(
            self.config.retry_base_ms,
            self.config.retry_max_ms,
            record.attempts.saturating_sub(1),
        );
        let available_at_ms = now_ms.saturating_add(delay);
        record.state = DeliveryState::Pending { available_at_ms };
        Ok(available_at_ms)
    }

    /// Records an inbound frame identity once per tenant/peer. Returns false for
    /// a duplicate. Caller should invoke only after authenticating the peer.
    pub fn record_inbound(&mut self, peer: PeerKey, frame_id: u64) -> Result<bool, DeliveryError> {
        Self::validate_peer(&peer)?;
        let keys = self.inbound.entry(peer).or_default();
        if keys.contains(&frame_id) {
            return Ok(false);
        }
        if keys.len() >= self.config.max_inbound_keys_per_peer {
            return Err(DeliveryError::InboundCapacity);
        }
        keys.insert(frame_id);
        Ok(true)
    }

    /// Removes an inbound deduplication entry, for explicit retention management.
    pub fn forget_inbound(&mut self, peer: &PeerKey, frame_id: u64) -> bool {
        let removed = self
            .inbound
            .get_mut(peer)
            .map(|keys| keys.remove(&frame_id))
            .unwrap_or(false);
        if self.inbound.get(peer).is_some_and(BTreeSet::is_empty) {
            self.inbound.remove(peer);
        }
        removed
    }

    pub fn get(&self, delivery_id: u64) -> Option<&DeliveryRecord> {
        self.records.get(&delivery_id)
    }

    /// Removes terminal records and returns the number removed.
    pub fn prune_terminal(&mut self) -> usize {
        let before = self.records.len();
        self.records.retain(|_, record| {
            !matches!(
                record.state,
                DeliveryState::Delivered | DeliveryState::Failed
            )
        });
        before.saturating_sub(self.records.len())
    }

    pub fn stats(&self) -> DeliveryStats {
        let mut stats = DeliveryStats {
            pending: 0,
            leased: 0,
            delivered: 0,
            failed: 0,
            next_delivery_id: self.next_delivery_id,
        };
        for record in self.records.values() {
            match record.state {
                DeliveryState::Pending { .. } => stats.pending += 1,
                DeliveryState::Leased { .. } => stats.leased += 1,
                DeliveryState::Delivered => stats.delivered += 1,
                DeliveryState::Failed => stats.failed += 1,
            }
        }
        stats
    }
}

fn retry_delay(base: u64, maximum: u64, prior_attempts: u32) -> u64 {
    let shift = prior_attempts.min(63);
    let multiplier = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
    base.saturating_mul(multiplier).min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> PeerKey {
        PeerKey {
            tenant_id: "tenant-1".to_owned(),
            peer_id: "peer-1".to_owned(),
        }
    }

    fn config() -> DeliveryConfig {
        DeliveryConfig {
            max_pending: 2,
            max_frame_bytes: 32,
            max_attempts: 3,
            lease_ms: 10,
            retry_base_ms: 5,
            retry_max_ms: 20,
            max_inbound_keys_per_peer: 2,
        }
    }

    #[test]
    fn enqueues_claims_and_acknowledges() {
        let mut queue = DeltaDelivery::new(config()).expect("valid config");
        let id = queue.enqueue(peer(), vec![1, 2, 3], 100).expect("enqueue");
        let lease = queue.claim(100).expect("claim");
        assert_eq!(lease.delivery_id, id);
        assert_eq!(lease.attempt, 1);
        assert_eq!(queue.acknowledge(id, 105), Ok(()));
        assert_eq!(
            queue.get(id).map(|r| r.state.clone()),
            Some(DeliveryState::Delivered)
        );
    }

    #[test]
    fn retry_uses_backoff_and_reclaims() {
        let mut queue = DeltaDelivery::new(config()).expect("valid config");
        let id = queue.enqueue(peer(), vec![9], 0).expect("enqueue");
        assert!(queue.claim(0).is_some());
        assert_eq!(queue.retry(id, 1), Ok(6));
        assert!(queue.claim(5).is_none());
        assert_eq!(queue.claim(6).map(|lease| lease.attempt), Some(2));
    }

    #[test]
    fn expired_lease_is_not_acknowledged_and_is_retried_later() {
        let mut queue = DeltaDelivery::new(config()).expect("valid config");
        let id = queue.enqueue(peer(), vec![9], 0).expect("enqueue");
        assert!(queue.claim(0).is_some());
        assert_eq!(queue.acknowledge(id, 10), Err(DeliveryError::LeaseExpired));
        assert!(queue.claim(10).is_none());
        assert!(queue.claim(20).is_some());
    }

    #[test]
    fn attempt_limit_transitions_to_failed() {
        let mut queue = DeltaDelivery::new(config()).expect("valid config");
        let id = queue.enqueue(peer(), vec![9], 0).expect("enqueue");
        assert!(queue.claim(0).is_some());
        assert_eq!(queue.retry(id, 1), Ok(6));
        assert!(queue.claim(6).is_some());
        assert_eq!(queue.retry(id, 7), Ok(17));
        assert!(queue.claim(17).is_some());
        assert_eq!(queue.retry(id, 18), Err(DeliveryError::AttemptLimitReached));
        assert_eq!(
            queue.get(id).map(|r| r.state.clone()),
            Some(DeliveryState::Failed)
        );
    }

    #[test]
    fn inbound_keys_are_idempotent_and_peer_scoped() {
        let mut queue = DeltaDelivery::new(config()).expect("valid config");
        assert_eq!(queue.record_inbound(peer(), 42), Ok(true));
        assert_eq!(queue.record_inbound(peer(), 42), Ok(false));
        let other = PeerKey {
            tenant_id: "tenant-2".to_owned(),
            peer_id: "peer-1".to_owned(),
        };
        assert_eq!(queue.record_inbound(other, 42), Ok(true));
    }

    #[test]
    fn capacities_and_terminal_pruning_are_enforced() {
        let mut queue = DeltaDelivery::new(config()).expect("valid config");
        let first = queue.enqueue(peer(), vec![1], 0).expect("first");
        queue.enqueue(peer(), vec![2], 0).expect("second");
        assert_eq!(
            queue.enqueue(peer(), vec![3], 0),
            Err(DeliveryError::QueueFull)
        );
        assert!(queue.claim(0).is_some());
        assert_eq!(queue.acknowledge(first, 1), Ok(()));
        assert_eq!(queue.prune_terminal(), 1);
        assert_eq!(queue.stats().pending, 1);
    }
}
