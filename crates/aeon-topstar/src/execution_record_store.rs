use std::path::Path;
use std::sync::{Arc, Mutex};

use std::cmp::Ordering;
use thiserror::Error;

use crate::{
    ActionId, ActorId, ExecutionOutcome, ExecutionRecord, ExecutionRecordId, Journal,
    PersistenceError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionRecordCursor {
    pub started_at_unix: i64,
    pub record_id: ExecutionRecordId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecordPage {
    pub records: Vec<ExecutionRecord>,
    pub next_cursor: Option<ExecutionRecordCursor>,
    pub has_more: bool,
}

impl ExecutionRecordPage {
    pub fn empty() -> Self {
        Self {
            records: Vec::new(),
            next_cursor: None,
            has_more: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionRecordQuery {
    pub actor: Option<ActorId>,
    pub action: Option<ActionId>,
    pub record: Option<ExecutionRecordId>,
    pub outcome: Option<ExecutionOutcome>,
    pub from_unix: Option<i64>,
    pub to_unix: Option<i64>,
    pub completed_from_unix: Option<i64>,
    pub completed_to_unix: Option<i64>,
}

impl ExecutionRecordCursor {
    pub fn new(record: &ExecutionRecord) -> Self {
        Self {
            started_at_unix: record.started_at_unix,
            record_id: record.id,
        }
    }
}

impl ExecutionRecordQuery {
    pub fn validate(&self) -> Result<(), ExecutionRecordQueryError> {
        if let (Some(from), Some(to)) = (self.from_unix, self.to_unix) {
            if from > to {
                return Err(ExecutionRecordQueryError::InvalidStartTimeRange { from, to });
            }
        }
        if let (Some(from), Some(to)) = (self.completed_from_unix, self.completed_to_unix) {
            if from > to {
                return Err(ExecutionRecordQueryError::InvalidCompletionTimeRange { from, to });
            }
        }
        Ok(())
    }

    fn matches(&self, record: &ExecutionRecord) -> bool {
        self.actor.is_none_or(|actor| actor == record.actor)
            && self.action.is_none_or(|action| action == record.action)
            && self.record.is_none_or(|id| id == record.id)
            && self
                .outcome
                .as_ref()
                .is_none_or(|outcome| outcome == &record.outcome)
            && self
                .from_unix
                .is_none_or(|from| record.started_at_unix >= from)
            && self.to_unix.is_none_or(|to| record.started_at_unix <= to)
            && self
                .completed_from_unix
                .is_none_or(|from| record.completed_at_unix.is_some_and(|value| value >= from))
            && self
                .completed_to_unix
                .is_none_or(|to| record.completed_at_unix.is_some_and(|value| value <= to))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecutionRecordQueryError {
    #[error(
        "execution record query has an invalid start time range: from {from} is after to {to}"
    )]
    InvalidStartTimeRange { from: i64, to: i64 },
    #[error(
        "execution record query has an invalid completion time range: from {from} is after to {to}"
    )]
    InvalidCompletionTimeRange { from: i64, to: i64 },
}

#[derive(Debug, Error)]
pub enum ExecutionRecordStoreError {
    #[error("execution record journal error: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("execution record {0} already exists")]
    Duplicate(ExecutionRecordId),
    #[error("execution record {0} is not complete")]
    Incomplete(ExecutionRecordId),
}

#[derive(Debug, Clone)]
pub struct ExecutionRecordStore {
    records: Arc<Mutex<Vec<ExecutionRecord>>>,
    journal: Arc<Mutex<Journal>>,
}

impl ExecutionRecordStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExecutionRecordStoreError> {
        let journal = Journal::open(path)?;
        let records = journal
            .read_all::<ExecutionRecord>()?
            .into_iter()
            .map(|record| record.payload)
            .map(|record| {
                if record.is_complete() {
                    Ok(record)
                } else {
                    Err(ExecutionRecordStoreError::Incomplete(record.id))
                }
            })
            .collect::<Result<Vec<_>, ExecutionRecordStoreError>>()?;

        Ok(Self {
            records: Arc::new(Mutex::new(records)),
            journal: Arc::new(Mutex::new(journal)),
        })
    }

    pub fn append(&self, record: ExecutionRecord) -> Result<(), ExecutionRecordStoreError> {
        let mut records = self.records.lock().expect("execution record lock poisoned");
        if records.iter().any(|existing| existing.id == record.id) {
            return Err(ExecutionRecordStoreError::Duplicate(record.id));
        }
        if !record.is_complete() {
            return Err(ExecutionRecordStoreError::Incomplete(record.id));
        }

        self.journal
            .lock()
            .expect("execution record journal lock poisoned")
            .append(&record)?;

        records.push(record);
        Ok(())
    }

    pub fn get(&self, id: ExecutionRecordId) -> Option<ExecutionRecord> {
        self.records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .find(|record| record.id == id)
            .cloned()
    }

    pub fn try_query(
        &self,
        query: &ExecutionRecordQuery,
    ) -> Result<Vec<ExecutionRecord>, ExecutionRecordQueryError> {
        query.validate()?;
        Ok(self.query_page_unchecked(query, 0, usize::MAX))
    }

    pub fn query(&self, query: &ExecutionRecordQuery) -> Vec<ExecutionRecord> {
        self.try_query(query).unwrap_or_default()
    }

    pub fn try_query_cursor(
        &self,
        query: &ExecutionRecordQuery,
        cursor: Option<ExecutionRecordCursor>,
        limit: usize,
    ) -> Result<Vec<ExecutionRecord>, ExecutionRecordQueryError> {
        query.validate()?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        Ok(self.query_cursor_unchecked(query, cursor, limit))
    }

    pub fn query_cursor(
        &self,
        query: &ExecutionRecordQuery,
        cursor: Option<ExecutionRecordCursor>,
        limit: usize,
    ) -> Vec<ExecutionRecord> {
        self.try_query_cursor(query, cursor, limit)
            .unwrap_or_default()
    }

    fn query_cursor_unchecked(
        &self,
        query: &ExecutionRecordQuery,
        cursor: Option<ExecutionRecordCursor>,
        limit: usize,
    ) -> Vec<ExecutionRecord> {
        let mut matches = self
            .records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .filter(|record| query.matches(record))
            .filter(|record| {
                cursor.is_none_or(|cursor| {
                    (record.started_at_unix, record.id) > (cursor.started_at_unix, cursor.record_id)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|a, b| match a.started_at_unix.cmp(&b.started_at_unix) {
            Ordering::Equal => a.id.cmp(&b.id),
            ordering => ordering,
        });
        matches.truncate(limit);
        matches
    }

    pub fn try_query_cursor_page(
        &self,
        query: &ExecutionRecordQuery,
        cursor: Option<ExecutionRecordCursor>,
        limit: usize,
    ) -> Result<ExecutionRecordPage, ExecutionRecordQueryError> {
        query.validate()?;
        if limit == 0 {
            return Ok(ExecutionRecordPage::empty());
        }
        Ok(self.query_cursor_page_unchecked(query, cursor, limit))
    }

    pub fn query_cursor_page(
        &self,
        query: &ExecutionRecordQuery,
        cursor: Option<ExecutionRecordCursor>,
        limit: usize,
    ) -> ExecutionRecordPage {
        self.try_query_cursor_page(query, cursor, limit)
            .unwrap_or_else(|_| ExecutionRecordPage::empty())
    }

    fn query_cursor_page_unchecked(
        &self,
        query: &ExecutionRecordQuery,
        cursor: Option<ExecutionRecordCursor>,
        limit: usize,
    ) -> ExecutionRecordPage {
        let mut matches = self
            .records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .filter(|record| query.matches(record))
            .filter(|record| {
                cursor.is_none_or(|cursor| {
                    (record.started_at_unix, record.id) > (cursor.started_at_unix, cursor.record_id)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|a, b| match a.started_at_unix.cmp(&b.started_at_unix) {
            Ordering::Equal => a.id.cmp(&b.id),
            ordering => ordering,
        });
        let has_more = matches.len() > limit;
        matches.truncate(limit);
        let next_cursor = if has_more {
            matches.last().map(ExecutionRecordCursor::new)
        } else {
            None
        };
        ExecutionRecordPage {
            records: matches,
            next_cursor,
            has_more,
        }
    }

    pub fn try_query_page(
        &self,
        query: &ExecutionRecordQuery,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<ExecutionRecord>, ExecutionRecordQueryError> {
        query.validate()?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        Ok(self.query_page_unchecked(query, offset, limit))
    }

    pub fn query_page(
        &self,
        query: &ExecutionRecordQuery,
        offset: usize,
        limit: usize,
    ) -> Vec<ExecutionRecord> {
        self.try_query_page(query, offset, limit)
            .unwrap_or_default()
    }

    fn query_page_unchecked(
        &self,
        query: &ExecutionRecordQuery,
        offset: usize,
        limit: usize,
    ) -> Vec<ExecutionRecord> {
        let mut matches = self
            .records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .filter(|record| query.matches(record))
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|a, b| match a.started_at_unix.cmp(&b.started_at_unix) {
            Ordering::Equal => a.id.cmp(&b.id),
            ordering => ordering,
        });
        matches.into_iter().skip(offset).take(limit).collect()
    }

    pub fn all(&self) -> Vec<ExecutionRecord> {
        self.records
            .lock()
            .expect("execution record lock poisoned")
            .clone()
    }

    pub fn len(&self) -> usize {
        self.records
            .lock()
            .expect("execution record lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionOutcome, ResponsibilityContractId};
    use std::time::{SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

    fn temporary_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("topstar-execution-records-{nonce}.log"))
    }

    fn record(actor: ActorId, action: ActionId) -> ExecutionRecord {
        ExecutionRecord::started(
            actor,
            action,
            Uuid::new_v4(),
            ResponsibilityContractId::new_v4(),
            10,
        )
    }

    #[test]
    fn append_and_recover_records() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let mut stored = record(actor, action);
        stored
            .complete(20, ExecutionOutcome::Succeeded)
            .expect("completion should succeed");

        {
            let store = ExecutionRecordStore::open(&path).expect("store should open");
            store.append(stored.clone()).expect("append should succeed");
            assert_eq!(store.get(stored.id), Some(stored.clone()));
        }

        let recovered = ExecutionRecordStore::open(&path).expect("store should recover");
        assert_eq!(recovered.all(), vec![stored]);

        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn incomplete_records_are_rejected() {
        let path = temporary_path();
        let store = ExecutionRecordStore::open(&path).expect("store should open");
        let stored = record(ActorId::new_v4(), ActionId::new_v4());

        assert!(matches!(
            store.append(stored.clone()),
            Err(ExecutionRecordStoreError::Incomplete(id)) if id == stored.id
        ));
        assert!(store.is_empty());
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn duplicate_record_ids_are_rejected() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let mut stored = record(actor, action);
        stored
            .complete(20, ExecutionOutcome::Succeeded)
            .expect("completion should succeed");

        let store = ExecutionRecordStore::open(&path).expect("store should open");
        store
            .append(stored.clone())
            .expect("first append should succeed");

        assert!(matches!(
            store.append(stored.clone()),
            Err(ExecutionRecordStoreError::Duplicate(id)) if id == stored.id
        ));
        assert_eq!(store.len(), 1);

        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_filters_by_actor_and_action() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let other_actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let other_action = ActionId::new_v4();

        let store = ExecutionRecordStore::open(&path).expect("store should open");
        for (actor, action) in [
            (actor, action),
            (actor, other_action),
            (other_actor, action),
        ] {
            let mut stored = record(actor, action);
            stored
                .complete(20, ExecutionOutcome::Succeeded)
                .expect("completion should succeed");
            store.append(stored).expect("append should succeed");
        }

        assert_eq!(
            store
                .query(&ExecutionRecordQuery {
                    actor: Some(actor),
                    action: Some(action),
                    record: None,
                    ..Default::default()
                })
                .len(),
            1
        );

        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_filters_by_start_time_range() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let mut early = record(actor, action);
        early.started_at_unix = 10;
        early
            .complete(11, ExecutionOutcome::Succeeded)
            .expect("completion");
        let mut late = record(actor, action);
        late.started_at_unix = 30;
        late.complete(31, ExecutionOutcome::Succeeded)
            .expect("completion");

        let store = ExecutionRecordStore::open(&path).expect("store should open");
        store.append(early).expect("first append");
        store.append(late).expect("second append");

        let results = store.query(&ExecutionRecordQuery {
            actor: Some(actor),
            action: None,
            record: None,
            outcome: None,
            from_unix: Some(20),
            to_unix: Some(40),
            completed_from_unix: None,
            completed_to_unix: None,
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].started_at_unix, 30);
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_supports_open_ended_time_ranges() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let mut stored = record(actor, action);
        stored.started_at_unix = 50;
        stored
            .complete(50, ExecutionOutcome::Succeeded)
            .expect("completion");

        let store = ExecutionRecordStore::open(&path).expect("store should open");
        store.append(stored).expect("append");
        assert_eq!(
            store
                .query(&ExecutionRecordQuery {
                    actor: Some(actor),
                    action: None,
                    record: None,
                    outcome: None,
                    from_unix: Some(50),
                    to_unix: None,
                    completed_from_unix: None,
                    completed_to_unix: None,
                })
                .len(),
            1
        );
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_page_is_deterministic_and_bounded() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let store = ExecutionRecordStore::open(&path).expect("store should open");

        for timestamp in [10_i64, 20, 30, 40] {
            let mut stored = record(actor, action);
            stored.started_at_unix = timestamp;
            stored
                .complete(timestamp, ExecutionOutcome::Succeeded)
                .expect("completion");
            store.append(stored).expect("append");
        }

        let query = ExecutionRecordQuery {
            actor: Some(actor),
            action: Some(action),
            record: None,
            outcome: None,
            from_unix: None,
            to_unix: None,
            completed_from_unix: None,
            completed_to_unix: None,
        };
        let page = store.query_page(&query, 1, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].started_at_unix, 20);
        assert_eq!(page[1].started_at_unix, 30);
        assert!(store.query_page(&query, 0, 0).is_empty());
        assert!(store.query_page(&query, 10, 2).is_empty());

        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn cursor_query_is_stable_across_appends() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let store = ExecutionRecordStore::open(&path).expect("store should open");

        for timestamp in [10_i64, 20, 30] {
            let mut stored = record(actor, action);
            stored.started_at_unix = timestamp;
            stored
                .complete(timestamp, ExecutionOutcome::Succeeded)
                .expect("completion");
            store.append(stored).expect("append");
        }

        let query = ExecutionRecordQuery {
            actor: Some(actor),
            action: Some(action),
            record: None,
            outcome: None,
            from_unix: None,
            to_unix: None,
            completed_from_unix: None,
            completed_to_unix: None,
        };
        let first = store.query_cursor(&query, None, 1);
        let cursor = ExecutionRecordCursor::new(&first[0]);

        let mut appended = record(actor, action);
        appended.started_at_unix = 40;
        appended
            .complete(40, ExecutionOutcome::Succeeded)
            .expect("completion");
        store.append(appended).expect("append");

        let second = store.query_cursor(&query, Some(cursor), 10);
        assert_eq!(
            second.iter().map(|r| r.started_at_unix).collect::<Vec<_>>(),
            vec![20, 30, 40]
        );
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn cursor_query_orders_by_timestamp_then_id() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let store = ExecutionRecordStore::open(&path).expect("store should open");

        let mut first = record(actor, action);
        first.started_at_unix = 20;
        first
            .complete(20, ExecutionOutcome::Succeeded)
            .expect("completion");
        let mut second = record(actor, action);
        second.started_at_unix = 20;
        second
            .complete(20, ExecutionOutcome::Succeeded)
            .expect("completion");
        let expected = if first.id < second.id {
            vec![first.id, second.id]
        } else {
            vec![second.id, first.id]
        };

        store.append(first).expect("append");
        store.append(second).expect("append");

        let query = ExecutionRecordQuery {
            actor: Some(actor),
            action: Some(action),
            record: None,
            outcome: None,
            from_unix: None,
            to_unix: None,
            completed_from_unix: None,
            completed_to_unix: None,
        };
        let ids = store
            .query_cursor(&query, None, 10)
            .into_iter()
            .map(|r| r.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, expected);
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_filters_by_outcome() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let store = ExecutionRecordStore::open(&path).expect("store should open");

        for (timestamp, outcome) in [
            (10_i64, ExecutionOutcome::Succeeded),
            (20, ExecutionOutcome::Failed),
            (30, ExecutionOutcome::Cancelled),
        ] {
            let mut stored = record(actor, action);
            stored.started_at_unix = timestamp;
            stored.complete(timestamp, outcome).expect("completion");
            store.append(stored).expect("append");
        }

        let results = store.query(&ExecutionRecordQuery {
            actor: Some(actor),
            action: Some(action),
            record: None,
            outcome: Some(ExecutionOutcome::Failed),
            from_unix: None,
            to_unix: None,
            completed_from_unix: None,
            completed_to_unix: None,
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, ExecutionOutcome::Failed);
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_filters_by_completion_time() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let store = ExecutionRecordStore::open(&path).expect("store should open");

        for (started, completed) in [(10_i64, 15_i64), (20, 25), (30, 35)] {
            let mut stored = record(actor, action);
            stored.started_at_unix = started;
            stored
                .complete(completed, ExecutionOutcome::Succeeded)
                .expect("completion");
            store.append(stored).expect("append");
        }

        let results = store.query(&ExecutionRecordQuery {
            actor: Some(actor),
            action: Some(action),
            record: None,
            outcome: Some(ExecutionOutcome::Succeeded),
            from_unix: None,
            to_unix: None,
            completed_from_unix: Some(20),
            completed_to_unix: Some(30),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].completed_at_unix, Some(25));
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn try_query_surfaces_invalid_start_time_range() {
        let path = temporary_path();
        let store = ExecutionRecordStore::open(&path).expect("store should open");
        let query = ExecutionRecordQuery {
            from_unix: Some(20),
            to_unix: Some(10),
            ..ExecutionRecordQuery::default()
        };

        assert_eq!(
            store.try_query(&query),
            Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
        );
        assert_eq!(
            store.try_query_page(&query, 0, 10),
            Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
        );
        assert_eq!(
            store.try_query_cursor(&query, None, 10),
            Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
        );
        assert_eq!(
            store.try_query_cursor_page(&query, None, 10),
            Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
        );
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_rejects_invalid_time_range() {
        let query = ExecutionRecordQuery {
            actor: None,
            action: None,
            record: None,
            outcome: None,
            from_unix: Some(20),
            to_unix: Some(10),
            completed_from_unix: None,
            completed_to_unix: None,
        };
        assert_eq!(
            query.validate(),
            Err(ExecutionRecordQueryError::InvalidStartTimeRange { from: 20, to: 10 })
        );

        let path = temporary_path();
        let store = ExecutionRecordStore::open(&path).expect("store should open");
        assert!(store.query(&query).is_empty());
        assert!(store.query_page(&query, 0, 10).is_empty());
        assert!(store.query_cursor(&query, None, 10).is_empty());
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn query_rejects_invalid_completion_time_range() {
        let query = ExecutionRecordQuery {
            actor: None,
            action: None,
            record: None,
            outcome: None,
            from_unix: None,
            to_unix: None,
            completed_from_unix: Some(20),
            completed_to_unix: Some(10),
        };
        assert_eq!(
            query.validate(),
            Err(ExecutionRecordQueryError::InvalidCompletionTimeRange { from: 20, to: 10 })
        );

        let path = temporary_path();
        let store = ExecutionRecordStore::open(&path).expect("store should open");
        assert!(store.query_cursor(&query, None, 10).is_empty());
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn cursor_page_reports_continuation_metadata() {
        let path = temporary_path();
        let actor = ActorId::new_v4();
        let action = ActionId::new_v4();
        let store = ExecutionRecordStore::open(&path).expect("store should open");
        for timestamp in [10_i64, 20, 30] {
            let mut stored = record(actor, action);
            stored.started_at_unix = timestamp;
            stored
                .complete(timestamp, ExecutionOutcome::Succeeded)
                .expect("completion");
            store.append(stored).expect("append");
        }
        let query = ExecutionRecordQuery {
            actor: Some(actor),
            action: Some(action),
            record: None,
            outcome: None,
            from_unix: None,
            to_unix: None,
            completed_from_unix: None,
            completed_to_unix: None,
        };
        let page = store.query_cursor_page(&query, None, 2);
        assert_eq!(page.records.len(), 2);
        assert!(page.has_more);
        assert_eq!(page.next_cursor.map(|c| c.started_at_unix), Some(20));
        let next = store.query_cursor_page(&query, page.next_cursor, 2);
        assert_eq!(next.records.len(), 1);
        assert!(!next.has_more);
        assert!(next.next_cursor.is_none());
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn store_preserves_terminal_records() {
        let path = temporary_path();
        let mut stored = record(ActorId::new_v4(), ActionId::new_v4());
        stored
            .complete(20, ExecutionOutcome::Succeeded)
            .expect("completion should succeed");

        let store = ExecutionRecordStore::open(&path).expect("store should open");
        store.append(stored.clone()).expect("append should succeed");

        assert_eq!(
            store.get(stored.id).expect("record should exist").outcome,
            ExecutionOutcome::Succeeded
        );

        std::fs::remove_file(path).expect("temporary store should be removed");
    }
}
