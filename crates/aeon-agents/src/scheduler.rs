//! Job table and due queue.

use crate::schedule::Schedule;
use std::collections::{BTreeMap, BTreeSet};
use core::fmt;

/// Job id.
pub type JobId = u64;

/// A registered agent job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Id.
    pub id: JobId,
    /// Owner (user or system component).
    pub owner: String,
    /// What the agent should do (an instruction or a tool call name).
    pub action: String,
    /// When it runs.
    pub schedule: Schedule,
    /// Next run time, if any.
    pub next_run: Option<u64>,
    /// Paused jobs keep their schedule but are not handed out.
    pub paused: bool,
    /// Consecutive failures.
    pub failures: u32,
}

/// Scheduler errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedError {
    /// Schedule parameters invalid.
    BadSchedule,
    /// Owner or action empty.
    BadJob,
    /// No such job.
    Unknown,
    /// Job limit reached.
    Full,
}

impl fmt::Display for SchedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadSchedule => "invalid schedule",
            Self::BadJob => "owner and action are required",
            Self::Unknown => "unknown job",
            Self::Full => "job limit reached",
        })
    }
}

impl std::error::Error for SchedError {}

/// Holds jobs and orders them by next run time.
#[derive(Debug, Clone)]
pub struct Scheduler {
    jobs: BTreeMap<JobId, Job>,
    queue: BTreeSet<(u64, JobId)>,
    next_id: JobId,
    max_jobs: usize,
    /// Failures after which a job is paused automatically.
    pub max_failures: u32,
}

impl Scheduler {
    /// Scheduler with room for `max_jobs`.
    #[must_use]
    pub fn new(max_jobs: usize) -> Self {
        Self {
            jobs: BTreeMap::new(),
            queue: BTreeSet::new(),
            next_id: 1,
            max_jobs,
            max_failures: 3,
        }
    }

    fn enqueue(&mut self, id: JobId) {
        if let Some(j) = self.jobs.get(&id) {
            if let (Some(t), false) = (j.next_run, j.paused) {
                self.queue.insert((t, id));
            }
        }
    }

    fn dequeue(&mut self, id: JobId) {
        if let Some(t) = self.jobs.get(&id).and_then(|j| j.next_run) {
            self.queue.remove(&(t, id));
        }
    }

    /// Register a job; its first run is computed from `now`.
    pub fn add(&mut self, owner: &str, action: &str, schedule: Schedule, now: u64) -> Result<JobId, SchedError> {
        if !schedule.is_valid() {
            return Err(SchedError::BadSchedule);
        }
        if owner.trim().is_empty() || action.trim().is_empty() {
            return Err(SchedError::BadJob);
        }
        if self.jobs.len() >= self.max_jobs {
            return Err(SchedError::Full);
        }
        let id = self.next_id;
        self.next_id += 1;
        // A job due exactly now still runs.
        let next_run = schedule.next_at_or_after(now);
        self.jobs.insert(
            id,
            Job {
                id,
                owner: owner.to_owned(),
                action: action.to_owned(),
                schedule,
                next_run,
                paused: false,
                failures: 0,
            },
        );
        self.enqueue(id);
        Ok(id)
    }

    /// Remove a job.
    pub fn remove(&mut self, id: JobId) -> Result<Job, SchedError> {
        self.dequeue(id);
        self.jobs.remove(&id).ok_or(SchedError::Unknown)
    }

    /// Pause or resume.
    pub fn set_paused(&mut self, id: JobId, paused: bool) -> Result<(), SchedError> {
        self.dequeue(id);
        let j = self.jobs.get_mut(&id).ok_or(SchedError::Unknown)?;
        j.paused = paused;
        if !paused {
            j.failures = 0;
        }
        self.enqueue(id);
        Ok(())
    }

    /// Pop the earliest job due at or before `now`.
    pub fn pop_due(&mut self, now: u64) -> Option<Job> {
        let &(t, id) = self.queue.first()?;
        if t > now {
            return None;
        }
        self.queue.remove(&(t, id));
        self.jobs.get(&id).cloned()
    }

    /// Record the outcome of a run that started at `ran_at` and reschedule.
    pub fn complete(&mut self, id: JobId, ran_at: u64, ok: bool) -> Result<(), SchedError> {
        self.dequeue(id);
        let max_failures = self.max_failures;
        let j = self.jobs.get_mut(&id).ok_or(SchedError::Unknown)?;
        if ok {
            j.failures = 0;
        } else {
            j.failures += 1;
            if j.failures >= max_failures {
                j.paused = true;
            }
        }
        j.next_run = j.schedule.next_after(ran_at);
        self.enqueue(id);
        Ok(())
    }

    /// Earliest pending run time (for sleeping until then).
    #[must_use]
    pub fn next_wake(&self) -> Option<u64> {
        self.queue.first().map(|&(t, _)| t)
    }

    /// Look up a job.
    #[must_use]
    pub fn get(&self, id: JobId) -> Option<&Job> {
        self.jobs.get(&id)
    }

    /// Jobs of one owner.
    pub fn by_owner<'a>(&'a self, owner: &'a str) -> impl Iterator<Item = &'a Job> + 'a {
        self.jobs.values().filter(move |j| j.owner == owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_order_and_reschedule() {
        let mut s = Scheduler::new(8);
        let a = s.add("u", "summarise mail", Schedule::Every { start: 100, period: 60 }, 0).unwrap();
        let b = s.add("u", "backup", Schedule::Once { at: 50 }, 0).unwrap();
        assert_eq!(s.next_wake(), Some(50));
        assert!(s.pop_due(49).is_none());
        assert_eq!(s.pop_due(120).unwrap().id, b);
        s.complete(b, 120, true).unwrap();
        assert_eq!(s.get(b).unwrap().next_run, None);
        assert_eq!(s.pop_due(120).unwrap().id, a);
        s.complete(a, 120, true).unwrap();
        assert_eq!(s.get(a).unwrap().next_run, Some(160));
        assert!(s.pop_due(159).is_none());
        assert_eq!(s.by_owner("u").count(), 2);
    }

    #[test]
    fn failures_pause_job() {
        let mut s = Scheduler::new(2);
        s.max_failures = 2;
        let id = s.add("sys", "sync", Schedule::Every { start: 0, period: 10 }, 0).unwrap();
        for t in [0, 10] {
            assert_eq!(s.pop_due(t).unwrap().id, id);
            s.complete(id, t, false).unwrap();
        }
        assert!(s.get(id).unwrap().paused);
        assert!(s.pop_due(1000).is_none());
        s.set_paused(id, false).unwrap();
        assert_eq!(s.pop_due(1000).unwrap().id, id);
    }

    #[test]
    fn validation() {
        let mut s = Scheduler::new(1);
        assert_eq!(s.add("", "x", Schedule::Once { at: 1 }, 0), Err(SchedError::BadJob));
        assert_eq!(
            s.add("u", "x", Schedule::Every { start: 0, period: 0 }, 0),
            Err(SchedError::BadSchedule)
        );
        s.add("u", "x", Schedule::Once { at: 1 }, 0).unwrap();
        assert_eq!(s.add("u", "y", Schedule::Once { at: 1 }, 0), Err(SchedError::Full));
        assert_eq!(s.remove(99), Err(SchedError::Unknown));
    }
}
