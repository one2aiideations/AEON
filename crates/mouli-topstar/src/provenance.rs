use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActionId, ActorId, EventId, EvidenceId, IntentId, ResponsibilityContractId};

pub type ProvenanceId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub id: ProvenanceId,
    pub actor: ActorId,
    pub intent: Option<IntentId>,
    pub responsibility: Option<ResponsibilityContractId>,
    pub action: Option<ActionId>,
    pub event: Option<EventId>,
    pub evidence: Vec<EvidenceId>,
}

impl Provenance {
    pub fn new(actor: ActorId) -> Self {
        Self {
            id: Uuid::new_v4(),
            actor,
            intent: None,
            responsibility: None,
            action: None,
            event: None,
            evidence: Vec::new(),
        }
    }
}
