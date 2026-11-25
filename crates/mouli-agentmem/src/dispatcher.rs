//! Lease-based dispatch coordination for replication repair tasks.
//!
//! This crate provides an in-memory state machine. It does not perform network I/O,
//! persist leases, or guarantee exactly-once delivery across process restarts.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct TaskKey {
    pub tenant_id: String,
    pub peer_id: String,
    pub cursor: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchTask {
    pub key: TaskKey,
    pub estimated_bytes: u64,
    pub priority: u8,
    pub available_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lease {
    pub token: u64,
    pub task: DispatchTask,
    pub expires_at_ms: u64,
    pub delivery_attempt: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchError {
    EmptyIdentity,
    DuplicateTask,
    CapacityReached,
    InvalidLeaseDuration,
    TokenExhausted,
    UnknownLease,
    LeaseExpired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchLimits {
    pub max_pending: usize,
    pub max_leased: usize,
    pub max_attempts: u32,
}

impl Default for DispatchLimits {
    fn default() -> Self {
        Self {
            max_pending: 8192,
            max_leased: 1024,
            max_attempts: 8,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    task: DispatchTask,
    attempts: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dispatcher {
    limits: DispatchLimits,
    pending: BTreeMap<TaskKey, Entry>,
    leased: BTreeMap<u64, Lease>,
    next_token: u64,
}

impl Dispatcher {
    pub fn new(limits: DispatchLimits) -> Self {
        Self {
            limits,
            pending: BTreeMap::new(),
            leased: BTreeMap::new(),
            next_token: 1,
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
    pub fn leased_len(&self) -> usize {
        self.leased.len()
    }

    pub fn enqueue(&mut self, task: DispatchTask) -> Result<(), DispatchError> {
        if task.key.tenant_id.trim().is_empty() || task.key.peer_id.trim().is_empty() {
            return Err(DispatchError::EmptyIdentity);
        }
        if self.pending.contains_key(&task.key)
            || self.leased.values().any(|lease| lease.task.key == task.key)
        {
            return Err(DispatchError::DuplicateTask);
        }
        if self.pending.len() >= self.limits.max_pending {
            return Err(DispatchError::CapacityReached);
        }
        self.pending
            .insert(task.key.clone(), Entry { task, attempts: 0 });
        Ok(())
    }

    /// Claims one available task, preferring higher priority and then earlier availability.
    pub fn claim(
        &mut self,
        now_ms: u64,
        lease_duration_ms: u64,
    ) -> Result<Option<Lease>, DispatchError> {
        if lease_duration_ms == 0 {
            return Err(DispatchError::InvalidLeaseDuration);
        }
        if self.leased.len() >= self.limits.max_leased {
            return Ok(None);
        }

        let selected_key = self
            .pending
            .values()
            .filter(|entry| {
                entry.task.available_at_ms <= now_ms && entry.attempts < self.limits.max_attempts
            })
            .min_by(|a, b| {
                b.task
                    .priority
                    .cmp(&a.task.priority)
                    .then_with(|| a.task.available_at_ms.cmp(&b.task.available_at_ms))
                    .then_with(|| a.task.key.cmp(&b.task.key))
            })
            .map(|entry| entry.task.key.clone());

        let Some(key) = selected_key else {
            return Ok(None);
        };
        let Some(entry) = self.pending.remove(&key) else {
            return Ok(None);
        };
        let token = self.next_token;
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or(DispatchError::TokenExhausted)?;
        let expires_at_ms = now_ms.saturating_add(lease_duration_ms);
        let lease = Lease {
            token,
            task: entry.task,
            expires_at_ms,
            delivery_attempt: entry.attempts.saturating_add(1),
        };
        self.leased.insert(token, lease.clone());
        Ok(Some(lease))
    }

    /// Acknowledges a lease only while it is still valid.
    pub fn acknowledge(&mut self, token: u64, now_ms: u64) -> Result<DispatchTask, DispatchError> {
        let lease = self.leased.get(&token).ok_or(DispatchError::UnknownLease)?;
        if now_ms >= lease.expires_at_ms {
            return Err(DispatchError::LeaseExpired);
        }
        self.leased
            .remove(&token)
            .map(|lease| lease.task)
            .ok_or(DispatchError::UnknownLease)
    }

    /// Releases a lease back to pending with a caller-selected retry time.
    pub fn release(&mut self, token: u64, available_at_ms: u64) -> Result<(), DispatchError> {
        let lease = self
            .leased
            .remove(&token)
            .ok_or(DispatchError::UnknownLease)?;
        if lease.delivery_attempt >= self.limits.max_attempts {
            return Ok(());
        }
        let mut task = lease.task;
        task.available_at_ms = available_at_ms;
        self.pending.insert(
            task.key.clone(),
            Entry {
                task,
                attempts: lease.delivery_attempt,
            },
        );
        Ok(())
    }

    /// Reclaims expired leases. Exhausted tasks are dropped and counted separately.
    pub fn reclaim_expired(&mut self, now_ms: u64) -> (usize, usize) {
        let expired_tokens: Vec<u64> = self
            .leased
            .iter()
            .filter(|(_, lease)| lease.expires_at_ms <= now_ms)
            .map(|(token, _)| *token)
            .collect();

        let mut requeued = 0_usize;
        let mut exhausted = 0_usize;
        for token in expired_tokens {
            if let Some(lease) = self.leased.remove(&token) {
                if lease.delivery_attempt >= self.limits.max_attempts {
                    exhausted = exhausted.saturating_add(1);
                } else {
                    let mut task = lease.task;
                    task.available_at_ms = now_ms;
                    self.pending.insert(
                        task.key.clone(),
                        Entry {
                            task,
                            attempts: lease.delivery_attempt,
                        },
                    );
                    requeued = requeued.saturating_add(1);
                }
            }
        }
        (requeued, exhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(peer: &str, priority: u8) -> DispatchTask {
        DispatchTask {
            key: TaskKey {
                tenant_id: "tenant-a".to_owned(),
                peer_id: peer.to_owned(),
                cursor: 7,
            },
            estimated_bytes: 512,
            priority,
            available_at_ms: 0,
        }
    }

    #[test]
    fn claim_prefers_priority_and_acknowledges() {
        let mut dispatcher = Dispatcher::new(DispatchLimits::default());
        assert!(dispatcher.enqueue(task("low", 1)).is_ok());
        assert!(dispatcher.enqueue(task("high", 9)).is_ok());
        let lease = dispatcher.claim(10, 100).ok().flatten();
        assert!(lease.is_some());
        let lease = match lease {
            Some(value) => value,
            None => return,
        };
        assert_eq!(lease.task.key.peer_id, "high");
        assert!(dispatcher.acknowledge(lease.token, 20).is_ok());
        assert_eq!(dispatcher.leased_len(), 0);
    }

    #[test]
    fn expired_lease_is_requeued() {
        let mut dispatcher = Dispatcher::new(DispatchLimits::default());
        assert!(dispatcher.enqueue(task("peer", 3)).is_ok());
        let lease = dispatcher.claim(10, 5).ok().flatten();
        let lease = match lease {
            Some(value) => value,
            None => return,
        };
        assert_eq!(dispatcher.reclaim_expired(15), (1, 0));
        assert_eq!(dispatcher.pending_len(), 1);
        assert_eq!(
            dispatcher.acknowledge(lease.token, 15),
            Err(DispatchError::UnknownLease)
        );
    }

    #[test]
    fn duplicate_is_rejected_while_task_is_leased() {
        let mut dispatcher = Dispatcher::new(DispatchLimits::default());
        assert!(dispatcher.enqueue(task("peer", 3)).is_ok());
        let lease = dispatcher.claim(0, 50).ok().flatten();
        assert!(lease.is_some());
        assert_eq!(
            dispatcher.enqueue(task("peer", 3)),
            Err(DispatchError::DuplicateTask)
        );
    }
}
