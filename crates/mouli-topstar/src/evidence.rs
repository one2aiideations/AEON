use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActionId, EventId};

pub type EvidenceId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub action: Option<ActionId>,
    pub event: Option<EventId>,
    pub source: String,
    pub content: serde_json::Value,
    pub timestamp_unix: i64,
}

impl Evidence {
    pub fn new(source: impl Into<String>, content: serde_json::Value, timestamp_unix: i64) -> Self {
        Self {
            id: Uuid::new_v4(),
            action: None,
            event: None,
            source: source.into(),
            content,
            timestamp_unix,
        }
    }
}
