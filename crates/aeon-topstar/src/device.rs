use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{CapabilityId, NetworkAddress, Resource, SessionState};

pub type DeviceId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineProfile {
    Tiny,
    Embedded,
    Iot,
    Wearable,
    Drone,
    Robot,
    Mobile,
    Computer,
    Workstation,
    Server,
    Edge,
    Cluster,
    Cloud,
    Vehicle,
    Industrial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerState {
    Unknown,
    Off,
    Booting,
    On,
    Sleep,
    Hibernating,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecurityState {
    Unknown,
    Untrusted,
    Provisioning,
    Trusted,
    Quarantined,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthState {
    Unknown,
    Healthy,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub architecture: String,
    pub cpu_cores: Option<u32>,
    pub cpu_threads: Option<u32>,
    pub gpu_count: Option<u32>,
    pub npu_count: Option<u32>,
    pub memory_bytes: Option<u64>,
    pub storage_bytes: Option<u64>,
}

impl HardwareInfo {
    pub fn new(architecture: impl Into<String>) -> Result<Self, DeviceError> {
        let architecture = architecture.into();
        if architecture.trim().is_empty() {
            return Err(DeviceError::InvalidHardware("architecture is empty"));
        }
        Ok(Self {
            architecture,
            cpu_cores: None,
            cpu_threads: None,
            gpu_count: None,
            npu_count: None,
            memory_bytes: None,
            storage_bytes: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub profile: MachineProfile,
    pub hardware: HardwareInfo,
    pub capabilities: Vec<CapabilityId>,
    pub resources: Vec<Resource>,
    pub network_addresses: Vec<NetworkAddress>,
    pub power: PowerState,
    pub security: SecurityState,
    pub health: HealthState,
    pub session_state: Option<SessionState>,
}

impl Device {
    pub fn new(
        name: impl Into<String>,
        profile: MachineProfile,
        hardware: HardwareInfo,
    ) -> Result<Self, DeviceError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DeviceError::InvalidDevice("device name is empty"));
        }

        Ok(Self {
            id: Uuid::new_v4(),
            name,
            profile,
            hardware,
            capabilities: Vec::new(),
            resources: Vec::new(),
            network_addresses: Vec::new(),
            power: PowerState::Unknown,
            security: SecurityState::Unknown,
            health: HealthState::Unknown,
            session_state: None,
        })
    }

    pub fn add_capability(&mut self, capability: CapabilityId) {
        if !self.capabilities.contains(&capability) {
            self.capabilities.push(capability);
        }
    }

    pub fn add_resource(&mut self, resource: Resource) {
        if !self
            .resources
            .iter()
            .any(|candidate| candidate.id == resource.id)
        {
            self.resources.push(resource);
        }
    }

    pub fn add_network_address(&mut self, address: NetworkAddress) {
        if !self.network_addresses.contains(&address) {
            self.network_addresses.push(address);
        }
    }

    pub fn set_power_state(&mut self, state: PowerState) {
        self.power = state;
    }

    pub fn set_security_state(&mut self, state: SecurityState) {
        self.security = state;
    }

    pub fn set_health_state(&mut self, state: HealthState) {
        self.health = state;
    }

    pub fn bind_session(&mut self, state: SessionState) -> Result<(), DeviceError> {
        if state == SessionState::Closed {
            return Err(DeviceError::InvalidSessionBinding);
        }
        self.session_state = Some(state);
        Ok(())
    }

    pub fn can_execute(&self) -> bool {
        matches!(self.security, SecurityState::Trusted)
            && matches!(self.power, PowerState::On)
            && matches!(self.health, HealthState::Healthy | HealthState::Degraded)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeviceError {
    #[error("invalid device: {0}")]
    InvalidDevice(&'static str),
    #[error("invalid hardware: {0}")]
    InvalidHardware(&'static str),
    #[error("closed sessions cannot be bound to a device")]
    InvalidSessionBinding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_deduplicates_capabilities_resources_and_addresses() {
        let hardware = HardwareInfo::new("x86_64").expect("hardware");
        let mut device = Device::new("node", MachineProfile::Server, hardware).expect("device");
        let capability = Uuid::new_v4();
        let resource = Resource::new(crate::ResourceKind::Memory, "ram");
        let address = NetworkAddress::new(
            crate::Transport::Tcp,
            crate::Endpoint::new("127.0.0.1", 8443).expect("endpoint"),
        );

        device.add_capability(capability);
        device.add_capability(capability);
        device.add_resource(resource.clone());
        device.add_resource(resource);
        device.add_network_address(address.clone());
        device.add_network_address(address);

        assert_eq!(device.capabilities.len(), 1);
        assert_eq!(device.resources.len(), 1);
        assert_eq!(device.network_addresses.len(), 1);
    }

    #[test]
    fn execution_requires_trusted_healthy_powered_device() {
        let hardware = HardwareInfo::new("aarch64").expect("hardware");
        let mut device = Device::new("edge", MachineProfile::Edge, hardware).expect("device");

        assert!(!device.can_execute());

        device.set_security_state(SecurityState::Trusted);
        device.set_power_state(PowerState::On);
        device.set_health_state(HealthState::Healthy);

        assert!(device.can_execute());
    }

    #[test]
    fn closed_session_cannot_be_bound() {
        let hardware = HardwareInfo::new("x86_64").expect("hardware");
        let mut device = Device::new("node", MachineProfile::Computer, hardware).expect("device");

        assert_eq!(
            device.bind_session(SessionState::Closed),
            Err(DeviceError::InvalidSessionBinding)
        );
    }
}
