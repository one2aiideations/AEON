use crate::{
    CapabilityId, DeviceId, DeviceLifecycle, DeviceRecord, DeviceRegistry, DeviceRegistryError,
    MachineProfile,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceSelector {
    pub profile: Option<MachineProfile>,
    pub capability: Option<CapabilityId>,
    pub lifecycle: Option<DeviceLifecycle>,
    pub executable: Option<bool>,
}

impl DeviceSelector {
    pub fn matches(&self, record: &DeviceRecord) -> bool {
        if self
            .profile
            .is_some_and(|profile| record.device.profile != profile)
        {
            return false;
        }
        if self
            .capability
            .is_some_and(|capability| !record.device.capabilities.contains(&capability))
        {
            return false;
        }
        if self
            .lifecycle
            .is_some_and(|lifecycle| record.lifecycle != lifecycle)
        {
            return false;
        }
        if self
            .executable
            .is_some_and(|executable| record.device.can_execute() != executable)
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub id: DeviceId,
    pub profile: MachineProfile,
    pub lifecycle: DeviceLifecycle,
    pub executable: bool,
}

impl From<&DeviceRecord> for DeviceSnapshot {
    fn from(record: &DeviceRecord) -> Self {
        Self {
            id: record.device.id,
            profile: record.device.profile,
            lifecycle: record.lifecycle,
            executable: record.device.can_execute(),
        }
    }
}

impl DeviceRegistry {
    pub fn discover(
        &self,
        selector: &DeviceSelector,
    ) -> Result<Vec<DeviceSnapshot>, DeviceRegistryError> {
        let mut snapshots = self
            .list()?
            .iter()
            .filter(|record| selector.matches(record))
            .map(DeviceSnapshot::from)
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|snapshot| snapshot.id);
        Ok(snapshots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HardwareInfo, MachineProfile};

    fn device(name: &str, profile: MachineProfile) -> crate::Device {
        crate::Device::new(
            name,
            profile,
            HardwareInfo::new("x86_64").expect("hardware"),
        )
        .expect("device")
    }

    #[test]
    fn discovery_filters_profile_and_lifecycle() {
        let registry = DeviceRegistry::new();
        let server = device("server", MachineProfile::Server);
        let edge = device("edge", MachineProfile::Edge);
        let edge_id = edge.id;

        registry.register(server).expect("server");
        registry.register(edge).expect("edge");
        registry
            .set_lifecycle(edge_id, DeviceLifecycle::Quarantined)
            .expect("quarantine");

        let selector = DeviceSelector {
            profile: Some(MachineProfile::Edge),
            lifecycle: Some(DeviceLifecycle::Quarantined),
            ..Default::default()
        };

        let found = registry.discover(&selector).expect("discover");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, edge_id);
    }
}
