use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CapabilityId, Identity};

pub type PolicyId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub id: PolicyId,
    pub name: String,
    pub allowed_capabilities: Vec<CapabilityId>,
    pub denied_capabilities: Vec<CapabilityId>,
}

impl Default for Policy {
    fn default() -> Self {
        Self::new("default")
    }
}

impl Policy {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            allowed_capabilities: Vec::new(),
            denied_capabilities: Vec::new(),
        }
    }

    pub fn permits(&self, capability: CapabilityId) -> bool {
        self.allowed_capabilities.contains(&capability)
            && !self.denied_capabilities.contains(&capability)
    }

    pub fn applies_to(&self, _identity: &Identity) -> bool {
        true
    }
}
