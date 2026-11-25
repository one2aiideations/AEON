//! Bounded, deterministic scheduling for agent-memory replication repair work.
//!
//! This crate plans work only. Callers own timers, transport, persistence, and execution.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairTask {
    pub tenant_id: String,
    pub peer_id: String,
    pub cursor: u64,
    pub estimated_bytes: u64,
    pub priority: u8,
    pub due_at_ms: u64,
    pub deadline_ms: Option<u64>,
    pub attempts: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduleError {
    EmptyIdentity,
    DuplicateTask,
    CapacityReached,
    InvalidDeadline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleLimits {
    pub max_tasks: usize,
    pub max_bytes_per_batch: u64,
    pub max_tasks_per_batch: usize,
    pub max_attempts: u32,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for ScheduleLimits {
    fn default() -> Self {
        Self {
            max_tasks: 4096,
            max_bytes_per_batch: 8 * 1024 * 1024,
            max_tasks_per_batch: 128,
            max_attempts: 12,
            base_backoff_ms: 1_000,
            max_backoff_ms: 300_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledBatch {
    pub tasks: Vec<RepairTask>,
    pub estimated_bytes: u64,
    pub deferred_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scheduler {
    limits: ScheduleLimits,
    tasks: BTreeMap<(String, String), RepairTask>,
}

impl Scheduler {
    pub fn new(limits: ScheduleLimits) -> Self {
        Self {
            limits,
            tasks: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn enqueue(&mut self, task: RepairTask) -> Result<(), ScheduleError> {
        if task.tenant_id.trim().is_empty() || task.peer_id.trim().is_empty() {
            return Err(ScheduleError::EmptyIdentity);
        }
        if task
            .deadline_ms
            .is_some_and(|deadline| deadline < task.due_at_ms)
        {
            return Err(ScheduleError::InvalidDeadline);
        }
        let key = (task.tenant_id.clone(), task.peer_id.clone());
        if self.tasks.contains_key(&key) {
            return Err(ScheduleError::DuplicateTask);
        }
        if self.tasks.len() >= self.limits.max_tasks {
            return Err(ScheduleError::CapacityReached);
        }
        self.tasks.insert(key, task);
        Ok(())
    }

    /// Selects due tasks in priority/deadline/fairness order without removing them.
    /// The caller acknowledges completed tasks with `complete`.
    pub fn plan_batch(&self, now_ms: u64) -> ScheduledBatch {
        let mut due: Vec<&RepairTask> = self
            .tasks
            .values()
            .filter(|task| task.due_at_ms <= now_ms && task.attempts < self.limits.max_attempts)
            .collect();

        due.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    a.deadline_ms
                        .unwrap_or(u64::MAX)
                        .cmp(&b.deadline_ms.unwrap_or(u64::MAX))
                })
                .then_with(|| a.tenant_id.cmp(&b.tenant_id))
                .then_with(|| a.peer_id.cmp(&b.peer_id))
        });

        let mut selected = Vec::new();
        let mut bytes = 0_u64;
        for task in due.iter() {
            if selected.len() >= self.limits.max_tasks_per_batch {
                break;
            }
            let next_bytes = bytes.saturating_add(task.estimated_bytes);
            if next_bytes > self.limits.max_bytes_per_batch {
                continue;
            }
            bytes = next_bytes;
            selected.push((*task).clone());
        }
        let deferred_count = due.len().saturating_sub(selected.len());
        ScheduledBatch {
            tasks: selected,
            estimated_bytes: bytes,
            deferred_count,
        }
    }

    pub fn complete(&mut self, tenant_id: &str, peer_id: &str) -> bool {
        self.tasks
            .remove(&(tenant_id.to_owned(), peer_id.to_owned()))
            .is_some()
    }

    /// Records a failed attempt and computes bounded exponential backoff.
    pub fn record_failure(&mut self, tenant_id: &str, peer_id: &str, now_ms: u64) -> Option<u64> {
        let key = (tenant_id.to_owned(), peer_id.to_owned());
        let task = self.tasks.get_mut(&key)?;
        task.attempts = task.attempts.saturating_add(1);
        if task.attempts >= self.limits.max_attempts {
            return None;
        }
        let shift = task.attempts.saturating_sub(1).min(63);
        let multiplier = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
        let delay = self
            .limits
            .base_backoff_ms
            .saturating_mul(multiplier)
            .min(self.limits.max_backoff_ms);
        let next_due = now_ms.saturating_add(delay);
        task.due_at_ms = next_due;
        Some(next_due)
    }

    pub fn remove_exhausted(&mut self) -> usize {
        let before = self.tasks.len();
        self.tasks
            .retain(|_, task| task.attempts < self.limits.max_attempts);
        before.saturating_sub(self.tasks.len())
    }

    pub fn task(&self, tenant_id: &str, peer_id: &str) -> Option<&RepairTask> {
        self.tasks.get(&(tenant_id.to_owned(), peer_id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(tenant: &str, peer: &str, priority: u8, due: u64, bytes: u64) -> RepairTask {
        RepairTask {
            tenant_id: tenant.to_owned(),
            peer_id: peer.to_owned(),
            cursor: 10,
            estimated_bytes: bytes,
            priority,
            due_at_ms: due,
            deadline_ms: None,
            attempts: 0,
        }
    }

    #[test]
    fn batch_respects_priority_and_budgets() {
        let mut limits = ScheduleLimits::default();
        limits.max_bytes_per_batch = 10;
        limits.max_tasks_per_batch = 2;
        let mut scheduler = Scheduler::new(limits);
        assert!(scheduler.enqueue(task("t1", "p1", 1, 0, 6)).is_ok());
        assert!(scheduler.enqueue(task("t2", "p2", 9, 0, 6)).is_ok());
        assert!(scheduler.enqueue(task("t3", "p3", 5, 0, 4)).is_ok());
        let batch = scheduler.plan_batch(0);
        assert_eq!(batch.tasks.len(), 2);
        assert_eq!(batch.tasks[0].peer_id, "p2");
        assert_eq!(batch.estimated_bytes, 10);
    }

    #[test]
    fn backoff_is_bounded_and_task_is_deferred() {
        let mut limits = ScheduleLimits::default();
        limits.base_backoff_ms = 10;
        limits.max_backoff_ms = 25;
        let mut scheduler = Scheduler::new(limits);
        assert!(scheduler.enqueue(task("t", "p", 1, 0, 1)).is_ok());
        assert_eq!(scheduler.record_failure("t", "p", 100), Some(110));
        assert!(scheduler.plan_batch(109).tasks.is_empty());
        assert_eq!(scheduler.plan_batch(110).tasks.len(), 1);
    }

    #[test]
    fn tenant_peer_identity_is_unique_and_completion_removes_task() {
        let mut scheduler = Scheduler::new(ScheduleLimits::default());
        assert!(scheduler.enqueue(task("tenant", "peer", 1, 0, 1)).is_ok());
        assert_eq!(
            scheduler.enqueue(task("tenant", "peer", 2, 0, 1)),
            Err(ScheduleError::DuplicateTask)
        );
        assert!(scheduler.complete("tenant", "peer"));
        assert!(!scheduler.complete("tenant", "peer"));
    }
}
