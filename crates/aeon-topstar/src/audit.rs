use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ActionId, ActorId, EventId};

pub type AuditEntryId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditOutcome {
    Allowed,
    Denied,
    Started,
    Succeeded,
    Failed,
    Cancelled,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: AuditEntryId,
    pub actor: ActorId,
    pub action: Option<ActionId>,
    pub event: Option<EventId>,
    pub timestamp_unix: i64,
    pub outcome: AuditOutcome,
    pub detail: String,
    pub previous_hash: Option<String>,
    pub hash: String,
}

impl AuditEntry {
    pub fn new(
        actor: ActorId,
        action: Option<ActionId>,
        timestamp_unix: i64,
        outcome: AuditOutcome,
        detail: impl Into<String>,
    ) -> Self {
        Self::with_previous_hash(actor, action, timestamp_unix, outcome, detail, None)
    }

    pub fn with_previous_hash(
        actor: ActorId,
        action: Option<ActionId>,
        timestamp_unix: i64,
        outcome: AuditOutcome,
        detail: impl Into<String>,
        previous_hash: Option<String>,
    ) -> Self {
        let mut entry = Self {
            id: Uuid::new_v4(),
            actor,
            action,
            event: None,
            timestamp_unix,
            outcome,
            detail: detail.into(),
            previous_hash,
            hash: String::new(),
        };
        entry.hash = entry.compute_hash();
        entry
    }

    pub fn compute_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.id.as_bytes());
        hasher.update(self.actor.as_bytes());
        if let Some(action) = self.action {
            hasher.update(action.as_bytes());
        }
        if let Some(event) = self.event {
            hasher.update(event.as_bytes());
        }
        hasher.update(self.timestamp_unix.to_le_bytes());
        hasher.update(serde_json::to_vec(&self.outcome).expect("audit outcome serializes"));
        hasher.update(self.detail.as_bytes());
        if let Some(previous_hash) = &self.previous_hash {
            hasher.update(previous_hash.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    pub fn is_integrity_valid(&self) -> bool {
        self.hash == self.compute_hash()
    }
}
