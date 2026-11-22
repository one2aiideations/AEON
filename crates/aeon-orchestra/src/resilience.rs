//! Resilience primitives: circuit breaker, retry backoff, leased job
//! queue and leader lease. Times are milliseconds.

use std::collections::{BTreeMap, VecDeque};

/// Breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Calls flow.
    Closed,
    /// Calls are refused until `until`.
    Open {
        /// Reopen probe time.
        until: u64,
    },
    /// One trial call allowed.
    HalfOpen,
}

/// Circuit breaker around a flaky dependency (model server, network).
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    state: BreakerState,
    failures: u32,
    /// Consecutive failures that open the breaker.
    pub threshold: u32,
    /// How long it stays open.
    pub cooldown_ms: u64,
}

impl CircuitBreaker {
    /// Breaker opening after `threshold` failures for `cooldown_ms`.
    #[must_use]
    pub fn new(threshold: u32, cooldown_ms: u64) -> Self {
        Self {
            state: BreakerState::Closed,
            failures: 0,
            threshold: threshold.max(1),
            cooldown_ms,
        }
    }

    /// Whether a call may go out at `now`.
    pub fn allow(&mut self, now: u64) -> bool {
        match self.state {
            BreakerState::Closed | BreakerState::HalfOpen => true,
            BreakerState::Open { until } if now >= until => {
                self.state = BreakerState::HalfOpen;
                true
            }
            BreakerState::Open { .. } => false,
        }
    }

    /// Report a call's result.
    pub fn record(&mut self, now: u64, success: bool) {
        if success {
            self.failures = 0;
            self.state = BreakerState::Closed;
            return;
        }
        self.failures += 1;
        if self.state == BreakerState::HalfOpen || self.failures >= self.threshold {
            self.state = BreakerState::Open {
                until: now + self.cooldown_ms,
            };
        }
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> BreakerState {
        self.state
    }
}

/// Exponential backoff with deterministic jitter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backoff {
    /// First delay.
    pub base_ms: u64,
    /// Upper bound.
    pub max_ms: u64,
    /// Attempts allowed (including the first).
    pub max_attempts: u32,
}

impl Backoff {
    /// Delay before retry number `attempt` (1-based), or `None` when out of
    /// attempts. `seed` spreads retries of different jobs (up to +25%).
    #[must_use]
    pub fn delay(&self, attempt: u32, seed: u64) -> Option<u64> {
        if attempt == 0 || attempt >= self.max_attempts {
            return None;
        }
        let exp = self
            .base_ms
            .saturating_mul(1u64 << (attempt - 1).min(32))
            .min(self.max_ms);
        let mix = seed
            .wrapping_add(u64::from(attempt))
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            >> 33;
        let jitter = exp / 4 * (mix % 1000) / 1000;
        Some(exp + jitter)
    }
}

/// A job in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Id.
    pub id: u64,
    /// Payload.
    pub payload: String,
    /// Deliveries so far.
    pub deliveries: u32,
}

/// At-least-once queue: claimed jobs are leased; unacknowledged jobs
/// return to the queue when the lease expires. Jobs delivered too often go
/// to the dead-letter list.
#[derive(Debug, Clone)]
pub struct LeasedQueue {
    ready: VecDeque<Job>,
    leased: BTreeMap<u64, (Job, u64, u32)>,
    dead: Vec<Job>,
    next: u64,
    /// Lease length.
    pub lease_ms: u64,
    /// Deliveries before a job is dead-lettered.
    pub max_deliveries: u32,
}

impl LeasedQueue {
    /// Queue with the given lease length and delivery limit.
    #[must_use]
    pub fn new(lease_ms: u64, max_deliveries: u32) -> Self {
        Self {
            ready: VecDeque::new(),
            leased: BTreeMap::new(),
            dead: Vec::new(),
            next: 1,
            lease_ms,
            max_deliveries: max_deliveries.max(1),
        }
    }

    /// Add a job.
    pub fn push(&mut self, payload: &str) -> u64 {
        let id = self.next;
        self.next += 1;
        self.ready.push_back(Job {
            id,
            payload: payload.to_owned(),
            deliveries: 0,
        });
        id
    }

    fn expire(&mut self, now: u64) {
        let expired: Vec<u64> = self
            .leased
            .iter()
            .filter(|(_, (_, until, _))| *until <= now)
            .map(|(&id, _)| id)
            .collect();
        for id in expired {
            if let Some((job, _, _)) = self.leased.remove(&id) {
                if job.deliveries >= self.max_deliveries {
                    self.dead.push(job);
                } else {
                    self.ready.push_back(job);
                }
            }
        }
    }

