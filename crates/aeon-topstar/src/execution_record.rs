use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActionId, ActorId, CapabilityId, Evidence, EvidenceId, ResponsibilityContractId};

pub type ExecutionRecordId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub id: ExecutionRecordId,
    pub actor: ActorId,
    pub action: ActionId,
    pub capability: CapabilityId,
    pub responsibility: ResponsibilityContractId,
    pub started_at_unix: i64,
    pub completed_at_unix: Option<i64>,
    pub outcome: ExecutionOutcome,
    pub evidence: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionOutcome {
    Started,
    Succeeded,
    Failed,
    Cancelled,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionRecordError {
    AlreadyCompleted,
    InvalidCompletionOutcome,
    InvalidCompletionTimestamp,
    EvidenceActionMismatch,
}

impl std::fmt::Display for ExecutionRecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyCompleted => f.write_str("execution record is already completed"),
            Self::InvalidCompletionOutcome => {
                f.write_str("execution completion requires a terminal outcome")
            }
            Self::InvalidCompletionTimestamp => {
                f.write_str("execution completion timestamp precedes start timestamp")
            }
            Self::EvidenceActionMismatch => f.write_str("evidence belongs to a different action"),
        }
    }
}

impl std::error::Error for ExecutionRecordError {}

impl ExecutionRecord {
    pub fn started(
        actor: ActorId,
        action: ActionId,
        capability: CapabilityId,
        responsibility: ResponsibilityContractId,
        timestamp_unix: i64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            actor,
            action,
            capability,
            responsibility,
            started_at_unix: timestamp_unix,
            completed_at_unix: None,
            outcome: ExecutionOutcome::Started,
            evidence: Vec::new(),
        }
    }

    pub fn is_complete(&self) -> bool {
        self.completed_at_unix.is_some()
    }

    pub fn complete(
        &mut self,
        timestamp_unix: i64,
        outcome: ExecutionOutcome,
    ) -> Result<(), ExecutionRecordError> {
        if self.is_complete() {
            return Err(ExecutionRecordError::AlreadyCompleted);
        }
        if !matches!(
            outcome,
            ExecutionOutcome::Succeeded
                | ExecutionOutcome::Failed
                | ExecutionOutcome::Cancelled
                | ExecutionOutcome::Revoked
        ) {
            return Err(ExecutionRecordError::InvalidCompletionOutcome);
        }
        if timestamp_unix < self.started_at_unix {
            return Err(ExecutionRecordError::InvalidCompletionTimestamp);
        }

        self.completed_at_unix = Some(timestamp_unix);
        self.outcome = outcome;
        Ok(())
    }

    pub fn attach_evidence(&mut self, evidence: &Evidence) -> Result<(), ExecutionRecordError> {
        if let Some(action) = evidence.action {
            if action != self.action {
                return Err(ExecutionRecordError::EvidenceActionMismatch);
            }
        }
        if !self.evidence.contains(&evidence.id) {
            self.evidence.push(evidence.id);
        }
        Ok(())
    }

    pub fn add_evidence(&mut self, evidence: EvidenceId) {
        if !self.evidence.contains(&evidence) {
            self.evidence.push(evidence);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> ExecutionRecord {
        ExecutionRecord::started(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            10,
        )
    }

    #[test]
    fn execution_record_tracks_completion_and_evidence() {
        let evidence = Uuid::new_v4();
        let mut record = record();
        record.add_evidence(evidence);
        record.add_evidence(evidence);
        record
            .complete(20, ExecutionOutcome::Succeeded)
            .expect("completion");

        assert!(record.is_complete());
        assert_eq!(record.completed_at_unix, Some(20));
        assert_eq!(record.outcome, ExecutionOutcome::Succeeded);
        assert_eq!(record.evidence, vec![evidence]);
    }

    #[test]
    fn attach_evidence_accepts_unbound_evidence() {
        let mut record = record();
        let evidence = Evidence::new("executor", serde_json::json!({"ok": true}), 20);
        record.attach_evidence(&evidence).expect("attach");
        assert_eq!(record.evidence, vec![evidence.id]);
    }

    #[test]
    fn attach_evidence_rejects_evidence_for_another_action() {
        let mut record = record();
        let mut evidence = Evidence::new("executor", serde_json::json!({"ok": true}), 20);
        evidence.action = Some(Uuid::new_v4());

        assert_eq!(
            record.attach_evidence(&evidence),
            Err(ExecutionRecordError::EvidenceActionMismatch)
        );
        assert!(record.evidence.is_empty());
    }

    #[test]
    fn completion_rejects_non_terminal_outcome() {
        let mut record = record();
        assert_eq!(
            record.complete(20, ExecutionOutcome::Started),
            Err(ExecutionRecordError::InvalidCompletionOutcome)
        );
        assert!(!record.is_complete());
    }

    #[test]
    fn completion_rejects_timestamp_before_start() {
        let mut record = record();
        assert_eq!(
            record.complete(9, ExecutionOutcome::Failed),
            Err(ExecutionRecordError::InvalidCompletionTimestamp)
        );
        assert!(!record.is_complete());
    }

    #[test]
    fn completed_record_cannot_be_completed_twice() {
        let mut record = record();
        record
            .complete(20, ExecutionOutcome::Succeeded)
            .expect("completion");
        assert_eq!(
            record.complete(30, ExecutionOutcome::Failed),
            Err(ExecutionRecordError::AlreadyCompleted)
        );
        assert_eq!(record.completed_at_unix, Some(20));
        assert_eq!(record.outcome, ExecutionOutcome::Succeeded);
    }
}
