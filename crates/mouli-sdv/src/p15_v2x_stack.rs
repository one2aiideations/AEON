//! Part 15: V2X STACK (3GPP C-V2X PC5, DSRC 802.11p, SAE J2735 BSM ASN.1 UPER)

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 15 - V2X STACK (3GPP C-V2X PC5, DSRC 802.11p, SAE J2735 BSM ASN.1 UPER)
// CRITICALITY: ISO 26262 ASIL-B / SAE J2735:2020 / IEEE 1609.2 / 3GPP TS 36.331
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocation, Unaligned Packed Encoding Rules (UPER)
// =================================================================================================

#[allow(unused_imports)]
use crate::fmath::FloatMath;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS & PHYSICAL/LINK LAYER CONFIGURATIONS
// =================================================================================================

/// 5.9 GHz Dedicated Short Range Communications (DSRC) / C-V2X Band (Channels 172-184)
pub const V2X_DEFAULT_CHANNEL_MHZ: u32 = 5890;

/// Maximum Transmission Unit for V2X Over-The-Air Layer 2 Frames
pub const V2X_MAX_PDU_SIZE: usize = 1400;

/// SAE J2735 Message Identifier Constants
pub const DSRC_MSG_ID_BSM: u16 = 0x0014; // Basic Safety Message (DSRCmsgID 20)
pub const DSRC_MSG_ID_EVA: u16 = 0x0016; // Emergency Vehicle Alert (DSRCmsgID 22)
pub const DSRC_MSG_ID_RSA: u16 = 0x001B; // Roadside Alert (DSRCmsgID 27)
pub const DSRC_MSG_ID_SPAT: u16 = 0x0013; // Signal Phase and Timing (DSRCmsgID 19)
pub const DSRC_MSG_ID_MAP: u16 = 0x0012; // Map Data (DSRCmsgID 18)

/// Circular queue depths for incoming and outgoing V2X packets
pub const V2X_TX_QUEUE_DEPTH: usize = 32;
pub const V2X_RX_QUEUE_DEPTH: usize = 64;

/// Maximum number of active remote vehicles tracked in the spatial safety table
pub const MAX_REMOTE_VEHICLES: usize = 128;

/// Privacy-protecting Temporary ID rotation threshold (every 300,000 ms = 5 minutes)
pub const TEMP_ID_ROTATION_INTERVAL_MS: u64 = 300_000;

// =================================================================================================
// 2. ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2xError {
    Ok = 0,
    BitstreamBufferExhausted = 1,
    MalformedAsn1UperPayload = 2,
    UnsupportedMessageId = 3,
    TxQueueFull = 4,
    RxQueueEmpty = 5,
    InvalidCrc16 = 6,
    SecurityVerificationFailed = 7,
    GeofenceOutOfRange = 8,
    ChannelCongestionThrottled = 9,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2xRadioMedium {
    DirectCellularSidelinkPc5 = 0, // 3GPP Rel 14/15/16 LTE/5G-V2X
    DsrcIeee80211p = 1,            // Dedicated Short-Range 802.11p WAVE
}

// =================================================================================================
// 3. ZERO-ALLOCATION ASN.1 UNALIGNED PACKED ENCODING RULES (UPER) ENGINE
// =================================================================================================

/// Bit-level stream serializer and deserializer conforming to ITU-T X.691 / ASN.1 UPER
pub struct Asn1BitStream<'a> {
    buffer: &'a mut [u8],
    bit_offset: usize,
    capacity_bits: usize,
}

impl<'a> Asn1BitStream<'a> {
    pub fn new_writer(buffer: &'a mut [u8]) -> Self {
        let capacity_bits = buffer.len() * 8;
        buffer.fill(0);
        Self {
            buffer,
            bit_offset: 0,
            capacity_bits,
        }
    }

    pub fn new_reader(buffer: &'a mut [u8], total_bits: usize) -> Self {
        Self {
            buffer,
            bit_offset: 0,
            capacity_bits: total_bits,
        }
    }

    #[inline(always)]
    pub fn bits_written(&self) -> usize {
        self.bit_offset
    }

    #[inline(always)]
    pub fn bytes_written(&self) -> usize {
        self.bit_offset.div_ceil(8)
    }

