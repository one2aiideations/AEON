use std::path::Path;
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::{
    ActionId, ActorId, AuditEntry, AuditOutcome, Journal, JournalRecord, PersistenceError,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery {
    pub actor: Option<ActorId>,
    pub action: Option<ActionId>,
    pub outcome: Option<AuditOutcome>,
    pub from_unix: Option<i64>,
    pub to_unix: Option<i64>,
}

impl AuditQuery {
    fn matches(&self, entry: &AuditEntry) -> bool {
        self.actor.is_none_or(|v| v == entry.actor)
            && self.action.is_none_or(|v| Some(v) == entry.action)
            && self.outcome.as_ref().is_none_or(|v| v == &entry.outcome)
            && self.from_unix.is_none_or(|v| entry.timestamp_unix >= v)
            && self.to_unix.is_none_or(|v| entry.timestamp_unix <= v)
    }
}

#[derive(Debug, Error)]
pub enum ExecutionAuditError {
    #[error("audit journal error: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("audit integrity violation at entry {0}")]
    IntegrityViolation(crate::AuditEntryId),
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionAuditLog {
    entries: Arc<Mutex<Vec<AuditEntry>>>,
    journal: Option<Arc<Mutex<Journal>>>,
    max_entries: Option<usize>,
}

impl ExecutionAuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::with_capacity(max_entries.min(1024)))),
            journal: None,
            max_entries: Some(max_entries),
        }
    }

    pub fn with_journal(path: impl AsRef<Path>) -> Result<Self, ExecutionAuditError> {
        let journal = Journal::open(path)?;
        let records: Vec<JournalRecord<AuditEntry>> = journal.read_all()?;
        let entries: Vec<AuditEntry> = records.into_iter().map(|record| record.payload).collect();
        Self::validate_chain(&entries)?;
        Ok(Self {
            entries: Arc::new(Mutex::new(entries)),
            journal: Some(Arc::new(Mutex::new(journal))),
            max_entries: None,
        })
    }

    pub fn validate_chain(entries: &[AuditEntry]) -> Result<(), ExecutionAuditError> {
        let mut previous = None;
        for entry in entries {
            if !entry.is_integrity_valid() || entry.previous_hash != previous {
                return Err(ExecutionAuditError::IntegrityViolation(entry.id));
            }
            previous = Some(entry.hash.clone());
        }
        Ok(())
    }

    pub fn append(&self, mut entry: AuditEntry) -> Result<(), ExecutionAuditError> {
        let mut entries = self.entries.lock().expect("audit log lock poisoned");
        if entry.previous_hash.is_none() {
            if let Some(previous) = entries.last() {
                entry.previous_hash = Some(previous.hash.clone());
                entry.hash = entry.compute_hash();
            }
        }
        if !entry.is_integrity_valid() {
            return Err(ExecutionAuditError::IntegrityViolation(entry.id));
        }
        if let Some(journal) = &self.journal {
            journal
                .lock()
                .expect("audit journal lock poisoned")
                .append(&entry)?;
        }
        entries.push(entry);
        if let Some(max_entries) = self.max_entries {
            if max_entries == 0 {
                entries.clear();
            } else if entries.len() > max_entries {
                let excess = entries.len() - max_entries;
                entries.drain(..excess);
            }
        }
        Ok(())
    }

    pub fn record(
        &self,
        actor: ActorId,
        action: Option<ActionId>,
        timestamp_unix: i64,
        outcome: AuditOutcome,
        detail: impl Into<String>,
    ) -> Result<(), ExecutionAuditError> {
        self.append(AuditEntry::new(
            actor,
            action,
            timestamp_unix,
            outcome,
            detail,
        ))
    }

    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .expect("audit log lock poisoned")
            .clone()
    }

    pub fn query(&self, query: &AuditQuery) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .expect("audit log lock poisoned")
            .iter()
            .filter(|entry| query.matches(entry))
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("audit log lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn verify_integrity(&self) -> Result<(), ExecutionAuditError> {
        let entries = self.entries();
        Self::validate_chain(&entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn audit_integrity_forms_a_chain() {
        let log = ExecutionAuditLog::new();
        let actor = Uuid::new_v4();
        log.record(actor, None, 1, AuditOutcome::Started, "one")
            .expect("append");
        log.record(actor, None, 2, AuditOutcome::Succeeded, "two")
            .expect("append");

        let entries = log.entries();
        assert!(entries.iter().all(AuditEntry::is_integrity_valid));
        assert_eq!(
            entries[1].previous_hash.as_deref(),
            Some(entries[0].hash.as_str())
        );
    }

    #[test]
    fn tampered_entry_is_rejected() {
        let log = ExecutionAuditLog::new();
        let actor = Uuid::new_v4();
        let mut entry = AuditEntry::new(actor, None, 1, AuditOutcome::Started, "one");
        entry.detail = "tampered".into();
        assert!(matches!(
            log.append(entry),
            Err(ExecutionAuditError::IntegrityViolation(_))
        ));
    }

    #[test]
    fn audit_query_filters_by_actor_and_outcome() {
        let log = ExecutionAuditLog::new();
        let actor = Uuid::new_v4();
        let other = Uuid::new_v4();
        log.record(actor, None, 1, AuditOutcome::Started, "started")
            .expect("append");
        log.record(actor, None, 2, AuditOutcome::Succeeded, "succeeded")
            .expect("append");
        log.record(other, None, 3, AuditOutcome::Succeeded, "other")
            .expect("append");

        let query = AuditQuery {
            actor: Some(actor),
            outcome: Some(AuditOutcome::Succeeded),
            ..Default::default()
        };
        let entries = log.query(&query);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].timestamp_unix, 2);
    }

    #[test]
    fn audit_query_filters_time_range() {
        let log = ExecutionAuditLog::new();
        let actor = Uuid::new_v4();
        log.record(actor, None, 10, AuditOutcome::Started, "before")
            .expect("append");
        log.record(actor, None, 20, AuditOutcome::Succeeded, "inside")
            .expect("append");
        log.record(actor, None, 30, AuditOutcome::Succeeded, "after")
            .expect("append");

        let query = AuditQuery {
            from_unix: Some(15),
            to_unix: Some(25),
            ..Default::default()
        };
        let entries = log.query(&query);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].timestamp_unix, 20);
    }

    #[test]
    fn audit_log_preserves_execution_order() {
        let log = ExecutionAuditLog::new();
        let actor = Uuid::new_v4();
        let action = Uuid::new_v4();

        log.record(actor, Some(action), 1, AuditOutcome::Started, "started")
            .expect("append");
        log.record(actor, Some(action), 2, AuditOutcome::Succeeded, "succeeded")
            .expect("append");

        let entries = log.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].outcome, AuditOutcome::Started);
        assert_eq!(entries[1].outcome, AuditOutcome::Succeeded);
    }
}
