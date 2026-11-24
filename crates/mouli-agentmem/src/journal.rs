//! Bounded, append-only-in-process audit journal for agent memory operations.
//!
//! Records decisions and lifecycle actions without storing memory contents.
//! This is an in-memory component, not tamper-proof or durable audit storage.

use std::collections::VecDeque;

pub type TenantId = u64;
pub type PrincipalId = u64;
pub type MemoryId = u128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryOperation {
    Read,
    Search,
    Append,
    Update,
    Delete,
    Share,
    Export,
    Import,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    Allowed,
    Denied,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEvent {
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub tenant_id: TenantId,
    pub principal_id: PrincipalId,
    pub operation: MemoryOperation,
    pub memory_id: Option<MemoryId>,
    pub outcome: AuditOutcome,
    /// Short, non-sensitive reason code; never put memory content here.
    pub reason_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    CapacityZero,
    ReasonTooLarge,
    SequenceExhausted,
    InvalidTimeOrder,
    InvalidRange,
}

pub struct AuditJournal {
    events: VecDeque<AuditEvent>,
    capacity: usize,
    next_sequence: u64,
    last_timestamp_ns: Option<u64>,
    max_reason_bytes: usize,
}

impl AuditJournal {
    pub fn new(capacity: usize, max_reason_bytes: usize) -> Result<Self, JournalError> {
        if capacity == 0 {
            return Err(JournalError::CapacityZero);
        }
        Ok(Self {
            events: VecDeque::with_capacity(capacity),
            capacity,
            next_sequence: 1,
            last_timestamp_ns: None,
            max_reason_bytes,
        })
    }

    /// Append an event. When full, the oldest event is evicted.
    /// Timestamps must be nondecreasing within this process.
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &mut self,
        timestamp_ns: u64,
        tenant_id: TenantId,
        principal_id: PrincipalId,
        operation: MemoryOperation,
        memory_id: Option<MemoryId>,
        outcome: AuditOutcome,
        reason_code: &str,
    ) -> Result<u64, JournalError> {
        if reason_code.len() > self.max_reason_bytes {
            return Err(JournalError::ReasonTooLarge);
        }
        if self
            .last_timestamp_ns
            .is_some_and(|last| timestamp_ns < last)
        {
            return Err(JournalError::InvalidTimeOrder);
        }
        let sequence = self.next_sequence;
        let next = sequence
            .checked_add(1)
            .ok_or(JournalError::SequenceExhausted)?;

        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(AuditEvent {
            sequence,
            timestamp_ns,
            tenant_id,
            principal_id,
            operation,
            memory_id,
            outcome,
            reason_code: reason_code.to_owned(),
        });
        self.next_sequence = next;
        self.last_timestamp_ns = Some(timestamp_ns);
        Ok(sequence)
    }

    /// Return events for one tenant and inclusive sequence bounds.
    pub fn read_range(
        &self,
        tenant_id: TenantId,
        start_sequence: u64,
        end_sequence: u64,
    ) -> Result<Vec<AuditEvent>, JournalError> {
        if start_sequence > end_sequence {
            return Err(JournalError::InvalidRange);
        }
        Ok(self
            .events
            .iter()
            .filter(|event| {
                event.tenant_id == tenant_id
                    && event.sequence >= start_sequence
                    && event.sequence <= end_sequence
            })
            .cloned()
            .collect())
    }

    pub fn oldest_sequence(&self) -> Option<u64> {
        self.events.front().map(|event| event.sequence)
    }

    pub fn newest_sequence(&self) -> Option<u64> {
        self.events.back().map(|event| event.sequence)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(journal: &mut AuditJournal, ts: u64, tenant: u64) -> Result<u64, JournalError> {
        journal.append(
            ts,
            tenant,
            9,
            MemoryOperation::Read,
            Some(42),
            AuditOutcome::Allowed,
            "policy_allow",
        )
    }

    #[test]
    fn journal_evicts_oldest_and_keeps_monotonic_sequence() {
        let mut journal = AuditJournal::new(2, 32).expect("valid capacity");
        assert_eq!(append(&mut journal, 1, 7), Ok(1));
        assert_eq!(append(&mut journal, 2, 7), Ok(2));
        assert_eq!(append(&mut journal, 3, 7), Ok(3));
        assert_eq!(journal.oldest_sequence(), Some(2));
        assert_eq!(journal.newest_sequence(), Some(3));
        assert_eq!(journal.len(), 2);
    }

    #[test]
    fn range_reads_are_tenant_scoped() {
        let mut journal = AuditJournal::new(8, 32).expect("valid capacity");
        assert!(append(&mut journal, 1, 7).is_ok());
        assert!(append(&mut journal, 2, 8).is_ok());
        let events = journal.read_range(7, 1, 10);
        assert_eq!(events.map(|items| items.len()), Ok(1));
    }

    #[test]
    fn rejects_bad_time_and_oversized_reason() {
        let mut journal = AuditJournal::new(4, 14).expect("valid capacity");
        assert!(append(&mut journal, 10, 1).is_ok());
        let out_of_order = journal.append(
            9,
            1,
            1,
            MemoryOperation::Delete,
            None,
            AuditOutcome::Denied,
            "no",
        );
        assert_eq!(out_of_order, Err(JournalError::InvalidTimeOrder));
        let too_long = journal.append(
            11,
            1,
            1,
            MemoryOperation::Delete,
            None,
            AuditOutcome::Denied,
            "reason-too-long",
        );
        assert_eq!(too_long, Err(JournalError::ReasonTooLarge));
    }
}
