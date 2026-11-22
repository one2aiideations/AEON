use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActorId, CapabilityId, ExecutionBoundary};

pub type ResponsibilityContractId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsibilityContract {
    pub id: ResponsibilityContractId,
    pub goal: String,
    pub actor: ActorId,
    pub allowed_capabilities: Vec<CapabilityId>,
    pub forbidden_capabilities: Vec<CapabilityId>,
    pub boundary: ExecutionBoundary,
    pub expires_at_unix: Option<i64>,
    pub revoked: bool,
}

impl ResponsibilityContract {
    pub fn new(goal: impl Into<String>, actor: ActorId) -> Self {
        Self {
            id: Uuid::new_v4(),
            goal: goal.into(),
            actor,
            allowed_capabilities: Vec::new(),
            forbidden_capabilities: Vec::new(),
            boundary: ExecutionBoundary::default(),
            expires_at_unix: None,
            revoked: false,
        }
    }

    pub fn allows(&self, capability: CapabilityId, now_unix: i64) -> bool {
        !self.revoked
            && self.expires_at_unix.is_none_or(|expiry| now_unix < expiry)
            && self.allowed_capabilities.contains(&capability)
            && !self.forbidden_capabilities.contains(&capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    #[test]
    fn revoked_contract_denies_capability() {
        let actor = Identity::new("agent");
        let capability = Uuid::new_v4();
        let mut contract = ResponsibilityContract::new("test", actor.id);
        contract.allowed_capabilities.push(capability);
        contract.revoked = true;
        assert!(!contract.allows(capability, 0));
    }
}
