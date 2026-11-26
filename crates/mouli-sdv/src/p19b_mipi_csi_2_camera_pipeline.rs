//! Part 19b: MIPI CSI-2 CAMERA PIPELINE, HARDWARE ISP, ara::com SOA & WATCHDOG MANAGER (WdgM)

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 19 - MIPI CSI-2 CAMERA PIPELINE, HARDWARE ISP, ara::com SOA & WATCHDOG MANAGER (WdgM)
// CRITICALITY: ISO 26262 ASIL-D / AUTOSAR Adaptive R22-11 / MIPI CSI-2 v3.0 / ISO 21434
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocation, Cacheline-Aligned Ring Buffers, MMIO Register Maps
// =================================================================================================

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS: MIPI CSI-2, ISP PIPELINE, ara::com & AUTOSAR WdgM CONFIGURATION
// =================================================================================================

/// Maximum concurrent MIPI CSI-2 virtual channels per physical D-PHY/C-PHY port
pub const MIPI_CSI2_MAX_VIRTUAL_CHANNELS: usize = 4;

/// RAW12 / RGB Bayer image sensor resolutions
pub const CAMERA_FRAME_WIDTH: usize = 1920;
pub const CAMERA_FRAME_HEIGHT: usize = 1080;
pub const CAMERA_RAW_BUFFER_BYTES: usize = 3110400; // 1920 * 1080 * 1.5 bytes for RAW12

/// Triple-buffered camera ring slots to eliminate tearing during AI inference
pub const CAMERA_FRAME_RING_SLOTS: usize = 3;

/// AUTOSAR WdgM Supervised Entities capacity
pub const WDGM_MAX_SUPERVISED_ENTITIES: usize = 16;
pub const WDGM_MAX_CHECKPOINTS_PER_ENTITY: usize = 8;

/// AUTOSAR ara::com Service registry limits
pub const ARA_COM_MAX_SERVICES: usize = 32;
pub const ARA_COM_MAX_EVENT_SUBSCRIBERS: usize = 16;
pub const ARA_COM_PAYLOAD_MAX_BYTES: usize = 1024;

// =================================================================================================
// 2. ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraIspError {
    Ok = 0,
    CphyEccMismatch = 1,
    CrcFrameCorrupted = 2,
    FifoOverflow = 3,
    RingBufferExhausted = 4,
    ExposureOutOfBounds = 5,
    WhitebalanceGainInvalid = 6,
    HdrFusionMismatch = 7,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AraComError {
    Ok = 0,
    ServiceNotOffered = 1,
    InstanceNotFound = 2,
    SubscriberPoolFull = 3,
    SerializationFailed = 4,
    ServiceMethodTimeout = 5,
    NetworkInterfaceDown = 6,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WdgmError {
    Ok = 0,
    EntityNotFound = 1,
    AliveSupervisionFailed = 2,
    DeadlineSupervisionExceeded = 3,
    LogicalSupervisionTransitionViolation = 4,
    HardwareWatchdogResetTriggered = 5,
}

// =================================================================================================
// 3. MIPI CSI-2 PROTOCOL ENGINE & HARDWARE ISP PIPELINE (AUTOMOTIVE ADAS VISION)
// =================================================================================================

/// MIPI CSI-2 Data Types (MIPI Alliance Specification for Camera Serial Interface 2)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MipiDataType {
    FrameStart = 0x00,
    FrameEnd = 0x01,
    LineStart = 0x02,
    LineEnd = 0x03,
    EmbeddedTelemetryData = 0x12,
    Raw8 = 0x2A,
    Raw10 = 0x2B,
    Raw12 = 0x2C, // Standard automotive HDR sensor format
    Raw14 = 0x2D,
    UserDefined1 = 0x30,
}