    /// Writes a single bit into the stream
    pub fn write_bit(&mut self, bit: bool) -> Result<(), V2xError> {
        if self.bit_offset >= self.capacity_bits {
            return Err(V2xError::BitstreamBufferExhausted);
        }

        if bit {
            let byte_idx = self.bit_offset / 8;
            let bit_in_byte = 7 - (self.bit_offset % 8);
            self.buffer[byte_idx] |= 1 << bit_in_byte;
        }

        self.bit_offset += 1;
        Ok(())
    }

    /// Reads a single bit from the stream
    pub fn read_bit(&mut self) -> Result<bool, V2xError> {
        if self.bit_offset >= self.capacity_bits {
            return Err(V2xError::BitstreamBufferExhausted);
        }

        let byte_idx = self.bit_offset / 8;
        let bit_in_byte = 7 - (self.bit_offset % 8);
        let val = (self.buffer[byte_idx] & (1 << bit_in_byte)) != 0;

        self.bit_offset += 1;
        Ok(val)
    }

    /// Writes up to 32 bits into the stream
    pub fn write_bits(&mut self, value: u32, bit_count: usize) -> Result<(), V2xError> {
        if self.bit_offset + bit_count > self.capacity_bits {
            return Err(V2xError::BitstreamBufferExhausted);
        }

        for i in (0..bit_count).rev() {
            let bit = ((value >> i) & 1) != 0;
            self.write_bit(bit)?;
        }
        Ok(())
    }

    /// Reads up to 32 bits from the stream
    pub fn read_bits(&mut self, bit_count: usize) -> Result<u32, V2xError> {
        if self.bit_offset + bit_count > self.capacity_bits {
            return Err(V2xError::BitstreamBufferExhausted);
        }

        let mut res = 0u32;
        for _ in 0..bit_count {
            let bit = self.read_bit()?;
            res = (res << 1) | (if bit { 1 } else { 0 });
        }
        Ok(res)
    }

    /// Encodes a constrained whole integer within range [min, max]
    pub fn write_constrained_int(
        &mut self,
        value: i64,
        min: i64,
        max: i64,
    ) -> Result<(), V2xError> {
        let clamped = if value < min {
            min
        } else if value > max {
            max
        } else {
            value
        };
        let range = (max - min) as u64;
        let norm = (clamped - min) as u64;

        if range == 0 {
            return Ok(());
        }

        // Calculate bits required: ceil(log2(range + 1))
        let bits_required = 64 - range.leading_zeros() as usize;
        self.write_bits(norm as u32, bits_required)
    }

    /// Decodes a constrained whole integer within range [min, max]
    pub fn read_constrained_int(&mut self, min: i64, max: i64) -> Result<i64, V2xError> {
        let range = (max - min) as u64;
        if range == 0 {
            return Ok(min);
        }

        let bits_required = 64 - range.leading_zeros() as usize;
        let val = self.read_bits(bits_required)? as i64;
        Ok(min + val)
    }
}

// =================================================================================================
// 4. SAE J2735 BASIC SAFETY MESSAGE (BSM) DATA STRUCTURES
// =================================================================================================

/// Brake System Status as defined by SAE J2735 DE_BrakeSystemStatus
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct J2735BrakeSystemStatus {
    pub wheel_brakes_active: u8, // 4-bit bitmask: FrontLeft, FrontRight, RearLeft, RearRight
    pub traction_control_state: u8, // 0: Unavailable, 1: Off, 2: On, 3: Engaged
    pub abs_state: u8,           // 0: Unavailable, 1: Off, 2: On, 3: Engaged
    pub stability_control_state: u8, // 0: Unavailable, 1: Off, 2: On, 3: Engaged
    pub brake_boost_applied: bool,
    pub aux_brakes_active: bool,
}

impl J2735BrakeSystemStatus {
    pub const fn nominal() -> Self {
        Self {
            wheel_brakes_active: 0,
            traction_control_state: 2,  // Active/On
            abs_state: 2,               // Active/On
            stability_control_state: 2, // Active/On
            brake_boost_applied: false,
            aux_brakes_active: false,
        }
    }
}

/// 4-Way Vehicle Dynamic Accelerations (DE_AccelerationSet4Way)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct J2735AccelerationSet4Way {
    pub longitudinal_m_s2: f32, // -20.0 to 20.0 m/s^2 (0.01 m/s^2 resolution)
    pub lateral_m_s2: f32,      // -20.0 to 20.0 m/s^2 (0.01 m/s^2 resolution)
    pub vertical_g: f32,        // -2.5 to 2.5 g (0.02 g resolution)
    pub yaw_rate_deg_s: f32,    // -100.0 to 100.0 deg/s (0.01 deg/s resolution)
}

