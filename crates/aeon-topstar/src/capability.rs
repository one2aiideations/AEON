use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type CapabilityId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityRisk {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub id: CapabilityId,
    pub name: String,
    pub description: String,
    pub risk: CapabilityRisk,
}

impl Capability {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        risk: CapabilityRisk,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            description: description.into(),
            risk,
        }
    }
}
