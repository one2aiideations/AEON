use std::collections::HashMap;
use thiserror::Error;

use crate::{Capability, CapabilityId};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("capability already registered: {0}")]
    AlreadyRegistered(CapabilityId),
    #[error("capability not found: {0}")]
    NotFound(CapabilityId),
}

#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    capabilities: HashMap<CapabilityId, Capability>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, capability: Capability) -> Result<(), RegistryError> {
        if self.capabilities.contains_key(&capability.id) {
            return Err(RegistryError::AlreadyRegistered(capability.id));
        }
        self.capabilities.insert(capability.id, capability);
        Ok(())
    }

    pub fn get(&self, id: CapabilityId) -> Result<&Capability, RegistryError> {
        self.capabilities
            .get(&id)
            .ok_or(RegistryError::NotFound(id))
    }

    pub fn contains(&self, id: CapabilityId) -> bool {
        self.capabilities.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityRisk;

    #[test]
    fn registers_and_retrieves_capability() {
        let capability = Capability::new("read", "read data", CapabilityRisk::Low);
        let id = capability.id;
        let mut registry = CapabilityRegistry::new();
        registry.register(capability).unwrap();
        assert_eq!(registry.get(id).unwrap().name, "read");
    }
}