/// SAE J2735 BSM Part I - Core Safety Data (Transmitted nominally at 10Hz)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct J2735BsmCoreData {
    pub msg_count: u8,                   // 0..127 sequence counter
    pub temp_id: [u8; 4],                // Privacy-rotated 4-byte temporary ID
    pub dsecond_ms: u16,                 // Milliseconds within current minute (0..59999)
    pub latitude_deg: f64,               // Decimal degrees: -90.0 to 90.0 (1/10 microdegree)
    pub longitude_deg: f64,              // Decimal degrees: -180.0 to 180.0 (1/10 microdegree)
    pub elevation_m: f32,                // Elevation in meters (-409.5 to 6143.9 m)
    pub semi_major_axis_accuracy_m: f32, // Horizontal accuracy ellipse
    pub semi_minor_axis_accuracy_m: f32,
    pub semi_major_orientation_deg: f32,
    pub transmission_state: u8, // 0: Neutral, 1: Park, 2: Forward, 3: Reverse
    pub speed_m_s: f32,         // 0.0 to 163.8 m/s (0.02 m/s resolution)
    pub heading_deg: f32,       // 0.0 to 359.9875 degrees (0.0125 deg resolution)
    pub steering_wheel_angle_deg: f32, // -189.0 to 189.0 degrees (1.5 deg resolution)
    pub accel_set: J2735AccelerationSet4Way,
    pub brakes: J2735BrakeSystemStatus,
    pub vehicle_width_cm: u16,  // 0 to 1023 cm
    pub vehicle_length_cm: u16, // 0 to 4095 cm
}

impl J2735BsmCoreData {
    pub const fn empty() -> Self {
        Self {
            msg_count: 0,
            temp_id: [0u8; 4],
            dsecond_ms: 0,
            latitude_deg: 0.0,
            longitude_deg: 0.0,
            elevation_m: 0.0,
            semi_major_axis_accuracy_m: 0.5,
            semi_minor_axis_accuracy_m: 0.5,
            semi_major_orientation_deg: 0.0,
            transmission_state: 2, // Forward
            speed_m_s: 0.0,
            heading_deg: 0.0,
            steering_wheel_angle_deg: 0.0,
            accel_set: J2735AccelerationSet4Way {
                longitudinal_m_s2: 0.0,
                lateral_m_s2: 0.0,
                vertical_g: 0.0,
                yaw_rate_deg_s: 0.0,
            },
            brakes: J2735BrakeSystemStatus::nominal(),
            vehicle_width_cm: 200,
            vehicle_length_cm: 480,
        }
    }