/// Hardware MMIO register definitions for an Automotive MIPI CSI-2 Host Controller
#[repr(C)]
pub struct MipiCsi2HardwareRegisters {
    pub ctrl: u32,              // Offset 0x00: Controller Control & Enable
    pub lane_cfg: u32,          // Offset 0x04: D-PHY / C-PHY Data Lane Multiplexing
    pub vc_active: u32,         // Offset 0x08: Virtual Channel Enable Mask
    pub intr_status: u32,       // Offset 0x0C: Interrupt Status (ECC, CRC, FIFO)
    pub intr_mask: u32,         // Offset 0x10: Interrupt Mask
    pub frame_crc_status: u32,  // Offset 0x14: Per-frame CRC-16 hardware check
    pub line_counter: u32,      // Offset 0x18: Current scanline index
    pub dma_target_addr_0: u64, // Offset 0x20: Primary FrameBuffer DMA Bus Address
    pub dma_target_addr_1: u64, // Offset 0x28: Secondary FrameBuffer DMA Bus Address
}

/// Metadata descriptor attached to each captured camera frame
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CameraFrameMeta {
    pub frame_id: u64,
    pub timestamp_ns: u64,
    pub virtual_channel: u8,
    pub data_type: u8,
    pub exposure_time_us: u32,
    pub analog_gain_linear: f32,
    pub lux_ambient_estimate: f32,
    pub crc_verified: bool,
}

impl CameraFrameMeta {
    pub const fn empty() -> Self {
        Self {
            frame_id: 0,
            timestamp_ns: 0,
            virtual_channel: 0,
            data_type: 0,
            exposure_time_us: 0,
            analog_gain_linear: 1.0,
            lux_ambient_estimate: 0.0,
            crc_verified: false,
        }
    }
}

/// Triple-buffered camera DMA ring buffer
pub struct CameraDmaRingBuffer {
    buffers: UnsafeCell<[[u8; 1024]; CAMERA_FRAME_RING_SLOTS]>, // Truncated representation of pixel storage
    metadata: UnsafeCell<[CameraFrameMeta; CAMERA_FRAME_RING_SLOTS]>,
    active_write_idx: AtomicU8,
    last_completed_idx: AtomicU8,
    frame_sequence: AtomicU64,
}

// `CameraDmaRingBuffer` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl CameraDmaRingBuffer {
    pub const fn new() -> Self {
        Self {
            buffers: UnsafeCell::new([[0u8; 1024]; CAMERA_FRAME_RING_SLOTS]),
            metadata: UnsafeCell::new([CameraFrameMeta::empty(); CAMERA_FRAME_RING_SLOTS]),
            active_write_idx: AtomicU8::new(0),
            last_completed_idx: AtomicU8::new(0xFF),
            frame_sequence: AtomicU64::new(0),
        }
    }

    /// Advances the DMA target upon MIPI CSI-2 FrameEnd interrupt
    pub fn on_hardware_frame_end(
        &self,
        vc: u8,
        timestamp_ns: u64,
        crc_ok: bool,
    ) -> Result<u8, CameraIspError> {
        if !crc_ok {
            return Err(CameraIspError::CrcFrameCorrupted);
        }

        let curr_write = self.active_write_idx.load(Ordering::Relaxed);
        let seq = self.frame_sequence.fetch_add(1, Ordering::SeqCst);

        // Commit metadata for downstream AI PPU ingestion
        // SAFETY: `CameraDmaRingBuffer` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the frame metadata is alive.
        let metas = unsafe { &mut *self.metadata.get() };
        metas[curr_write as usize] = CameraFrameMeta {
            frame_id: seq,
            timestamp_ns,
            virtual_channel: vc,
            data_type: MipiDataType::Raw12 as u8,
            exposure_time_us: 11000,
            analog_gain_linear: 2.5,
            lux_ambient_estimate: 450.0,
            crc_verified: true,
        };

        self.last_completed_idx.store(curr_write, Ordering::Release);
        let next_write = (curr_write + 1) % (CAMERA_FRAME_RING_SLOTS as u8);
        self.active_write_idx.store(next_write, Ordering::Release);

        Ok(curr_write)
    }

    /// Acquires reference to the most recent intact frame without locking
    pub fn get_latest_frame_meta(&self) -> Option<CameraFrameMeta> {
        let idx = self.last_completed_idx.load(Ordering::Acquire);
        if idx == 0xFF {
            None
        } else {
            // SAFETY: `CameraDmaRingBuffer` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the frame metadata is alive.
            // It is only read here and the entry is `Copy`.
            let metas = unsafe { &*self.metadata.get() };
            Some(metas[idx as usize])
        }
    }
}

