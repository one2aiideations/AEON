use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ActorId;

pub type IntentId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub id: IntentId,
    pub actor: ActorId,
    pub statement: String,
    pub context: serde_json::Value,
}

impl Intent {
    pub fn new(actor: ActorId, statement: impl Into<String>, context: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            actor,
            statement: statement.into(),
            context,
        }
    }
}