    /// Encodes the BSM Core Data structure into ASN.1 UPER binary
    pub fn encode_uper(&self, bitstream: &mut Asn1BitStream) -> Result<(), V2xError> {
        // DSRC Message Identifier: 0x0014 (BSM)
        bitstream.write_bits(DSRC_MSG_ID_BSM as u32, 16)?;

        // MsgCount: 0..127 (7 bits)
        bitstream.write_constrained_int(self.msg_count as i64, 0, 127)?;

        // TemporaryID: 4 octets (32 bits)
        let temp_id_val = u32::from_be_bytes(self.temp_id);
        bitstream.write_bits(temp_id_val, 32)?;

        // DSecond: 0..65535 (SAE J2735 constrained 0..59999, 16 bits)
        bitstream.write_constrained_int(self.dsecond_ms as i64, 0, 59999)?;

        // Latitude: -900,000,000 to 900,000,001 (1/10 microdegrees, 31 bits)
        let lat_encoded = (self.latitude_deg * 10_000_000.0) as i64;
        bitstream.write_constrained_int(lat_encoded, -900_000_000, 900_000_001)?;

        // Longitude: -1,799,999,999 to 1,800,000,001 (1/10 microdegrees, 32 bits)
        let lon_encoded = (self.longitude_deg * 10_000_000.0) as i64;
        bitstream.write_constrained_int(lon_encoded, -1_799_999_999, 1_800_000_001)?;

        // Elevation: -4096 to 61439 (0.1 meter, offset -409.6m)
        let elev_encoded = ((self.elevation_m + 409.6) * 10.0) as i64;
        bitstream.write_constrained_int(elev_encoded, 0, 65535)?;

        // PositionalAccuracy: Semi-major (8 bits), Semi-minor (8 bits), Orientation (16 bits)
        let semi_maj = (self.semi_major_axis_accuracy_m / 0.05).min(255.0) as i64;
        let semi_min = (self.semi_minor_axis_accuracy_m / 0.05).min(255.0) as i64;
        let orient = (self.semi_major_orientation_deg / 0.005_493_248).min(65535.0) as i64;
        bitstream.write_constrained_int(semi_maj, 0, 255)?;
        bitstream.write_constrained_int(semi_min, 0, 255)?;
        bitstream.write_constrained_int(orient, 0, 65535)?;

        // TransmissionState: 0..7 (3 bits)
        bitstream.write_constrained_int(self.transmission_state as i64, 0, 7)?;

        // Speed: 0..8191 (0.02 m/s resolution, 13 bits)
        let speed_encoded = (self.speed_m_s / 0.02).min(8191.0) as i64;
        bitstream.write_constrained_int(speed_encoded, 0, 8191)?;

        // Heading: 0..28799 (0.0125 deg resolution, 15 bits)
        let heading_encoded = (self.heading_deg / 0.0125).min(28799.0) as i64;
        bitstream.write_constrained_int(heading_encoded, 0, 28799)?;

        // SteeringWheelAngle: -126..127 (1.5 deg steps, 8 bits)
        let steer_encoded = (self.steering_wheel_angle_deg / 1.5).clamp(-126.0, 127.0) as i64;
        bitstream.write_constrained_int(steer_encoded, -126, 127)?;

        // AccelerationSet4Way:
        // Long / Lat: -2000..2001 (0.01 m/s^2 resolution, 12 bits)
        let lon_acc = (self.accel_set.longitudinal_m_s2 * 100.0).clamp(-2000.0, 2001.0) as i64;
        let lat_acc = (self.accel_set.lateral_m_s2 * 100.0).clamp(-2000.0, 2001.0) as i64;
        bitstream.write_constrained_int(lon_acc, -2000, 2001)?;
        bitstream.write_constrained_int(lat_acc, -2000, 2001)?;

        // Vertical: -127..127 (0.02 g resolution, 8 bits)
        let vert_acc = (self.accel_set.vertical_g / 0.02).clamp(-127.0, 127.0) as i64;
        bitstream.write_constrained_int(vert_acc, -127, 127)?;

        // YawRate: -32767..32767 (0.01 deg/s resolution, 16 bits)
        let yaw_acc = (self.accel_set.yaw_rate_deg_s * 100.0).clamp(-32767.0, 32767.0) as i64;
        bitstream.write_constrained_int(yaw_acc, -32767, 32767)?;

        // BrakeSystemStatus:
        // Wheel brakes (4 bits), Traction control (2 bits), ABS (2 bits), SCS (2 bits), Boost (1 bit), Aux (1 bit)
        bitstream.write_bits(self.brakes.wheel_brakes_active as u32, 4)?;
        bitstream.write_bits(self.brakes.traction_control_state as u32, 2)?;
        bitstream.write_bits(self.brakes.abs_state as u32, 2)?;
        bitstream.write_bits(self.brakes.stability_control_state as u32, 2)?;
        bitstream.write_bit(self.brakes.brake_boost_applied)?;
        bitstream.write_bit(self.brakes.aux_brakes_active)?;

        // VehicleSize: Width 0..1023 cm (10 bits), Length 0..4095 cm (12 bits)
        bitstream.write_constrained_int(self.vehicle_width_cm as i64, 0, 1023)?;
        bitstream.write_constrained_int(self.vehicle_length_cm as i64, 0, 4095)?;

        Ok(())
    }