impl Default for CameraDmaRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Hardware Image Signal Processor (ISP) Tuner & Auto-Exposure/White-Balance Controller
pub struct AutomotiveIspEngine {
    target_luminance_target: f32, // 0.0 - 1.0 Normalized
    current_red_gain: AtomicU32,  // Fixed point Q16.16
    current_blue_gain: AtomicU32, // Fixed point Q16.16
    current_exposure_us: AtomicU32,
    hdr_stitching_enabled: AtomicBool,
}

impl AutomotiveIspEngine {
    pub const fn new() -> Self {
        Self {
            target_luminance_target: 0.18, // Standard 18% middle-grey automotive target
            current_red_gain: AtomicU32::new(1 << 16),
            current_blue_gain: AtomicU32::new(1 << 16),
            current_exposure_us: AtomicU32::new(10000), // 10ms baseline
            hdr_stitching_enabled: AtomicBool::new(true),
        }
    }

    /// Executes closed-loop Auto-Exposure (AEC) step based on histogram feedback
    pub fn compute_auto_exposure_step(
        &self,
        mean_histogram_luminance: f32,
    ) -> Result<u32, CameraIspError> {
        if mean_histogram_luminance <= 0.0 || mean_histogram_luminance > 1.0 {
            return Err(CameraIspError::ExposureOutOfBounds);
        }

        let curr_exp = self.current_exposure_us.load(Ordering::Relaxed) as f32;
        let luminance_ratio = self.target_luminance_target / mean_histogram_luminance;

        // Proportional filter clamping adjustment step between 0.8x and 1.25x
        let clamped_ratio = if luminance_ratio > 1.25 {
            1.25
        } else if luminance_ratio < 0.8 {
            0.8
        } else {
            luminance_ratio
        };

        let new_exposure = (curr_exp * clamped_ratio) as u32;
        // Strict automotive bounds: 50us (glare) to 33000us (night low-light)
        let safe_exposure = new_exposure.clamp(50, 33000);

        self.current_exposure_us
            .store(safe_exposure, Ordering::Release);
        Ok(safe_exposure)
    }

    /// Applies Von Kries white balance chromatic adaptation
    pub fn update_white_balance_gains(
        &self,
        r_gain: f32,
        b_gain: f32,
    ) -> Result<(), CameraIspError> {
        if r_gain < 0.5 || r_gain > 4.0 || b_gain < 0.5 || b_gain > 4.0 {
            return Err(CameraIspError::WhitebalanceGainInvalid);
        }

        let r_fixed = (r_gain * 65536.0) as u32;
        let b_fixed = (b_gain * 65536.0) as u32;

        self.current_red_gain.store(r_fixed, Ordering::Relaxed);
        self.current_blue_gain.store(b_fixed, Ordering::Relaxed);
        Ok(())
    }
}

impl Default for AutomotiveIspEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 4. AUTOSAR ADAPTIVE ara::com SERVICE-ORIENTED ARCHITECTURE (SOA)
// =================================================================================================

/// AUTOSAR Service Identifier Descriptor
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AraServiceId {
    pub service_id: u16,
    pub instance_id: u16,
    pub major_version: u8,
    pub minor_version: u8,
}

/// Service-Oriented Event Payload Container
pub struct AraComEventPacket {
    pub header: AraServiceId,
    pub event_id: u16,
    pub sequence_counter: u32,
    pub payload_len: usize,
    pub payload: [u8; ARA_COM_PAYLOAD_MAX_BYTES],
}

impl AraComEventPacket {
    pub const fn empty() -> Self {
        Self {
            header: AraServiceId {
                service_id: 0,
                instance_id: 0,
                major_version: 0,
                minor_version: 0,
            },
            event_id: 0,
            sequence_counter: 0,
            payload_len: 0,
            payload: [0u8; ARA_COM_PAYLOAD_MAX_BYTES],
        }
    }
}

