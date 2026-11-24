use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActionId, EvidenceId};

pub type ResultId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultStatus {
    Success,
    Failure,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    pub id: ResultId,
    pub action: ActionId,
    pub status: ResultStatus,
    pub output: serde_json::Value,
    pub evidence: Vec<EvidenceId>,
}

impl ActionResult {
    pub fn success(action: ActionId, output: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            action,
            status: ResultStatus::Success,
            output,
            evidence: Vec::new(),
        }
    }
}
