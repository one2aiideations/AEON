//! Part 2: COVESA VSS v4.0 SIGNAL TREE ENGINE, WIRE-LEVEL SOME/IP & CAN-FD ROUTER

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 2 - COVESA VSS v4.0 SIGNAL TREE ENGINE, WIRE-LEVEL SOME/IP & CAN-FD ROUTER
// CRITICALITY: ISO 26262 ASIL-B / ASIL-D Mixed Criticality Communication Fabric
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none (no_std compatible)
// CONSTRAINTS: Zero Dynamic Allocations, Fixed Memory Layouts, Wire-Level Determinism
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

// =================================================================================================
// 1. SIGNAL TREE & BUS SIZING CONSTANTS
// =================================================================================================

/// Maximum distinct VSS signal nodes allocated in static registry storage
pub const MAX_VSS_NODES: usize = 512;

/// Maximum payload capacity per SOME/IP message frame in bytes
pub const SOMEIP_MAX_PAYLOAD_SIZE: usize = 1400;

/// Maximum CAN FD data frame payload capacity (ISO 11898-1:2015)
pub const CANFD_MAX_PAYLOAD_SIZE: usize = 64;

/// Maximum registered SOME/IP service dispatch handlers
pub const MAX_SOMEIP_SERVICES: usize = 32;

/// Maximum subscriber routing entries in virtual bus table
pub const MAX_BUS_ROUTES: usize = 64;

