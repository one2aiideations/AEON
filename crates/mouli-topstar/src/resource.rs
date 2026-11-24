use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type ResourceId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    Cpu,
    Memory,
    Storage,
    Network,
    Gpu,
    Device,
    Financial,
    Physical,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub name: String,
    pub limit: Option<u64>,
    pub unit: Option<String>,
}

impl Resource {
    pub fn new(kind: ResourceKind, name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            name: name.into(),
            limit: None,
            unit: None,
        }
    }

    pub fn with_limit(mut self, limit: u64, unit: impl Into<String>) -> Self {
        self.limit = Some(limit);
        self.unit = Some(unit.into());
        self
    }
}
