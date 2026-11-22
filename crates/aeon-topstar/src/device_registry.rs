use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use thiserror::Error;

use crate::{Device, DeviceId, SecurityState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceLifecycle {
    Active,
    Quarantined,
    Revoked,
}

#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub device: Device,
    pub lifecycle: DeviceLifecycle,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceRegistry {
    devices: Arc<RwLock<HashMap<DeviceId, DeviceRecord>>>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, device: Device) -> Result<DeviceId, DeviceRegistryError> {
        let id = device.id;
        let mut devices = self
            .devices
            .write()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        if devices.contains_key(&id) {
            return Err(DeviceRegistryError::AlreadyRegistered(id));
        }
        let lifecycle = match device.security {
            SecurityState::Quarantined => DeviceLifecycle::Quarantined,
            SecurityState::Revoked => DeviceLifecycle::Revoked,
            _ => DeviceLifecycle::Active,
        };
        devices.insert(id, DeviceRecord { device, lifecycle });
        Ok(id)
    }

    pub fn list(&self) -> Result<Vec<DeviceRecord>, DeviceRegistryError> {
        let devices = self
            .devices
            .read()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        let mut records = devices.values().cloned().collect::<Vec<_>>();
        records.sort_by_key(|record| record.device.id);
        Ok(records)
    }

    pub fn get(&self, id: DeviceId) -> Result<Option<DeviceRecord>, DeviceRegistryError> {
        let devices = self
            .devices
            .read()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        Ok(devices.get(&id).cloned())
    }

    pub fn set_lifecycle(
        &self,
        id: DeviceId,
        lifecycle: DeviceLifecycle,
    ) -> Result<(), DeviceRegistryError> {
        let mut devices = self
            .devices
            .write()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        let record = devices
            .get_mut(&id)
            .ok_or(DeviceRegistryError::NotFound(id))?;
        record.lifecycle = lifecycle;
        record.device.security = match lifecycle {
            DeviceLifecycle::Active => SecurityState::Trusted,
            DeviceLifecycle::Quarantined => SecurityState::Quarantined,
            DeviceLifecycle::Revoked => SecurityState::Revoked,
        };
        Ok(())
    }

    pub fn unregister(&self, id: DeviceId) -> Result<DeviceRecord, DeviceRegistryError> {
        let mut devices = self
            .devices
            .write()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        devices.remove(&id).ok_or(DeviceRegistryError::NotFound(id))
    }

    pub fn len(&self) -> Result<usize, DeviceRegistryError> {
        let devices = self
            .devices
            .read()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        Ok(devices.len())
    }

    pub fn is_empty(&self) -> Result<bool, DeviceRegistryError> {
        Ok(self.len()? == 0)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeviceRegistryError {
    #[error("device {0} is already registered")]
    AlreadyRegistered(DeviceId),
    #[error("device {0} was not found")]
    NotFound(DeviceId),
    #[error("device registry lock is poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HardwareInfo, MachineProfile};

    fn device(name: &str) -> Device {
        Device::new(
            name,
            MachineProfile::Server,
            HardwareInfo::new("x86_64").expect("hardware"),
        )
        .expect("device")
    }

    #[test]
    fn registration_and_lookup_are_durable_in_memory() {
        let registry = DeviceRegistry::new();
        let id = registry.register(device("node")).expect("register");

        assert_eq!(registry.len().expect("len"), 1);
        assert_eq!(
            registry.get(id).expect("lookup").expect("record").device.id,
            id
        );
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let registry = DeviceRegistry::new();
        let first = device("node");
        let id = first.id;
        registry.register(first).expect("register");

        let mut duplicate = device("other");
        duplicate.id = id;

        assert_eq!(
            registry.register(duplicate),
            Err(DeviceRegistryError::AlreadyRegistered(id))
        );
    }

    #[test]
    fn quarantine_and_revoke_change_security_state() {
        let registry = DeviceRegistry::new();
        let id = registry.register(device("node")).expect("register");

        registry
            .set_lifecycle(id, DeviceLifecycle::Quarantined)
            .expect("quarantine");
        assert_eq!(
            registry
                .get(id)
                .expect("lookup")
                .expect("record")
                .device
                .security,
            SecurityState::Quarantined
        );

        registry
            .set_lifecycle(id, DeviceLifecycle::Revoked)
            .expect("revoke");
        assert_eq!(
            registry
                .get(id)
                .expect("lookup")
                .expect("record")
                .device
                .security,
            SecurityState::Revoked
        );
    }
}