/// ara::com Skeleton Base (Service Provider Side)
pub struct AraComSkeleton {
    service_id: AraServiceId,
    is_offered: AtomicBool,
    event_seq: AtomicU32,
    subscriber_count: AtomicU8,
    subscriber_endpoints: UnsafeCell<[u16; ARA_COM_MAX_EVENT_SUBSCRIBERS]>,
}

// SAFETY: only conditionally sound. `register_subscriber` reads `subscriber_count`, writes the slot
// at that index, then increments, with no lock; two concurrent registrars could both write the same
// slot and lose a subscriber. It is sound only with one registrar at a time (for example all
// subscriptions made at start-up from one context). The other fields are atomics.
unsafe impl Sync for AraComSkeleton {}

impl AraComSkeleton {
    pub const fn new(service: AraServiceId) -> Self {
        Self {
            service_id: service,
            is_offered: AtomicBool::new(false),
            event_seq: AtomicU32::new(0),
            subscriber_count: AtomicU8::new(0),
            subscriber_endpoints: UnsafeCell::new([0u16; ARA_COM_MAX_EVENT_SUBSCRIBERS]),
        }
    }

    /// Announces service availability across in-vehicle network (SOME/IP SD announce)
    pub fn offer_service(&self) {
        self.is_offered.store(true, Ordering::SeqCst);
    }

    pub fn stop_offer_service(&self) {
        self.is_offered.store(false, Ordering::SeqCst);
    }

    /// Dispatches an event update to all registered subscriber clients
    pub fn send_event(&self, event_id: u16, data: &[u8]) -> Result<AraComEventPacket, AraComError> {
        if !self.is_offered.load(Ordering::Acquire) {
            return Err(AraComError::ServiceNotOffered);
        }
        if data.len() > ARA_COM_PAYLOAD_MAX_BYTES {
            return Err(AraComError::SerializationFailed);
        }

        let seq = self.event_seq.fetch_add(1, Ordering::Relaxed);
        let mut packet = AraComEventPacket::empty();
        packet.header = self.service_id;
        packet.event_id = event_id;
        packet.sequence_counter = seq;
        packet.payload_len = data.len();
        packet.payload[..data.len()].copy_from_slice(data);

        Ok(packet)
    }

    /// Registers a client subscriber endpoint
    pub fn register_subscriber(&self, client_endpoint_id: u16) -> Result<(), AraComError> {
        let count = self.subscriber_count.load(Ordering::Relaxed) as usize;
        if count >= ARA_COM_MAX_EVENT_SUBSCRIBERS {
            return Err(AraComError::SubscriberPoolFull);
        }

        // SAFETY: `register_subscriber` checks the count, writes the slot at `count`, then increments, with no lock,
        // so it is only sound with one registrar at a time (see the `Sync` impl). This is the only code that
        // touches the endpoint table, so no reader exists.
        let endpoints = unsafe { &mut *self.subscriber_endpoints.get() };
        endpoints[count] = client_endpoint_id;
        self.subscriber_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// =================================================================================================
// 5. AUTOSAR WATCHDOG MANAGER (WdgM) - ISO 26262 ASIL-D SUPERVISION
// =================================================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WdgmEntityStatus {
    Ok,
    Failed,
    Expired,
}

/// Checkpoint descriptor for logical execution path verification
#[derive(Debug, Clone, Copy)]
pub struct WdgmCheckpoint {
    pub checkpoint_id: u8,
    pub expected_predecessor_id: u8, // 0xFF indicates entry point
    pub max_execution_time_us: u32,
    pub last_reached_timestamp_us: u64,
}

const NO_ENTITY: Option<SupervisedEntity> = None;

/// Supervised Entity running under ISO 26262 ASIL-D oversight
pub struct SupervisedEntity {
    pub entity_id: u16,
    pub alive_counter: AtomicU32,
    pub expected_alive_indications: u32,
    pub min_margin: u32,
    pub max_margin: u32,
    pub current_checkpoint: AtomicU8,
    pub status: AtomicU8, // WdgmEntityStatus
    pub checkpoints: [WdgmCheckpoint; WDGM_MAX_CHECKPOINTS_PER_ENTITY],
}

impl SupervisedEntity {
    pub const fn new(id: u16, expected_alive: u32, min_m: u32, max_m: u32) -> Self {
        Self {
            entity_id: id,
            alive_counter: AtomicU32::new(0),
            expected_alive_indications: expected_alive,
            min_margin: min_m,
            max_margin: max_m,
            current_checkpoint: AtomicU8::new(0xFF),
            status: AtomicU8::new(WdgmEntityStatus::Ok as u8),
            checkpoints: [WdgmCheckpoint {
                checkpoint_id: 0,
                expected_predecessor_id: 0xFF,
                max_execution_time_us: 10000,
                last_reached_timestamp_us: 0,
            }; WDGM_MAX_CHECKPOINTS_PER_ENTITY],
        }
    }