    /// Decodes an incoming ASN.1 UPER bitstream into a BSM Core Data structure
    pub fn decode_uper(bitstream: &mut Asn1BitStream) -> Result<Self, V2xError> {
        let msg_id = bitstream.read_bits(16)? as u16;
        if msg_id != DSRC_MSG_ID_BSM {
            return Err(V2xError::UnsupportedMessageId);
        }

        let msg_count = bitstream.read_constrained_int(0, 127)? as u8;
        let temp_id_val = bitstream.read_bits(32)?;
        let temp_id = temp_id_val.to_be_bytes();
        let dsecond_ms = bitstream.read_constrained_int(0, 59999)? as u16;

        let lat_encoded = bitstream.read_constrained_int(-900_000_000, 900_000_001)?;
        let latitude_deg = (lat_encoded as f64) / 10_000_000.0;

        let lon_encoded = bitstream.read_constrained_int(-1_799_999_999, 1_800_000_001)?;
        let longitude_deg = (lon_encoded as f64) / 10_000_000.0;

        let elev_encoded = bitstream.read_constrained_int(0, 65535)?;
        let elevation_m = ((elev_encoded as f32) / 10.0) - 409.6;

        let semi_maj = (bitstream.read_constrained_int(0, 255)? as f32) * 0.05;
        let semi_min = (bitstream.read_constrained_int(0, 255)? as f32) * 0.05;
        let orient = (bitstream.read_constrained_int(0, 65535)? as f32) * 0.005_493_248;

        let transmission_state = bitstream.read_constrained_int(0, 7)? as u8;
        let speed_m_s = (bitstream.read_constrained_int(0, 8191)? as f32) * 0.02;
        let heading_deg = (bitstream.read_constrained_int(0, 28799)? as f32) * 0.0125;
        let steering_wheel_angle_deg = (bitstream.read_constrained_int(-126, 127)? as f32) * 1.5;

        let lon_acc = (bitstream.read_constrained_int(-2000, 2001)? as f32) * 0.01;
        let lat_acc = (bitstream.read_constrained_int(-2000, 2001)? as f32) * 0.01;
        let vert_acc = (bitstream.read_constrained_int(-127, 127)? as f32) * 0.02;
        let yaw_acc = (bitstream.read_constrained_int(-32767, 32767)? as f32) * 0.01;

        let wheel_brakes_active = bitstream.read_bits(4)? as u8;
        let traction_control_state = bitstream.read_bits(2)? as u8;
        let abs_state = bitstream.read_bits(2)? as u8;
        let stability_control_state = bitstream.read_bits(2)? as u8;
        let brake_boost_applied = bitstream.read_bit()?;
        let aux_brakes_active = bitstream.read_bit()?;

        let vehicle_width_cm = bitstream.read_constrained_int(0, 1023)? as u16;
        let vehicle_length_cm = bitstream.read_constrained_int(0, 4095)? as u16;

        Ok(Self {
            msg_count,
            temp_id,
            dsecond_ms,
            latitude_deg,
            longitude_deg,
            elevation_m,
            semi_major_axis_accuracy_m: semi_maj,
            semi_minor_axis_accuracy_m: semi_min,
            semi_major_orientation_deg: orient,
            transmission_state,
            speed_m_s,
            heading_deg,
            steering_wheel_angle_deg,
            accel_set: J2735AccelerationSet4Way {
                longitudinal_m_s2: lon_acc,
                lateral_m_s2: lat_acc,
                vertical_g: vert_acc,
                yaw_rate_deg_s: yaw_acc,
            },
            brakes: J2735BrakeSystemStatus {
                wheel_brakes_active,
                traction_control_state,
                abs_state,
                stability_control_state,
                brake_boost_applied,
                aux_brakes_active,
            },
            vehicle_width_cm,
            vehicle_length_cm,
        })
    }
}

// =================================================================================================
// 5. GEOSPATIAL CORRIDOR & HAZARD ARBITRATION
// =================================================================================================

/// Tracked safety record of a neighboring vehicle received over the V2X airlink
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RemoteVehicleSafetyState {
    pub temp_id: [u8; 4],
    pub last_seen_timestamp_ms: u64,
    pub distance_to_host_m: f32,
    pub relative_bearing_deg: f32,
    pub relative_speed_m_s: f32,
    pub is_eebl_active: bool, // Electronic Emergency Brake Light
    pub is_forward_collision_threat: bool,
    pub is_cross_traffic_threat: bool,
}

impl RemoteVehicleSafetyState {
    pub const fn empty() -> Self {
        Self {
            temp_id: [0u8; 4],
            last_seen_timestamp_ms: 0,
            distance_to_host_m: 0.0,
            relative_bearing_deg: 0.0,
            relative_speed_m_s: 0.0,
            is_eebl_active: false,
            is_forward_collision_threat: false,
            is_cross_traffic_threat: false,
        }
    }
}

/// Geospatial spatial relevance filter and threat arbitrator
pub struct V2xSafetyCorridorArbitrator {
    remote_table: UnsafeCell<[RemoteVehicleSafetyState; MAX_REMOTE_VEHICLES]>,
    active_count: AtomicU16,
    emergency_vehicle_alert_active: AtomicBool,
}

