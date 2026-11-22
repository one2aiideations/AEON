//! Runs due jobs through an executor and audits each run.

use crate::audit::AuditLog;
use crate::scheduler::{Job, Scheduler};

/// Executes a job's action. Returns an outcome summary or an error message.
pub trait Executor {
    /// Run `job` at time `now`.
    fn run(&mut self, job: &Job, now: u64) -> Result<String, String>;
}

impl<F: FnMut(&Job, u64) -> Result<String, String>> Executor for F {
    fn run(&mut self, job: &Job, now: u64) -> Result<String, String> {
        self(job, now)
    }
}

/// Scheduler plus audit log.
#[derive(Debug, Clone)]
pub struct Runner {
    /// Job table.
    pub scheduler: Scheduler,
    /// Audit trail of every run.
    pub audit: AuditLog,
}

impl Runner {
    /// New runner.
    #[must_use]
    pub fn new(scheduler: Scheduler) -> Self {
        Self {
            scheduler,
            audit: AuditLog::new(),
        }
    }

    /// Run every job due at `now`, at most `limit` runs. Returns runs made.
    pub fn tick(&mut self, now: u64, limit: usize, exec: &mut dyn Executor) -> usize {
        let mut runs = 0;
        while runs < limit {
            let Some(job) = self.scheduler.pop_due(now) else {
                break;
            };
            let result = exec.run(&job, now);
            let (ok, outcome) = match &result {
                Ok(s) => (true, format!("ok: {s}")),
                Err(e) => (false, format!("error: {e}")),
            };
            self.audit.append(
                now,
                &format!("agent:{}", job.owner),
                &job.action,
                &format!("job:{}", job.id),
                &outcome,
            );
            // The job was popped, so it still exists unless the executor removed it.
            let _ = self.scheduler.complete(job.id, now, ok);
            runs += 1;
        }
        runs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Schedule;

    #[test]
    fn runs_and_audits() {
        let mut r = Runner::new(Scheduler::new(4));
        r.scheduler
            .add("alice", "daily digest", Schedule::Every { start: 0, period: 100 }, 0)
            .unwrap();
        r.scheduler
            .add("bob", "flaky sync", Schedule::Once { at: 0 }, 0)
            .unwrap();
        let mut exec = |job: &Job, _now: u64| {
            if job.owner == "bob" {
                Err("network down".to_string())
            } else {
                Ok("3 items".to_string())
            }
        };
        assert_eq!(r.tick(0, 10, &mut exec), 2);
        assert_eq!(r.tick(50, 10, &mut exec), 0);
        assert_eq!(r.tick(100, 10, &mut exec), 1);
        assert_eq!(r.audit.entries().len(), 3);
        assert!(r.audit.entries().iter().any(|e| e.outcome == "error: network down"));
        assert_eq!(r.audit.verify(), None);
    }

    #[test]
    fn limit_caps_runs() {
        let mut r = Runner::new(Scheduler::new(4));
        for i in 0..3 {
            r.scheduler
                .add("u", &format!("job {i}"), Schedule::Once { at: 0 }, 0)
                .unwrap();
        }
        let mut exec = |_: &Job, _: u64| -> Result<String, String> { Ok(String::new()) };
        assert_eq!(r.tick(0, 2, &mut exec), 2);
        assert_eq!(r.tick(0, 2, &mut exec), 1);
    }
}