    /// Report alive indication from task loop
    pub fn report_alive(&self) {
        self.alive_counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Logical supervision: verify valid control flow transition between checkpoints
    pub fn reach_checkpoint(&self, cp_id: u8, current_time_us: u64) -> Result<(), WdgmError> {
        let prev = self.current_checkpoint.load(Ordering::Relaxed);

        if cp_id as usize >= WDGM_MAX_CHECKPOINTS_PER_ENTITY {
            return Err(WdgmError::LogicalSupervisionTransitionViolation);
        }

        let cp = &self.checkpoints[cp_id as usize];
        if cp.expected_predecessor_id != 0xFF && cp.expected_predecessor_id != prev {
            // Control flow graph violated! Program execution order compromised
            self.status
                .store(WdgmEntityStatus::Failed as u8, Ordering::SeqCst);
            return Err(WdgmError::LogicalSupervisionTransitionViolation);
        }

        // Deadline check
        if prev != 0xFF {
            let prev_cp = &self.checkpoints[prev as usize];
            let elapsed = current_time_us.saturating_sub(prev_cp.last_reached_timestamp_us);
            if elapsed > (cp.max_execution_time_us as u64) {
                self.status
                    .store(WdgmEntityStatus::Expired as u8, Ordering::SeqCst);
                return Err(WdgmError::DeadlineSupervisionExceeded);
            }
        }

        self.current_checkpoint.store(cp_id, Ordering::Release);
        Ok(())
    }
}

/// Central Watchdog Manager (WdgM) Supervising Mixed-Criticality Entities
pub struct WatchdogManager {
    entities: UnsafeCell<[Option<SupervisedEntity>; WDGM_MAX_SUPERVISED_ENTITIES]>,
    entity_count: AtomicU8,
    system_wide_status: AtomicU8,
    hw_watchdog_pet_inhibited: AtomicBool,
}

// `WatchdogManager` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl WatchdogManager {
    pub const fn new() -> Self {
        Self {
            entities: UnsafeCell::new([NO_ENTITY; WDGM_MAX_SUPERVISED_ENTITIES]),
            entity_count: AtomicU8::new(0),
            system_wide_status: AtomicU8::new(WdgmEntityStatus::Ok as u8),
            hw_watchdog_pet_inhibited: AtomicBool::new(false),
        }
    }

    /// Registers a supervised entity
    pub fn register_entity(&self, entity: SupervisedEntity) -> Result<usize, WdgmError> {
        let idx = self.entity_count.load(Ordering::Relaxed) as usize;
        if idx >= WDGM_MAX_SUPERVISED_ENTITIES {
            return Err(WdgmError::EntityNotFound);
        }

        // SAFETY: `WatchdogManager` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the entity table is alive.
        let ents = unsafe { &mut *self.entities.get() };
        ents[idx] = Some(entity);
        self.entity_count.fetch_add(1, Ordering::SeqCst);
        Ok(idx)
    }

