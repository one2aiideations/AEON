use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActorId, CapabilityId, ResponsibilityContractId};

pub type ActionId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionState {
    Proposed,
    Authorized,
    Executing,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub id: ActionId,
    pub actor: ActorId,
    pub capability: CapabilityId,
    pub responsibility: ResponsibilityContractId,
    pub state: ActionState,
    pub intent: String,
}

impl Action {
    pub fn new(
        actor: ActorId,
        capability: CapabilityId,
        responsibility: ResponsibilityContractId,
        intent: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            actor,
            capability,
            responsibility,
            state: ActionState::Proposed,
            intent: intent.into(),
        }
    }
}