unsafe impl Sync for V2xSafetyCorridorArbitrator {}

impl V2xSafetyCorridorArbitrator {
    pub const fn new() -> Self {
        Self {
            remote_table: UnsafeCell::new([RemoteVehicleSafetyState::empty(); MAX_REMOTE_VEHICLES]),
            active_count: AtomicU16::new(0),
            emergency_vehicle_alert_active: AtomicBool::new(false),
        }
    }

    /// Fast flat-earth equirectangular approximation for bare-metal distance in meters
    fn compute_distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f32 {
        const DEG_TO_RAD: f64 = 0.017453292519943295;
        const EARTH_RADIUS_M: f64 = 6_371_000.0;

        let lat1_rad = lat1 * DEG_TO_RAD;
        let lat2_rad = lat2 * DEG_TO_RAD;
        let dlat_rad = (lat2 - lat1) * DEG_TO_RAD;
        let dlon_rad = (lon2 - lon1) * DEG_TO_RAD;

        let x = dlon_rad * ((lat1_rad + lat2_rad) * 0.5).cos();
        let y = dlat_rad;

        ((x * x + y * y).sqrt() * EARTH_RADIUS_M) as f32
    }

    /// Evaluates an incoming remote BSM against the host vehicle's current kinematic trajectory
    pub fn ingest_remote_bsm(
        &self,
        host_bsm: &J2735BsmCoreData,
        remote_bsm: &J2735BsmCoreData,
        current_time_ms: u64,
    ) {
        let dist_m = Self::compute_distance_m(
            host_bsm.latitude_deg,
            host_bsm.longitude_deg,
            remote_bsm.latitude_deg,
            remote_bsm.longitude_deg,
        );

        // Ignore messages beyond 300 meters
        if dist_m > 300.0 {
            return;
        }

        // Detect Electronic Emergency Brake Light (EEBL): Remote vehicle deceleration > 4.0 m/s^2
        let eebl_active =
            remote_bsm.accel_set.longitudinal_m_s2 < -4.0 || remote_bsm.brakes.brake_boost_applied;

        // Relative heading and forward collision evaluation
        let heading_delta = (remote_bsm.heading_deg - host_bsm.heading_deg).abs();
        let is_same_direction = heading_delta < 45.0 || heading_delta > 315.0;
        let is_cross_traffic = heading_delta > 60.0 && heading_delta < 120.0;

        let is_forward_threat = is_same_direction
            && dist_m < 60.0
            && (host_bsm.speed_m_s > remote_bsm.speed_m_s || eebl_active);

        let table = unsafe { &mut *self.remote_table.get() };
        let count = self.active_count.load(Ordering::Relaxed) as usize;

        // Search for existing entry by Temporary ID
        let mut found_idx = None;
        for i in 0..count {
            if table[i].temp_id == remote_bsm.temp_id {
                found_idx = Some(i);
                break;
            }
        }

        let target_idx = match found_idx {
            Some(idx) => idx,
            None => {
                if count < MAX_REMOTE_VEHICLES {
                    self.active_count.fetch_add(1, Ordering::Relaxed);
                    count
                } else {
                    0 // Overwrite oldest entry
                }
            }
        };

        table[target_idx] = RemoteVehicleSafetyState {
            temp_id: remote_bsm.temp_id,
            last_seen_timestamp_ms: current_time_ms,
            distance_to_host_m: dist_m,
            relative_bearing_deg: heading_delta,
            relative_speed_m_s: host_bsm.speed_m_s - remote_bsm.speed_m_s,
            is_eebl_active: eebl_active,
            is_forward_collision_threat: is_forward_threat,
            is_cross_traffic_threat: is_cross_traffic && dist_m < 40.0,
        };
    }

    pub fn is_emergency_vehicle_nearby(&self) -> bool {
        self.emergency_vehicle_alert_active.load(Ordering::Acquire)
    }

    pub fn set_emergency_vehicle_alert(&self, active: bool) {
        self.emergency_vehicle_alert_active
            .store(active, Ordering::Release);
    }
}

