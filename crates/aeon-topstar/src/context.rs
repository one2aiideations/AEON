use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type ContextId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub id: ContextId,
    pub values: serde_json::Value,
}

impl Context {
    pub fn new(values: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            values,
        }
    }
}