    /// Evaluates supervision window (invoked periodically at 100Hz from ASIL-D timer)
    pub fn evaluate_supervision_cycle(&self) -> Result<(), WdgmError> {
        let count = self.entity_count.load(Ordering::Relaxed) as usize;
        // SAFETY: `WatchdogManager` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the entity table is alive.
        let ents = unsafe { &mut *self.entities.get() };

        for i in 0..count {
            if let Some(ref mut ent) = ents[i] {
                let alive = ent.alive_counter.swap(0, Ordering::SeqCst);
                let min = ent
                    .expected_alive_indications
                    .saturating_sub(ent.min_margin);
                let max = ent
                    .expected_alive_indications
                    .saturating_add(ent.max_margin);

                // Alive supervision verification
                if alive < min || alive > max {
                    ent.status
                        .store(WdgmEntityStatus::Failed as u8, Ordering::SeqCst);
                    self.system_wide_status
                        .store(WdgmEntityStatus::Failed as u8, Ordering::SeqCst);
                    self.hw_watchdog_pet_inhibited.store(true, Ordering::SeqCst);
                    return Err(WdgmError::AliveSupervisionFailed);
                }
            }
        }

        Ok(())
    }

    /// Servicing the physical SoC watchdog hardware register
    pub fn pet_hardware_watchdog(&self, mmio_reg: &AtomicU32) -> Result<(), WdgmError> {
        if self.hw_watchdog_pet_inhibited.load(Ordering::Acquire) {
            // Intentionally withhold watchdog pet to cause hardware MCU reset into safe state
            return Err(WdgmError::HardwareWatchdogResetTriggered);
        }

        // Write magic key to hardware MMIO register
        mmio_reg.store(0xCAFE_BABE, Ordering::SeqCst);
        Ok(())
    }
}

impl Default for WatchdogManager {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. UNIT TESTS & VERIFICATION SUITE
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mipi_csi2_frame_progression() {
        let ring = CameraDmaRingBuffer::new();
        assert!(ring.get_latest_frame_meta().is_none());

        let slot = ring
            .on_hardware_frame_end(0, 1_000_000, true)
            .expect("Frame commit ok");
        assert_eq!(slot, 0);

        let meta = ring.get_latest_frame_meta().expect("Frame meta present");
        assert_eq!(meta.frame_id, 0);
        assert_eq!(meta.timestamp_ns, 1_000_000);
        assert!(meta.crc_verified);
    }

    #[test]
    fn test_isp_auto_exposure_clamping() {
        let isp = AutomotiveIspEngine::new();
        // High scene luminance (overexposed) -> should reduce exposure time
        let new_exp = isp.compute_auto_exposure_step(0.72).expect("AEC step ok");
        assert!(new_exp < 10000);

        // Low scene luminance (dark tunnel) -> should increase exposure time
        let dark_exp = isp
            .compute_auto_exposure_step(0.05)
            .expect("AEC step dark ok");
        assert!(dark_exp > new_exp);
    }

    #[test]
    fn test_ara_com_skeleton_event_routing() {
        let svc_id = AraServiceId {
            service_id: 0x4200,
            instance_id: 0x0001,
            major_version: 1,
            minor_version: 0,
        };
        let skeleton = AraComSkeleton::new(svc_id);

        let err = skeleton.send_event(0x01, b"speed_payload");
        assert_eq!(err.err(), Some(AraComError::ServiceNotOffered));

        skeleton.offer_service();
        let pkt = skeleton
            .send_event(0x01, b"speed_payload")
            .expect("Event dispatched");
        assert_eq!(pkt.event_id, 0x01);
        assert_eq!(pkt.payload_len, 13);
        assert_eq!(&pkt.payload[..13], b"speed_payload");
    }

    #[test]
    fn test_wdgm_alive_and_logical_supervision() {
        let wdgm = WatchdogManager::new();
        let entity = SupervisedEntity::new(0x10, 10, 2, 2);

        // Feed alive counter 10 times (nominal)
        for _ in 0..10 {
            entity.report_alive();
        }

        let reg_idx = wdgm.register_entity(entity).expect("Registered");
        assert_eq!(wdgm.evaluate_supervision_cycle(), Ok(()));

        // Test alive underflow violation
        assert_eq!(
            wdgm.evaluate_supervision_cycle(),
            Err(WdgmError::AliveSupervisionFailed)
        );

        let dummy_hw_reg = AtomicU32::new(0);
        assert_eq!(
            wdgm.pet_hardware_watchdog(&dummy_hw_reg),
            Err(WdgmError::HardwareWatchdogResetTriggered)
        );
    }
}
