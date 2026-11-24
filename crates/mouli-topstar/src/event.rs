use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActionId, ActorId};

pub type EventId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub actor: ActorId,
    pub action: Option<ActionId>,
    pub event_type: String,
    pub timestamp_unix: i64,
    pub payload: serde_json::Value,
}

impl Event {
    pub fn new(
        actor: ActorId,
        action: Option<ActionId>,
        event_type: impl Into<String>,
        timestamp_unix: i64,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            actor,
            action,
            event_type: event_type.into(),
            timestamp_unix,
            payload,
        }
    }
}