impl Default for V2xSafetyCorridorArbitrator {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. CHANNEL CONGESTION & HARDWARE V2X TRANSCEIVER CONTROLLER
// =================================================================================================

/// Outbound V2X Over-the-Air Packet Descriptor
#[repr(C)]
#[derive(Clone, Copy)]
pub struct V2xTxFrame {
    pub medium: V2xRadioMedium,
    pub priority: u8, // 0 (Highest, ASIL/Safety) to 7 (Background/Infotainment)
    pub payload_len: usize,
    pub payload: [u8; V2X_MAX_PDU_SIZE],
}

impl V2xTxFrame {
    pub const fn empty() -> Self {
        Self {
            medium: V2xRadioMedium::DirectCellularSidelinkPc5,
            priority: 0,
            payload_len: 0,
            payload: [0u8; V2X_MAX_PDU_SIZE],
        }
    }
}

/// V2X Radio Controller managing Channel Busy Ratio (CBR) and Transmission Queues
#[repr(C, align(64))]
pub struct V2xRadioController {
    tx_queue: UnsafeCell<[V2xTxFrame; V2X_TX_QUEUE_DEPTH]>,
    tx_head: AtomicU16,
    tx_tail: AtomicU16,
    channel_busy_ratio_pct: AtomicU8, // 0..100% channel load
    current_temp_id: UnsafeCell<[u8; 4]>,
    last_temp_id_rotation_ms: AtomicU64,
    temp_id_seed: AtomicU32,
}

unsafe impl Sync for V2xRadioController {}

impl V2xRadioController {
    pub const fn new(initial_seed: u32) -> Self {
        Self {
            tx_queue: UnsafeCell::new([V2xTxFrame::empty(); V2X_TX_QUEUE_DEPTH]),
            tx_head: AtomicU16::new(0),
            tx_tail: AtomicU16::new(0),
            channel_busy_ratio_pct: AtomicU8::new(15), // Baseline 15% channel occupancy
            current_temp_id: UnsafeCell::new([0xDE, 0xAD, 0xBE, 0xEF]),
            last_temp_id_rotation_ms: AtomicU64::new(0),
            temp_id_seed: AtomicU32::new(initial_seed),
        }
    }

    /// Periodically rotates the Temporary Vehicle ID for MAC/PHY privacy
    pub fn maybe_rotate_temp_id(&self, current_time_ms: u64) {
        let last = self.last_temp_id_rotation_ms.load(Ordering::Relaxed);
        if current_time_ms.saturating_sub(last) >= TEMP_ID_ROTATION_INTERVAL_MS {
            // Simple Xorshift32 PRNG for bare-metal privacy ID generation
            let mut x = self.temp_id_seed.load(Ordering::Relaxed);
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.temp_id_seed.store(x, Ordering::Relaxed);

            let new_bytes = x.to_be_bytes();
            unsafe {
                *self.current_temp_id.get() = new_bytes;
            }
            self.last_temp_id_rotation_ms
                .store(current_time_ms, Ordering::SeqCst);
        }
    }

    pub fn get_current_temp_id(&self) -> [u8; 4] {
        unsafe { *self.current_temp_id.get() }
    }

    /// Enqueues an outgoing safety broadcast frame, adjusting frequency based on CBR
    pub fn enqueue_tx_frame(&self, frame: &V2xTxFrame) -> Result<(), V2xError> {
        let cbr = self.channel_busy_ratio_pct.load(Ordering::Relaxed);

        // Decentralized Congestion Control (DCC): Drop non-safety packets if channel load > 60%
        if cbr > 60 && frame.priority > 2 {
            return Err(V2xError::ChannelCongestionThrottled);
        }

        let tail = self.tx_tail.load(Ordering::Relaxed) as usize;
        let head = self.tx_head.load(Ordering::Acquire) as usize;
        let next_tail = (tail + 1) % V2X_TX_QUEUE_DEPTH;

        if next_tail == head {
            return Err(V2xError::TxQueueFull);
        }

        let queue = unsafe { &mut *self.tx_queue.get() };
        queue[tail] = *frame;

        self.tx_tail.store(next_tail as u16, Ordering::Release);
        Ok(())
    }

    /// Dequeues a frame for raw transmission by the baseband SDR / C-V2X PHY chip
    pub fn dequeue_tx_frame(&self) -> Option<V2xTxFrame> {
        let head = self.tx_head.load(Ordering::Relaxed) as usize;
        let tail = self.tx_tail.load(Ordering::Acquire) as usize;

        if head == tail {
            return None;
        }

        let queue = unsafe { &*self.tx_queue.get() };
        let frame = queue[head];
        let next_head = (head + 1) % V2X_TX_QUEUE_DEPTH;

        self.tx_head.store(next_head as u16, Ordering::Release);
        Some(frame)
    }

