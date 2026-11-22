use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActionId, EvidenceId, ResultId};

pub type VerificationId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationStatus {
    Pending,
    Verified,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    pub id: VerificationId,
    pub action: ActionId,
    pub result: ResultId,
    pub evidence: Vec<EvidenceId>,
    pub status: VerificationStatus,
    pub reason: Option<String>,
}

impl Verification {
    pub fn pending(action: ActionId, result: ResultId) -> Self {
        Self {
            id: Uuid::new_v4(),
            action,
            result,
            evidence: Vec::new(),
            status: VerificationStatus::Pending,
            reason: None,
        }
    }

    pub fn verified(mut self) -> Self {
        self.status = VerificationStatus::Verified;
        self.reason = None;
        self
    }

    pub fn rejected(mut self, reason: impl Into<String>) -> Self {
        self.status = VerificationStatus::Rejected;
        self.reason = Some(reason.into());
        self
    }
}
