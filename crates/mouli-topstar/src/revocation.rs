use std::collections::HashSet;

use crate::{ActionId, CapabilityId, ResponsibilityContractId};

#[derive(Debug, Default)]
pub struct RevocationRegistry {
    capabilities: HashSet<CapabilityId>,
    responsibilities: HashSet<ResponsibilityContractId>,
    actions: HashSet<ActionId>,
}

impl RevocationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revoke_capability(&mut self, id: CapabilityId) {
        self.capabilities.insert(id);
    }

    pub fn revoke_responsibility(&mut self, id: ResponsibilityContractId) {
        self.responsibilities.insert(id);
    }

    pub fn revoke_action(&mut self, id: ActionId) {
        self.actions.insert(id);
    }

    pub fn is_capability_revoked(&self, id: CapabilityId) -> bool {
        self.capabilities.contains(&id)
    }

    pub fn is_responsibility_revoked(&self, id: ResponsibilityContractId) -> bool {
        self.responsibilities.contains(&id)
    }

    pub fn is_action_revoked(&self, id: ActionId) -> bool {
        self.actions.contains(&id)
    }
}