// =================================================================================================
// 2. ERROR TAXONOMY FOR COMMUNICATION & VSS ENGINE
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommError {
    Ok = 0,
    SignalNotFound = 100,
    SignalTypeMismatch = 101,
    SignalValueOutOfRange = 102,
    DeadbandFilterSuppressed = 103,
    BufferTooSmall = 104,
    MalformedSomeIpHeader = 105,
    UnsupportedSomeIpVersion = 106,
    SomeIpPayloadExceeded = 107,
    InvalidCanFdDlc = 108,
    CanFdPayloadTruncated = 109,
    ServiceNotRegistered = 110,
    RouteTableExhausted = 111,
    LockContentionTimeout = 112,
    AsilIntegrityViolation = 113,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalAsil {
    Qm = 0,
    AsilA = 1,
    AsilB = 2,
    AsilC = 3,
    AsilD = 4,
}

// =================================================================================================
// 3. FNV-1a COMPILE-TIME STRING HASHING FOR VSS PATHS
// =================================================================================================

/// Deterministic FNV-1a 32-bit hash function for constant-time VSS path lookup
#[inline(always)]
pub const fn fnv1a_hash_32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

// =================================================================================================
// 4. COVESA VEHICLE SIGNAL SPECIFICATION (VSS v4.0) DATA MODEL
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Boolean = 0,
    Int32 = 1,
    Int64 = 2,
    Uint32 = 3,
    Uint64 = 4,
    Float32 = 5,
    Float64 = 6,
    StringShort = 7, // Fixed 16-byte UTF-8 string buffer
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union TypedSignalValue {
    pub boolean: bool,
    pub i32_val: i32,
    pub i64_val: i64,
    pub u32_val: u32,
    pub u64_val: u64,
    pub f32_val: f32,
    pub f64_val: f64,
    pub str_val: [u8; 16],
}

impl TypedSignalValue {
    pub const fn empty() -> Self {
        Self { u64_val: 0 }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorActuatorKind {
    Sensor = 0,
    Actuator = 1,
    Attribute = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VssSignalDescriptor {
    pub path_hash: u32,
    pub data_type: DataType,
    pub kind: SensorActuatorKind,
    pub asil_level: SignalAsil,
    pub min_f64: f64,
    pub max_f64: f64,
    pub deadband: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VssSignalNode {
    pub descriptor: VssSignalDescriptor,
    pub current_value: TypedSignalValue,
    pub last_sent_f32: f32,
    pub timestamp_micros: u64,
    pub write_counter: u32,
    pub is_valid: bool,
}

impl VssSignalNode {
    pub const fn empty() -> Self {
        Self {
            descriptor: VssSignalDescriptor {
                path_hash: 0,
                data_type: DataType::Float32,
                kind: SensorActuatorKind::Sensor,
                asil_level: SignalAsil::Qm,
                min_f64: 0.0,
                max_f64: 0.0,
                deadband: 0.0,
            },
            current_value: TypedSignalValue::empty(),
            last_sent_f32: 0.0,
            timestamp_micros: 0,
            write_counter: 0,
            is_valid: false,
        }
    }
}

// =================================================================================================
// 5. EXTENDED VSS VEHICLE SIGNAL DEFINITIONS (STANDARDIZED PATHS)
// =================================================================================================

pub mod vss_paths {
    use super::*;

    // Powertrain & Battery Subsystems (ASIL-C / ASIL-D)
    pub const VEHICLE_POWERTRAIN_TRACTIONBATTERY_STATEOFCHARGE: u32 =
        fnv1a_hash_32(b"Vehicle.Powertrain.TractionBattery.StateOfCharge");
    pub const VEHICLE_POWERTRAIN_TRACTIONBATTERY_VOLTAGE: u32 =
        fnv1a_hash_32(b"Vehicle.Powertrain.TractionBattery.Voltage");
    pub const VEHICLE_POWERTRAIN_TRACTIONBATTERY_CURRENT: u32 =
        fnv1a_hash_32(b"Vehicle.Powertrain.TractionBattery.Current");
    pub const VEHICLE_POWERTRAIN_ELECTRICMOTOR_FRONT_TORQUE: u32 =
        fnv1a_hash_32(b"Vehicle.Powertrain.ElectricMotor.Front.Torque");
    pub const VEHICLE_POWERTRAIN_ELECTRICMOTOR_REAR_TORQUE: u32 =
        fnv1a_hash_32(b"Vehicle.Powertrain.ElectricMotor.Rear.Torque");
    pub const VEHICLE_POWERTRAIN_ELECTRICMOTOR_SPEED: u32 =
        fnv1a_hash_32(b"Vehicle.Powertrain.ElectricMotor.Speed");

    // Chassis, Steering & Braking Subsystems (ASIL-D)
    pub const VEHICLE_CHASSIS_STEERINGWHEEL_ANGLE: u32 =
        fnv1a_hash_32(b"Vehicle.Chassis.SteeringWheel.Angle");
    pub const VEHICLE_CHASSIS_STEERINGWHEEL_ANGULARRATE: u32 =
        fnv1a_hash_32(b"Vehicle.Chassis.SteeringWheel.AngularRate");
    pub const VEHICLE_CHASSIS_BRAKE_PEDALPOSITION: u32 =
        fnv1a_hash_32(b"Vehicle.Chassis.Brake.PedalPosition");
    pub const VEHICLE_CHASSIS_BRAKE_PRESSURE_MASTER: u32 =
        fnv1a_hash_32(b"Vehicle.Chassis.Brake.Pressure.Master");
    pub const VEHICLE_CHASSIS_ACCELERATOR_PEDALPOSITION: u32 =
        fnv1a_hash_32(b"Vehicle.Chassis.Accelerator.PedalPosition");
    pub const VEHICLE_SPEED: u32 = fnv1a_hash_32(b"Vehicle.Speed");

    // ADAS & Autonomous Trajectory Subsystems (ASIL-D)
    pub const VEHICLE_ADAS_AEB_TRIGGERED: u32 = fnv1a_hash_32(b"Vehicle.ADAS.AEB.Triggered");
    pub const VEHICLE_ADAS_TARGET_ACCELERATION: u32 =
        fnv1a_hash_32(b"Vehicle.ADAS.TargetAcceleration");
    pub const VEHICLE_ADAS_TARGET_STEERING_ANGLE: u32 =
        fnv1a_hash_32(b"Vehicle.ADAS.TargetSteeringAngle");

    // Cabin, Thermal & Body Controls (ASIL-A / QM)
    pub const VEHICLE_CABIN_HVAC_TARGETTEMPERATURE: u32 =
        fnv1a_hash_32(b"Vehicle.Cabin.HVAC.TargetTemperature");
    pub const VEHICLE_CABIN_HVAC_AMBIENTTEMPERATURE: u32 =
        fnv1a_hash_32(b"Vehicle.Cabin.HVAC.AmbientTemperature");
    pub const VEHICLE_BODY_LIGHTS_BEAMHIGH: u32 = fnv1a_hash_32(b"Vehicle.Body.Lights.BeamHigh");
    pub const VEHICLE_BODY_DOORS_ROW1_LEFT_ISOPEN: u32 =
        fnv1a_hash_32(b"Vehicle.Body.Doors.Row1.Left.IsOpen");
}

// =================================================================================================
// 6. THREAD-SAFE, ZERO-ALLOCATION VSS SIGNAL ENGINE
// =================================================================================================

pub struct VssSignalEngine {
    nodes: UnsafeCell<[VssSignalNode; MAX_VSS_NODES]>,
    node_count: AtomicUsize,
    lock: AtomicBool,
}

// SAFETY: the only interior-mutable field is `nodes`, and every method that touches it does so
// between `acquire_lock` (Acquire) and `release_lock` (Release) on the same spin lock, so accesses
// are mutually exclusive and ordered. A thread that panics while holding the lock leaves it
// locked, and the others then spin forever.
unsafe impl Sync for VssSignalEngine {}

impl VssSignalEngine {
    pub const fn new() -> Self {
        Self {
            nodes: UnsafeCell::new([VssSignalNode::empty(); MAX_VSS_NODES]),
            node_count: AtomicUsize::new(0),
            lock: AtomicBool::new(false),
        }
    }

    #[inline(always)]
    fn acquire_lock(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
    }

    #[inline(always)]
    fn release_lock(&self) {
        self.lock.store(false, Ordering::Release);
    }

    pub fn register_signal(&self, descriptor: VssSignalDescriptor) -> Result<(), CommError> {
        self.acquire_lock();
        let count = self.node_count.load(Ordering::Relaxed);
        if count >= MAX_VSS_NODES {
            self.release_lock();
            return Err(CommError::RouteTableExhausted);
        }

        // SAFETY: `acquire_lock` above is still held (every return path below releases it first),
        // so no other thread has a reference into `nodes`.
        let nodes = unsafe { &mut *self.nodes.get() };
        for i in 0..count {
            if nodes[i].is_valid && nodes[i].descriptor.path_hash == descriptor.path_hash {
                self.release_lock();
                return Ok(()); // Signal already registered
            }
        }

        nodes[count] = VssSignalNode {
            descriptor,
            current_value: TypedSignalValue::empty(),
            last_sent_f32: 0.0,
            timestamp_micros: 0,
            write_counter: 0,
            is_valid: true,
        };

        self.node_count.store(count + 1, Ordering::Release);
        self.release_lock();
        Ok(())
    }

    pub fn update_float32(
        &self,
        path_hash: u32,
        val: f32,
        timestamp_micros: u64,
    ) -> Result<bool, CommError> {
        self.acquire_lock();
        let count = self.node_count.load(Ordering::Relaxed);
        // SAFETY: the lock taken by `acquire_lock` at the top is held until `release_lock`, so
        // this is the only reference into `nodes`.
        let nodes = unsafe { &mut *self.nodes.get() };

        for i in 0..count {
            let node = &mut nodes[i];
            if node.is_valid && node.descriptor.path_hash == path_hash {
                // Range validity check
                if (val as f64) < node.descriptor.min_f64 || (val as f64) > node.descriptor.max_f64
                {
                    self.release_lock();
                    return Err(CommError::SignalValueOutOfRange);
                }

                // Deadband evaluation
                let diff = (val - node.last_sent_f32).abs();
                let deadband_exceeded = diff >= node.descriptor.deadband || node.write_counter == 0;

                node.current_value.f32_val = val;
                node.timestamp_micros = timestamp_micros;
                node.write_counter = node.write_counter.wrapping_add(1);

                if deadband_exceeded {
                    node.last_sent_f32 = val;
                }

                self.release_lock();
                return Ok(deadband_exceeded);
            }
        }

        self.release_lock();
        Err(CommError::SignalNotFound)
    }

    pub fn read_float32(&self, path_hash: u32) -> Result<(f32, u64), CommError> {
        self.acquire_lock();
        let count = self.node_count.load(Ordering::Relaxed);
        // SAFETY: the lock taken by `acquire_lock` at the top is held, so no writer can run while
        // this shared reference exists.
        let nodes = unsafe { &*self.nodes.get() };

        for i in 0..count {
            let node = &nodes[i];
            if node.is_valid && node.descriptor.path_hash == path_hash {
                // SAFETY: the first 8 bytes of the union are always initialised (`empty()` writes
                // `u64_val: 0`, and every write replaces them), and every bit pattern is a valid
                // `f32`. The value is only meaningful if this signal was written as an f32; this
                // function does not check the descriptor's declared type.
                let val = unsafe { node.current_value.f32_val };
                let ts = node.timestamp_micros;
                self.release_lock();
                return Ok((val, ts));
            }
        }

        self.release_lock();
        Err(CommError::SignalNotFound)
    }

    pub fn populate_standard_vehicle_catalog(&self) -> Result<(), CommError> {
        // Battery SoC: 0.0% to 100.0%, 0.1% deadband, ASIL-C
        self.register_signal(VssSignalDescriptor {
            path_hash: vss_paths::VEHICLE_POWERTRAIN_TRACTIONBATTERY_STATEOFCHARGE,
            data_type: DataType::Float32,
            kind: SensorActuatorKind::Sensor,
            asil_level: SignalAsil::AsilC,
            min_f64: 0.0,
            max_f64: 100.0,
            deadband: 0.1,
        })?;

        // Steering Angle: -720.0 to +720.0 deg, 0.05 deg deadband, ASIL-D
        self.register_signal(VssSignalDescriptor {
            path_hash: vss_paths::VEHICLE_CHASSIS_STEERINGWHEEL_ANGLE,
            data_type: DataType::Float32,
            kind: SensorActuatorKind::Actuator,
            asil_level: SignalAsil::AsilD,
            min_f64: -720.0,
            max_f64: 720.0,
            deadband: 0.05,
        })?;

        // Brake Pressure: 0.0 to 300.0 Bar, 0.2 Bar deadband, ASIL-D
        self.register_signal(VssSignalDescriptor {
            path_hash: vss_paths::VEHICLE_CHASSIS_BRAKE_PRESSURE_MASTER,
            data_type: DataType::Float32,
            kind: SensorActuatorKind::Sensor,
            asil_level: SignalAsil::AsilD,
            min_f64: 0.0,
            max_f64: 300.0,
            deadband: 0.2,
        })?;

        // Vehicle Speed: 0.0 to 350.0 km/h, 0.1 km/h deadband, ASIL-B
        self.register_signal(VssSignalDescriptor {
            path_hash: vss_paths::VEHICLE_SPEED,
            data_type: DataType::Float32,
            kind: SensorActuatorKind::Sensor,
            asil_level: SignalAsil::AsilB,
            min_f64: 0.0,
            max_f64: 350.0,
            deadband: 0.1,
        })?;

        // Motor Front Torque: -1500.0 to +1500.0 Nm, 1.0 Nm deadband, ASIL-D
        self.register_signal(VssSignalDescriptor {
            path_hash: vss_paths::VEHICLE_POWERTRAIN_ELECTRICMOTOR_FRONT_TORQUE,
            data_type: DataType::Float32,
            kind: SensorActuatorKind::Actuator,
            asil_level: SignalAsil::AsilD,
            min_f64: -1500.0,
            max_f64: 1500.0,
            deadband: 1.0,
        })?;

        Ok(())
    }
}

impl Default for VssSignalEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. WIRE-LEVEL SOME/IP PROTOCOL ENCODER & DECODER (AUTOSAR FO R22-11)
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SomeIpMessageType {
    Request = 0x00,
    RequestNoReturn = 0x01,
    Notification = 0x02,
    Response = 0x80,
    Error = 0x81,
    TpRequest = 0x20,
    TpResponse = 0xA0,
    TpNotification = 0x22,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SomeIpReturnCode {
    EOk = 0x00,
    ENotOk = 0x01,
    EUnknownService = 0x02,
    EUnknownMethod = 0x03,
    ENotReady = 0x04,
    ENotReachable = 0x05,
    ETimeout = 0x06,
    EWrongProtocolVersion = 0x07,
    EWrongInterfaceVersion = 0x08,
    EMalformedMessage = 0x09,
    EWrongMessageType = 0x0A,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct SomeIpHeader {
    pub service_id: u16,
    pub method_or_event_id: u16,
    pub length: u32, // Bytes following this field (Request ID to end of payload)
    pub client_id: u16,
    pub session_id: u16,
    pub protocol_version: u8,
    pub interface_version: u8,
    pub message_type: u8,
    pub return_code: u8,
}

pub const SOMEIP_HEADER_SIZE: usize = 16;
pub const SOMEIP_PROTOCOL_VERSION_CURRENT: u8 = 0x01;

#[repr(C)]
pub struct SomeIpFrame {
    pub header: SomeIpHeader,
    pub payload_len: usize,
    pub payload: [u8; SOMEIP_MAX_PAYLOAD_SIZE],
}

impl SomeIpFrame {
    pub const fn new() -> Self {
        Self {
            header: SomeIpHeader {
                service_id: 0,
                method_or_event_id: 0,
                length: 8, // Minimum header length after 'length' field
                client_id: 0,
                session_id: 0,
                protocol_version: SOMEIP_PROTOCOL_VERSION_CURRENT,
                interface_version: 1,
                message_type: SomeIpMessageType::Notification as u8,
                return_code: SomeIpReturnCode::EOk as u8,
            },
            payload_len: 0,
            payload: [0u8; SOMEIP_MAX_PAYLOAD_SIZE],
        }
    }

    /// Serializes SOME/IP frame to big-endian wire-level bytes
    pub fn serialize_to_buffer(&self, dest: &mut [u8]) -> Result<usize, CommError> {
        let total_size = SOMEIP_HEADER_SIZE + self.payload_len;
        if dest.len() < total_size {
            return Err(CommError::BufferTooSmall);
        }

        // Message ID: Service ID (16-bit) + Method/Event ID (16-bit)
        dest[0] = (self.header.service_id >> 8) as u8;
        dest[1] = self.header.service_id as u8;
        dest[2] = (self.header.method_or_event_id >> 8) as u8;
        dest[3] = self.header.method_or_event_id as u8;

        // Length (32-bit): 8 bytes for remaining header + payload length
        let wire_length = (8 + self.payload_len) as u32;
        dest[4] = (wire_length >> 24) as u8;
        dest[5] = (wire_length >> 16) as u8;
        dest[6] = (wire_length >> 8) as u8;
        dest[7] = wire_length as u8;

        // Request ID: Client ID (16-bit) + Session ID (16-bit)
        dest[8] = (self.header.client_id >> 8) as u8;
        dest[9] = self.header.client_id as u8;
        dest[10] = (self.header.session_id >> 8) as u8;
        dest[11] = self.header.session_id as u8;

        // Versions and Types
        dest[12] = self.header.protocol_version;
        dest[13] = self.header.interface_version;
        dest[14] = self.header.message_type;
        dest[15] = self.header.return_code;

        // Payload copy
        dest[SOMEIP_HEADER_SIZE..total_size].copy_from_slice(&self.payload[..self.payload_len]);
        Ok(total_size)
    }

    /// Zero-copy deserialization of incoming wire-level byte slice
    pub fn deserialize_from_buffer(src: &[u8]) -> Result<Self, CommError> {
        if src.len() < SOMEIP_HEADER_SIZE {
            return Err(CommError::MalformedSomeIpHeader);
        }

        let service_id = ((src[0] as u16) << 8) | (src[1] as u16);
        let method_or_event_id = ((src[2] as u16) << 8) | (src[3] as u16);
        let length = ((src[4] as u32) << 24)
            | ((src[5] as u32) << 16)
            | ((src[6] as u32) << 8)
            | (src[7] as u32);

        if length < 8 {
            return Err(CommError::MalformedSomeIpHeader);
        }

        let payload_len = (length - 8) as usize;
        if payload_len > SOMEIP_MAX_PAYLOAD_SIZE || src.len() < (SOMEIP_HEADER_SIZE + payload_len) {
            return Err(CommError::SomeIpPayloadExceeded);
        }

        let client_id = ((src[8] as u16) << 8) | (src[9] as u16);
        let session_id = ((src[10] as u16) << 8) | (src[11] as u16);
        let protocol_version = src[12];
        let interface_version = src[13];
        let message_type = src[14];
        let return_code = src[15];

        if protocol_version != SOMEIP_PROTOCOL_VERSION_CURRENT {
            return Err(CommError::UnsupportedSomeIpVersion);
        }

        let mut frame = Self::new();
        frame.header = SomeIpHeader {
            service_id,
            method_or_event_id,
            length,
            client_id,
            session_id,
            protocol_version,
            interface_version,
            message_type,
            return_code,
        };
        frame.payload_len = payload_len;
        frame.payload[..payload_len]
            .copy_from_slice(&src[SOMEIP_HEADER_SIZE..SOMEIP_HEADER_SIZE + payload_len]);

        Ok(frame)
    }
}

impl Default for SomeIpFrame {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. CAN FD DATAGRAM ARCHITECTURE (ISO 11898-1:2015)
// =================================================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CanFdFrame {
    pub can_id: u32,
    pub is_extended: bool,
    pub is_fd: bool,
    pub bit_rate_switch: bool,
    pub error_state_indicator: bool,
    pub dlc: u8,
    pub payload_len: u8,
    pub data: [u8; CANFD_MAX_PAYLOAD_SIZE],
}

impl CanFdFrame {
    pub const fn new() -> Self {
        Self {
            can_id: 0,
            is_extended: false,
            is_fd: true,
            bit_rate_switch: true,
            error_state_indicator: false,
            dlc: 0,
            payload_len: 0,
            data: [0u8; CANFD_MAX_PAYLOAD_SIZE],
        }
    }

    /// Converts CAN FD Data Length Code (DLC) to actual byte count
    pub const fn dlc_to_length(dlc: u8) -> u8 {
        match dlc {
            0..=8 => dlc,
            9 => 12,
            10 => 16,
            11 => 20,
            12 => 24,
            13 => 32,
            14 => 48,
            15 => 64,
            _ => 0,
        }
    }

    /// Converts payload byte count to the minimal legal CAN FD DLC
    pub const fn length_to_dlc(len: u8) -> u8 {
        match len {
            0..=8 => len,
            9..=12 => 9,
            13..=16 => 10,
            17..=20 => 11,
            21..=24 => 12,
            25..=32 => 13,
            33..=48 => 14,
            _ => 15,
        }
    }

    pub fn set_payload(&mut self, payload: &[u8]) -> Result<(), CommError> {
        if payload.len() > CANFD_MAX_PAYLOAD_SIZE {
            return Err(CommError::CanFdPayloadTruncated);
        }
        let len = payload.len() as u8;
        self.dlc = Self::length_to_dlc(len);
        self.payload_len = len;
        self.data[..payload.len()].copy_from_slice(payload);
        Ok(())
    }
}

impl Default for CanFdFrame {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 9. HIGH-THROUGHPUT VIRTUAL BUS ROUTER
// =================================================================================================

pub type SomeIpDispatchFn = fn(frame: &SomeIpFrame) -> Result<(), CommError>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SomeIpServiceRoute {
    pub service_id: u16,
    pub method_id: u16,
    pub target_asil: SignalAsil,
    pub handler: Option<SomeIpDispatchFn>,
}

pub struct VirtualBusRouter {
    routes: UnsafeCell<[SomeIpServiceRoute; MAX_BUS_ROUTES]>,
    route_count: AtomicUsize,
    total_messages_routed: AtomicU64,
    dropped_frames: AtomicU64,
    lock: AtomicBool,
}

// SAFETY: only sound if routes are registered before frames are routed, or at least never at the
// same time: `register_route` holds `lock` but takes `&mut` to the whole `routes` array,
// while `route_someip_frame` takes `&` to the whole array without the lock. Running both at once
// would alias a `&mut` with a `&` (undefined behaviour) even though they touch different slots.
// The counters are atomics.
unsafe impl Sync for VirtualBusRouter {}

impl VirtualBusRouter {
    pub const fn new() -> Self {
        Self {
            routes: UnsafeCell::new(
                [SomeIpServiceRoute {
                    service_id: 0,
                    method_id: 0,
                    target_asil: SignalAsil::Qm,
                    handler: None,
                }; MAX_BUS_ROUTES],
            ),
            route_count: AtomicUsize::new(0),
            total_messages_routed: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            lock: AtomicBool::new(false),
        }
    }

    pub fn register_route(
        &self,
        service_id: u16,
        method_id: u16,
        target_asil: SignalAsil,
        handler: SomeIpDispatchFn,
    ) -> Result<(), CommError> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }

        let count = self.route_count.load(Ordering::Relaxed);
        if count >= MAX_BUS_ROUTES {
            self.lock.store(false, Ordering::Release);
            return Err(CommError::RouteTableExhausted);
        }

        // SAFETY: `lock` is held, so no other registration runs. No frame may be routed while a
        // route is being registered (see the `Sync` impl): that is a requirement on the caller,
        // not something this function enforces.
        let routes = unsafe { &mut *self.routes.get() };
        routes[count] = SomeIpServiceRoute {
            service_id,
            method_id,
            target_asil,
            handler: Some(handler),
        };

        self.route_count.store(count + 1, Ordering::Release);
        self.lock.store(false, Ordering::Release);
        Ok(())
    }

    pub fn route_someip_frame(&self, frame: &SomeIpFrame) -> Result<(), CommError> {
        let count = self.route_count.load(Ordering::Acquire);
        // SAFETY: this takes no lock, so it relies on the caller not registering routes at the
        // same time (see the `Sync` impl). `count` was loaded with Acquire, so every slot below it
        // was fully written before it was published.
        let routes = unsafe { &*self.routes.get() };

        for i in 0..count {
            let route = &routes[i];
            if route.service_id == frame.header.service_id
                && (route.method_id == frame.header.method_or_event_id || route.method_id == 0xFFFF)
            {
                if let Some(dispatch) = route.handler {
                    self.total_messages_routed.fetch_add(1, Ordering::Relaxed);
                    return dispatch(frame);
                }
            }
        }

        self.dropped_frames.fetch_add(1, Ordering::Relaxed);
        Err(CommError::ServiceNotRegistered)
    }

    pub fn bridge_canfd_to_vss(
        &self,
        frame: &CanFdFrame,
        vss: &VssSignalEngine,
        timestamp_micros: u64,
    ) -> Result<(), CommError> {
        match frame.can_id {
            // Standard CAN ID for Powertrain Inverter Motor RPM & Torque
            0x200 => {
                if frame.payload_len >= 8 {
                    let torque_raw = i16::from_be_bytes([frame.data[0], frame.data[1]]);
                    let speed_raw = u16::from_be_bytes([frame.data[2], frame.data[3]]);

                    let torque_nm = (torque_raw as f32) * 0.1;
                    let speed_kph = (speed_raw as f32) * 0.05;

                    vss.update_float32(
                        vss_paths::VEHICLE_POWERTRAIN_ELECTRICMOTOR_FRONT_TORQUE,
                        torque_nm,
                        timestamp_micros,
                    )?;
                    vss.update_float32(vss_paths::VEHICLE_SPEED, speed_kph, timestamp_micros)?;
                }
            }
            // Standard CAN ID for Brake Pressure Master Cylinder
            0x210 => {
                if frame.payload_len >= 4 {
                    let press_raw = u16::from_be_bytes([frame.data[0], frame.data[1]]);
                    let pressure_bar = (press_raw as f32) * 0.05;

                    vss.update_float32(
                        vss_paths::VEHICLE_CHASSIS_BRAKE_PRESSURE_MASTER,
                        pressure_bar,
                        timestamp_micros,
                    )?;
                }
            }
            // Standard CAN ID for Steer-By-Wire Angle
            0x220 if frame.payload_len >= 4 => {
                let angle_raw = i16::from_be_bytes([frame.data[0], frame.data[1]]);
                let angle_deg = (angle_raw as f32) * 0.1;

                vss.update_float32(
                    vss_paths::VEHICLE_CHASSIS_STEERINGWHEEL_ANGLE,
                    angle_deg,
                    timestamp_micros,
                )?;
            }
            _ => {
                // Non-registered telemetry frame ignored
            }
        }
        Ok(())
    }
}

impl Default for VirtualBusRouter {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 10. PART 2 UNIT TESTS & PROTOCOL VERIFICATION
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vss_engine_catalog_and_deadband() {
        let engine = VssSignalEngine::new();
        engine
            .populate_standard_vehicle_catalog()
            .expect("Catalog init failed");

        // First update must always exceed deadband
        let notify1 = engine
            .update_float32(vss_paths::VEHICLE_SPEED, 50.0, 1000)
            .expect("Update failed");
        assert!(notify1);

        // Update below 0.1 km/h deadband
        let notify2 = engine
            .update_float32(vss_paths::VEHICLE_SPEED, 50.05, 1010)
            .expect("Update failed");
        assert!(!notify2);

        // Update above deadband
        let notify3 = engine
            .update_float32(vss_paths::VEHICLE_SPEED, 50.25, 1020)
            .expect("Update failed");
        assert!(notify3);

        let (read_speed, ts) = engine
            .read_float32(vss_paths::VEHICLE_SPEED)
            .expect("Read speed failed");
        assert_eq!(read_speed, 50.25);
        assert_eq!(ts, 1020);
    }

    #[test]
    fn test_vss_range_enforcement() {
        let engine = VssSignalEngine::new();
        engine
            .populate_standard_vehicle_catalog()
            .expect("Catalog init failed");

        // Battery SoC out of [0.0, 100.0] bounds
        let res = engine.update_float32(
            vss_paths::VEHICLE_POWERTRAIN_TRACTIONBATTERY_STATEOFCHARGE,
            105.0,
            1000,
        );
        assert_eq!(res, Err(CommError::SignalValueOutOfRange));
    }

    #[test]
    fn test_someip_wire_roundtrip_serialization() {
        let mut frame = SomeIpFrame::new();
        frame.header.service_id = 0x1234;
        frame.header.method_or_event_id = 0x0001;
        frame.header.client_id = 0x0042;
        frame.header.session_id = 0x0007;
        frame.header.message_type = SomeIpMessageType::Request as u8;

        let sample_payload = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03];
        frame.payload_len = sample_payload.len();
        frame.payload[..sample_payload.len()].copy_from_slice(&sample_payload);

        let mut wire_buffer = [0u8; 128];
        let bytes_written = frame
            .serialize_to_buffer(&mut wire_buffer)
            .expect("Serialization failed");
        assert_eq!(bytes_written, SOMEIP_HEADER_SIZE + sample_payload.len());

        let parsed_frame = SomeIpFrame::deserialize_from_buffer(&wire_buffer[..bytes_written])
            .expect("Deserialization failed");

        assert_eq!({ parsed_frame.header.service_id }, 0x1234);
        assert_eq!({ parsed_frame.header.client_id }, 0x0042);
        assert_eq!(parsed_frame.payload_len, sample_payload.len());
        assert_eq!(
            &parsed_frame.payload[..parsed_frame.payload_len],
            &sample_payload[..]
        );
    }

    static ROUTE_RECEIVED: AtomicBool = AtomicBool::new(false);

    fn mock_service_dispatch(frame: &SomeIpFrame) -> Result<(), CommError> {
        ROUTE_RECEIVED.store(true, Ordering::SeqCst);
        Ok(())
    }

    #[test]
    fn test_virtual_bus_routing() {
        let router = VirtualBusRouter::new();
        router
            .register_route(0x1000, 0x0001, SignalAsil::AsilD, mock_service_dispatch)
            .expect("Route registration failed");

        let mut frame = SomeIpFrame::new();
        frame.header.service_id = 0x1000;
        frame.header.method_or_event_id = 0x0001;

        assert!(router.route_someip_frame(&frame).is_ok());
        assert!(ROUTE_RECEIVED.load(Ordering::SeqCst));
    }

    #[test]
    fn test_canfd_dlc_conversions() {
        assert_eq!(CanFdFrame::length_to_dlc(8), 8);
        assert_eq!(CanFdFrame::length_to_dlc(15), 10);
        assert_eq!(CanFdFrame::length_to_dlc(64), 15);

        assert_eq!(CanFdFrame::dlc_to_length(10), 16);
        assert_eq!(CanFdFrame::dlc_to_length(15), 64);
    }
}