    pub fn update_cbr(&self, measured_cbr_pct: u8) {
        self.channel_busy_ratio_pct
            .store(measured_cbr_pct.min(100), Ordering::Relaxed);
    }
}

// =================================================================================================
// 7. VERIFICATION SUITE & UNIT TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asn1_bitstream_read_write() {
        let mut raw_buf = [0u8; 16];
        {
            let mut writer = Asn1BitStream::new_writer(&mut raw_buf);
            writer.write_bit(true).expect("Bit ok");
            writer.write_bit(false).expect("Bit ok");
            writer.write_bits(0b10110, 5).expect("Bits ok");
            writer
                .write_constrained_int(1234, 1000, 2000)
                .expect("Int ok");
        }

        {
            let mut reader = Asn1BitStream::new_reader(&mut raw_buf, 16 * 8);
            assert_eq!(reader.read_bit(), Ok(true));
            assert_eq!(reader.read_bit(), Ok(false));
            assert_eq!(reader.read_bits(5), Ok(0b10110));
            assert_eq!(reader.read_constrained_int(1000, 2000), Ok(1234));
        }
    }

    #[test]
    fn test_bsm_core_encode_decode_roundtrip() {
        let mut original_bsm = J2735BsmCoreData::empty();
        original_bsm.msg_count = 42;
        original_bsm.temp_id = [0x11, 0x22, 0x33, 0x44];
        original_bsm.dsecond_ms = 45120;
        original_bsm.latitude_deg = 37.774929;
        original_bsm.longitude_deg = -122.419416;
        original_bsm.elevation_m = 45.2;
        original_bsm.speed_m_s = 28.5; // ~102 km/h
        original_bsm.heading_deg = 184.5;
        original_bsm.steering_wheel_angle_deg = -3.0;
        original_bsm.accel_set.longitudinal_m_s2 = -1.25;
        original_bsm.accel_set.lateral_m_s2 = 0.45;
        original_bsm.vehicle_width_cm = 195;
        original_bsm.vehicle_length_cm = 490;

        let mut packet_buf = [0u8; 256];
        let bytes_written;
        {
            let mut writer = Asn1BitStream::new_writer(&mut packet_buf);
            original_bsm.encode_uper(&mut writer).expect("Encode ok");
            bytes_written = writer.bytes_written();
        }

        assert!(bytes_written > 30 && bytes_written < 60);

        let decoded_bsm;
        {
            let mut reader = Asn1BitStream::new_reader(&mut packet_buf, bytes_written * 8);
            decoded_bsm = J2735BsmCoreData::decode_uper(&mut reader).expect("Decode ok");
        }

        assert_eq!(decoded_bsm.msg_count, 42);
        assert_eq!(decoded_bsm.temp_id, [0x11, 0x22, 0x33, 0x44]);
        assert!((decoded_bsm.latitude_deg - 37.774929).abs() < 0.000001);
        assert!((decoded_bsm.longitude_deg - (-122.419416)).abs() < 0.000001);
        assert!((decoded_bsm.speed_m_s - 28.5).abs() < 0.05);
        assert!((decoded_bsm.heading_deg - 184.5).abs() < 0.02);
    }

    #[test]
    fn test_v2x_congestion_control_and_id_rotation() {
        let radio = V2xRadioController::new(0x12345678);
        let id1 = radio.get_current_temp_id();

        // Advance time and check ID rotation
        radio.maybe_rotate_temp_id(TEMP_ID_ROTATION_INTERVAL_MS + 10);
        let id2 = radio.get_current_temp_id();
        assert_ne!(id1, id2);

        // Test congestion throttling: High CBR drops non-critical frames
        radio.update_cbr(75); // Severe congestion
        let mut low_priority_frame = V2xTxFrame::empty();
        low_priority_frame.priority = 5;

        let res = radio.enqueue_tx_frame(&low_priority_frame);
        assert_eq!(res, Err(V2xError::ChannelCongestionThrottled));

        // ASIL-B / Safety frame must still enqueue
        let mut safety_frame = V2xTxFrame::empty();
        safety_frame.priority = 0;
        assert_eq!(radio.enqueue_tx_frame(&safety_frame), Ok(()));
    }
}
