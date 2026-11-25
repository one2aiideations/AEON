//! Replication lag and freshness monitoring for AgentOS memory replicas.
//!
//! Pure evaluation logic. Callers supply source/destination watermarks and
//! timestamps; scheduling, metrics export, alert delivery, and durable history
//! are intentionally outside this crate.

use std::collections::BTreeMap;

pub type TenantId = u64;
pub type ReplicaId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReplicaPair {
    pub tenant_id: TenantId,
    pub source_replica: ReplicaId,
    pub destination_replica: ReplicaId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaSample {
    pub pair: ReplicaPair,
    pub source_sequence: u64,
    pub applied_sequence: u64,
    pub source_updated_at_ns: u64,
    pub destination_updated_at_ns: u64,
    pub sampled_at_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthState {
    Healthy,
    Lagging,
    Stale,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaHealth {
    pub pair: ReplicaPair,
    pub state: HealthState,
    pub sequence_lag: u64,
    pub freshness_lag_ns: u64,
    pub evaluated_at_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorError {
    ZeroCapacity,
    CapacityReached,
    FutureTimestamp,
    AppliedAheadOfSource,
}

#[derive(Clone, Debug)]
pub struct MonitorConfig {
    pub max_pairs: usize,
    pub healthy_sequence_lag: u64,
    pub stale_after_ns: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            max_pairs: 100_000,
            healthy_sequence_lag: 0,
            stale_after_ns: 60_000_000_000,
        }
    }
}

pub struct ReplicationMonitor {
    config: MonitorConfig,
    latest: BTreeMap<ReplicaPair, ReplicaHealth>,
}

impl ReplicationMonitor {
    pub fn new(config: MonitorConfig) -> Result<Self, MonitorError> {
        if config.max_pairs == 0 {
            return Err(MonitorError::ZeroCapacity);
        }
        Ok(Self {
            config,
            latest: BTreeMap::new(),
        })
    }

    pub fn evaluate(&mut self, sample: ReplicaSample) -> Result<&ReplicaHealth, MonitorError> {
        if sample.source_updated_at_ns > sample.sampled_at_ns
            || sample.destination_updated_at_ns > sample.sampled_at_ns
        {
            return Err(MonitorError::FutureTimestamp);
        }
        if sample.applied_sequence > sample.source_sequence {
            return Err(MonitorError::AppliedAheadOfSource);
        }
        if !self.latest.contains_key(&sample.pair) && self.latest.len() >= self.config.max_pairs {
            return Err(MonitorError::CapacityReached);
        }

        let sequence_lag = sample
            .source_sequence
            .saturating_sub(sample.applied_sequence);
        let freshness_lag_ns = sample
            .sampled_at_ns
            .saturating_sub(sample.destination_updated_at_ns);

        let state = if freshness_lag_ns >= self.config.stale_after_ns {
            HealthState::Stale
        } else if sequence_lag > self.config.healthy_sequence_lag {
            HealthState::Lagging
        } else {
            HealthState::Healthy
        };

        let health = ReplicaHealth {
            pair: sample.pair,
            state,
            sequence_lag,
            freshness_lag_ns,
            evaluated_at_ns: sample.sampled_at_ns,
        };
        self.latest.insert(sample.pair, health);
        // The key was inserted above, so this lookup is guaranteed by construction.
        match self.latest.get(&sample.pair) {
            Some(value) => Ok(value),
            None => Err(MonitorError::CapacityReached),
        }
    }

    pub fn get(&self, pair: ReplicaPair) -> Option<&ReplicaHealth> {
        self.latest.get(&pair)
    }

    pub fn remove(&mut self, pair: ReplicaPair) -> bool {
        self.latest.remove(&pair).is_some()
    }

    pub fn counts(&self) -> BTreeMap<HealthState, usize> {
        let mut counts = BTreeMap::new();
        for health in self.latest.values() {
            let count = counts.entry(health.state).or_insert(0usize);
            *count = count.saturating_add(1);
        }
        counts
    }

    pub fn len(&self) -> usize {
        self.latest.len()
    }

    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(tenant: u64, destination: u64) -> ReplicaPair {
        ReplicaPair {
            tenant_id: tenant,
            source_replica: 1,
            destination_replica: destination,
        }
    }

    fn sample(
        pair: ReplicaPair,
        source: u64,
        applied: u64,
        destination_time: u64,
    ) -> ReplicaSample {
        ReplicaSample {
            pair,
            source_sequence: source,
            applied_sequence: applied,
            source_updated_at_ns: 90,
            destination_updated_at_ns: destination_time,
            sampled_at_ns: 100,
        }
    }

    fn monitor() -> ReplicationMonitor {
        ReplicationMonitor::new(MonitorConfig {
            max_pairs: 4,
            healthy_sequence_lag: 2,
            stale_after_ns: 50,
        })
        .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn classifies_healthy_lagging_and_stale_replicas() {
        let mut monitor = monitor();
        assert_eq!(
            monitor
                .evaluate(sample(pair(1, 2), 10, 9, 99))
                .map(|h| h.state),
            Ok(HealthState::Healthy)
        );
        assert_eq!(
            monitor
                .evaluate(sample(pair(1, 3), 10, 7, 99))
                .map(|h| h.state),
            Ok(HealthState::Lagging)
        );
        assert_eq!(
            monitor
                .evaluate(sample(pair(1, 4), 10, 10, 40))
                .map(|h| h.state),
            Ok(HealthState::Stale)
        );
    }

    #[test]
    fn rejects_impossible_watermarks_and_future_times() {
        let mut monitor = monitor();
        assert_eq!(
            monitor.evaluate(sample(pair(1, 2), 5, 6, 99)).map(|_| ()),
            Err(MonitorError::AppliedAheadOfSource)
        );
        let mut future = sample(pair(1, 2), 5, 5, 99);
        future.sampled_at_ns = 98;
        assert_eq!(
            monitor.evaluate(future).map(|_| ()),
            Err(MonitorError::FutureTimestamp)
        );
    }

    #[test]
    fn bounds_pairs_and_reports_state_counts() {
        let mut monitor = monitor();
        assert!(monitor.evaluate(sample(pair(1, 2), 1, 1, 99)).is_ok());
        assert!(monitor.evaluate(sample(pair(1, 3), 5, 1, 99)).is_ok());
        assert_eq!(monitor.counts().get(&HealthState::Healthy), Some(&1));
        assert_eq!(monitor.counts().get(&HealthState::Lagging), Some(&1));
        assert!(monitor.remove(pair(1, 3)));
        assert_eq!(monitor.len(), 1);
    }
}