    /// Claim the next job for `worker` at `now`.
    pub fn claim(&mut self, worker: u32, now: u64) -> Option<Job> {
        self.expire(now);
        let mut job = self.ready.pop_front()?;
        job.deliveries += 1;
        self.leased
            .insert(job.id, (job.clone(), now + self.lease_ms, worker));
        Some(job)
    }

    /// Acknowledge completion. Only the current lease holder may ack.
    pub fn ack(&mut self, id: u64, worker: u32) -> bool {
        match self.leased.get(&id) {
            Some((_, _, w)) if *w == worker => {
                self.leased.remove(&id);
                true
            }
            _ => false,
        }
    }

    /// Jobs waiting.
    #[must_use]
    pub fn ready_len(&self) -> usize {
        self.ready.len()
    }

    /// Jobs that failed too often.
    #[must_use]
    pub fn dead_letters(&self) -> &[Job] {
        &self.dead
    }
}

/// Leader lease: one coordinator at a time among cooperating processes.
#[derive(Debug, Clone, Default)]
pub struct LeaderLease {
    holder: Option<(u32, u64)>,
    term: u64,
}

impl LeaderLease {
    /// Try to become (or stay) leader until `now + ttl`. Returns the term
    /// on success; a higher term fences out the previous leader.
    pub fn acquire(&mut self, node: u32, now: u64, ttl: u64) -> Option<u64> {
        match self.holder {
            Some((h, until)) if h != node && until > now => None,
            Some((h, _)) if h == node => {
                self.holder = Some((node, now + ttl));
                Some(self.term)
            }
            _ => {
                self.term += 1;
                self.holder = Some((node, now + ttl));
                Some(self.term)
            }
        }
    }

    /// Current leader at `now`.
    #[must_use]
    pub fn leader(&self, now: u64) -> Option<u32> {
        self.holder
            .filter(|&(_, until)| until > now)
            .map(|(h, _)| h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_cycle() {
        let mut b = CircuitBreaker::new(2, 1000);
        assert!(b.allow(0));
        b.record(0, false);
        assert!(b.allow(1));
        b.record(1, false);
        assert_eq!(b.state(), BreakerState::Open { until: 1001 });
        assert!(!b.allow(500));
        assert!(b.allow(1001));
        assert_eq!(b.state(), BreakerState::HalfOpen);
        b.record(1001, false);
        assert!(!b.allow(1500), "failed probe reopens");
        assert!(b.allow(2001));
        b.record(2001, true);
        assert_eq!(b.state(), BreakerState::Closed);
    }

    #[test]
    fn backoff_grows_and_caps() {
        let b = Backoff {
            base_ms: 100,
            max_ms: 1000,
            max_attempts: 6,
        };
        let d1 = b.delay(1, 7).unwrap();
        let d3 = b.delay(3, 7).unwrap();
        assert!((100..=125).contains(&d1));
        assert!((400..=500).contains(&d3));
        assert!(b.delay(5, 7).unwrap() <= 1250);
        assert!(b.delay(6, 7).is_none());
        assert!(b.delay(0, 7).is_none());
    }

    #[test]
    fn leased_queue_redelivers_and_dead_letters() {
        let mut q = LeasedQueue::new(100, 2);
        let id = q.push("summarise inbox");
        let j = q.claim(1, 0).unwrap();
        assert_eq!(j.id, id);
        assert!(q.claim(2, 50).is_none());
        let j = q.claim(2, 100).unwrap();
        assert_eq!(j.deliveries, 2);
        assert!(!q.ack(id, 1), "old lease holder cannot ack");
        assert!(q.claim(3, 200).is_none());
        assert_eq!(q.dead_letters().len(), 1);
        let id2 = q.push("ok job");
        q.claim(4, 300).unwrap();
        assert!(q.ack(id2, 4));
        assert_eq!(q.ready_len(), 0);
    }

    #[test]
    fn leader_lease_fencing() {
        let mut l = LeaderLease::default();
        assert_eq!(l.acquire(1, 0, 100), Some(1));
        assert_eq!(l.acquire(2, 50, 100), None);
        assert_eq!(l.acquire(1, 90, 100), Some(1), "renewal keeps term");
        assert_eq!(l.leader(150), Some(1));
        assert_eq!(l.acquire(2, 200, 100), Some(2));
        assert_eq!(l.leader(250), Some(2));
        assert_eq!(l.leader(400), None);
    }
}
