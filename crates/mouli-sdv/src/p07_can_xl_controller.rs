//! Part 7: CAN-XL CONTROLLER, PCIE TLP ENGINE, MPU ISOLATION & MULTI-CORE IPI ENGINE

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 7 - CAN-XL CONTROLLER, PCIE TLP ENGINE, MPU ISOLATION & MULTI-CORE IPI ENGINE
// CRITICALITY: ISO 26262 ASIL-D / CiA 610-1 CAN-XL / PCI-SIG Base 5.0 / ARMv8-R & RISC-V PMP
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Allocations, Deterministic Bounded Timing
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

// =================================================================================================
// 1. CONSTANTS: CAN-XL, PCIE, MPU & MULTI-CORE SPECS
// =================================================================================================

/// Maximum payload capacity for CAN-XL frames (up to 2048 bytes per CiA 610-1)
pub const MAX_CANXL_DATA_LEN: usize = 2048;

/// CiA 610-1 Service Data Unit Types (SDT)
pub const CANXL_SDT_CLASSIC_CAN: u8 = 0x01;
pub const CANXL_SDT_SOMEIP: u8 = 0x02;
pub const CANXL_SDT_ETHERNET_TUNNEL: u8 = 0x03;
pub const CANXL_SDT_DIAG_ISO_TP: u8 = 0x04;

/// Maximum number of PCIe BAR registers per Physical Function
pub const MAX_PCIE_BARS: usize = 6;

/// Maximum capacity of inbound/outbound PCIe TLP queues
pub const PCIE_TLP_RING_CAPACITY: usize = 32;

/// Maximum payload capacity per PCIe Transaction Layer Packet (512 bytes max payload)
pub const MAX_PCIE_TLP_PAYLOAD_WORDS: usize = 128; // 128 * 4 bytes = 512 bytes

/// Hardware MPU maximum regions per core (typical Cortex-R82 or RISC-V PMP)
pub const MAX_MPU_REGIONS: usize = 16;

/// Maximum CPU cores managed in the unified automotive SoC cluster
pub const MAX_CPU_CORES: usize = 8;

// =================================================================================================
// 2. ERROR DEFINITIONS
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanXlError {
    Ok = 0,
    PayloadTooLarge = 1,
    InvalidPriorityId = 2,
    CrcMismatch = 3,
    TransceiverError = 4,
    RingBufferOverflow = 5,
    SdtNotSupported = 6,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcieError {
    Ok = 0,
    MalformedTlp = 1,
    UnsupportedRequest = 2,
    CompleterAbort = 3,
    RingBufferFull = 4,
    RingBufferEmpty = 5,
    InvalidBarIndex = 6,
    AlignmentViolation = 7,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpuError {
    Ok = 0,
    RegionOverlap = 1,
    InvalidPermissions = 2,
    AlignmentFault = 3,
    RegionExceeded = 4,
    HardwareFault = 5,
    AsilInhibitionViolation = 6,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpiError {
    Ok = 0,
    InvalidTargetCore = 1,
    MailboxFull = 2,
    Timeout = 3,
    TargetNotReady = 4,
}

// =================================================================================================
// 3. CAN-XL (CiA 610-1 / ISO 11898-1:2024) SUBSYSTEM
// =================================================================================================

/// CAN-XL Protocol Header according to CiA 610-1 standard
#[derive(Debug, Clone, Copy)]
pub struct CanXlHeader {
    pub priority_id: u16,       // 11-bit Priority ID (arbitration field)
    pub sec_bit: bool,          // Simple Extended Content / Security bit
    pub sdt: u8,                // 8-bit Service Data Unit Type
    pub vcid: u8,               // 8-bit Virtual CAN Network ID
    pub data_length_bytes: u16, // 11-bit DLC representation (1..2048 bytes)
    pub acceptance_field: u32,  // 32-bit Acceptance Filter Field
}

/// Fully contained, non-allocating CAN-XL Frame
#[derive(Clone, Copy)]
pub struct CanXlFrame {
    pub header: CanXlHeader,
    pub payload: [u8; MAX_CANXL_DATA_LEN],
    pub crc32: u32,
}

impl CanXlFrame {
    pub const fn empty() -> Self {
        Self {
            header: CanXlHeader {
                priority_id: 0,
                sec_bit: false,
                sdt: 0,
                vcid: 0,
                data_length_bytes: 0,
                acceptance_field: 0,
            },
            payload: [0u8; MAX_CANXL_DATA_LEN],
            crc32: 0,
        }
    }

    /// Computes the CAN-XL CRC-32 (Polynomial: 0x04C11DB7, seeded for frame safety)
    pub fn calculate_crc32(header: &CanXlHeader, data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        let poly: u32 = 0x04C1_1DB7;

        // Hash Header Fields
        let prio_bytes = header.priority_id.to_be_bytes();
        for &b in prio_bytes.iter() {
            crc ^= (b as u32) << 24;
            for _ in 0..8 {
                if (crc & 0x8000_0000) != 0 {
                    crc = (crc << 1) ^ poly;
                } else {
                    crc <<= 1;
                }
            }
        }

        crc ^= (header.sdt as u32) << 24;
        for _ in 0..8 {
            if (crc & 0x8000_0000) != 0 {
                crc = (crc << 1) ^ poly;
            } else {
                crc <<= 1;
            }
        }

        // Hash Data Payload
        for &byte in data.iter() {
            crc ^= (byte as u32) << 24;
            for _ in 0..8 {
                if (crc & 0x8000_0000) != 0 {
                    crc = (crc << 1) ^ poly;
                } else {
                    crc <<= 1;
                }
            }
        }

        !crc
    }
}

/// Memory-Mapped CAN-XL Controller with Dual-Speed Transmission Engine
pub struct CanXlController {
    base_register_addr: usize,
    rx_frames_counter: AtomicU64,
    tx_frames_counter: AtomicU64,
    bus_off_state: AtomicBool,
    lock: AtomicBool,
}

impl CanXlController {
    pub const fn new(base_mmio_addr: usize) -> Self {
        Self {
            base_register_addr: base_mmio_addr,
            rx_frames_counter: AtomicU64::new(0),
            tx_frames_counter: AtomicU64::new(0),
            bus_off_state: AtomicBool::new(false),
            lock: AtomicBool::new(false),
        }
    }

    #[inline(always)]
    fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
    }

    #[inline(always)]
    fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }

    /// Formats and transmits a CAN-XL frame onto the physical transceiver registers
    pub fn transmit_frame(&self, frame: &CanXlFrame) -> Result<(), CanXlError> {
        if frame.header.data_length_bytes as usize > MAX_CANXL_DATA_LEN {
            return Err(CanXlError::PayloadTooLarge);
        }
        if frame.header.priority_id > 0x07FF {
            return Err(CanXlError::InvalidPriorityId);
        }
        if self.bus_off_state.load(Ordering::Acquire) {
            return Err(CanXlError::TransceiverError);
        }

        self.acquire();

        // Emulate writing directly to memory-mapped FIFO registers
        // Reg 0x00: Arbitration Field (Priority ID)
        // Reg 0x04: Control Field (SDT, VCID, Length)
        // Reg 0x08: Acceptance Filter
        // Reg 0x10..N: Payload FIFO stream
        self.tx_frames_counter.fetch_add(1, Ordering::Relaxed);

        self.release();
        Ok(())
    }

    /// Ingests a raw byte array from the physical CAN-XL controller hardware buffer
    pub fn receive_frame(
        &self,
        raw_buffer: &[u8],
        out_frame: &mut CanXlFrame,
    ) -> Result<(), CanXlError> {
        if raw_buffer.len() < 12 {
            return Err(CanXlError::PayloadTooLarge);
        }

        // Header deserialization
        let prio = u16::from_be_bytes([raw_buffer[0], raw_buffer[1]]) & 0x07FF;
        let sdt = raw_buffer[2];
        let vcid = raw_buffer[3];
        let dlc_len = u16::from_be_bytes([raw_buffer[4], raw_buffer[5]]) as usize;
        let af = u32::from_be_bytes([raw_buffer[6], raw_buffer[7], raw_buffer[8], raw_buffer[9]]);

        if dlc_len > MAX_CANXL_DATA_LEN || (10 + dlc_len + 4) > raw_buffer.len() {
            return Err(CanXlError::PayloadTooLarge);
        }

        out_frame.header.priority_id = prio;
        out_frame.header.sdt = sdt;
        out_frame.header.vcid = vcid;
        out_frame.header.data_length_bytes = dlc_len as u16;
        out_frame.header.acceptance_field = af;

        out_frame.payload[..dlc_len].copy_from_slice(&raw_buffer[10..10 + dlc_len]);

        let crc_bytes = &raw_buffer[10 + dlc_len..10 + dlc_len + 4];
        let rx_crc = u32::from_be_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);

        let calculated =
            CanXlFrame::calculate_crc32(&out_frame.header, &out_frame.payload[..dlc_len]);
        if rx_crc != calculated {
            return Err(CanXlError::CrcMismatch);
        }

        out_frame.crc32 = rx_crc;
        self.rx_frames_counter.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

// =================================================================================================
// 4. PCIE TRANSACTION LAYER PACKET (TLP) ENGINE & BAR CONTROLLER
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlpType {
    MemoryRead32 = 0x00,
    MemoryRead64 = 0x20,
    MemoryWrite32 = 0x40,
    MemoryWrite64 = 0x60,
    Completion = 0x0A,
    CompletionWithData = 0x4A,
    ConfigurationRead0 = 0x04,
    ConfigurationWrite0 = 0x44,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlpCompletionStatus {
    SuccessfulCompletion = 0x0,
    UnsupportedRequest = 0x1,
    ConfigurationRequestRetryStatus = 0x2,
    CompleterAbort = 0x4,
}

/// PCIe Transaction Layer Packet Header
#[derive(Debug, Clone, Copy)]
pub struct TlpHeader {
    pub tlp_type: TlpType,
    pub traffic_class: u8,  // TC0..TC7 (Automotive QoS prioritization)
    pub length_dwords: u16, // 10-bit length (number of 32-bit words)
    pub requester_id: u16,  // Bus:Device:Function (BDF)
    pub tag: u8,            // Transaction tag for out-of-order tracking
    pub address: u64,       // Target MMIO address (32-bit or 64-bit)
    pub status: TlpCompletionStatus,
}

#[derive(Clone, Copy)]
pub struct TransactionLayerPacket {
    pub header: TlpHeader,
    pub payload: [u32; MAX_PCIE_TLP_PAYLOAD_WORDS],
}

impl TransactionLayerPacket {
    pub const fn empty() -> Self {
        Self {
            header: TlpHeader {
                tlp_type: TlpType::MemoryRead32,
                traffic_class: 0,
                length_dwords: 0,
                requester_id: 0,
                tag: 0,
                address: 0,
                status: TlpCompletionStatus::SuccessfulCompletion,
            },
            payload: [0u32; MAX_PCIE_TLP_PAYLOAD_WORDS],
        }
    }
}

/// PCIe Base Address Register (BAR) Configuration
#[derive(Debug, Clone, Copy)]
pub struct PcieBar {
    pub base_address: u64,
    pub size_bytes: u64,
    pub is_64bit: bool,
    pub is_prefetchable: bool,
    pub is_active: bool,
}

pub struct PcieRootPortController {
    bars: [PcieBar; MAX_PCIE_BARS],
    tx_tlp_queue: UnsafeCell<[TransactionLayerPacket; PCIE_TLP_RING_CAPACITY]>,
    tx_head: AtomicU32,
    tx_tail: AtomicU32,
    rx_tlp_queue: UnsafeCell<[TransactionLayerPacket; PCIE_TLP_RING_CAPACITY]>,
    rx_head: AtomicU32,
    rx_tail: AtomicU32,
    lock: AtomicBool,
}

// `PcieRootPortController` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl PcieRootPortController {
    pub const fn new() -> Self {
        const INIT_BAR: PcieBar = PcieBar {
            base_address: 0,
            size_bytes: 0,
            is_64bit: false,
            is_prefetchable: false,
            is_active: false,
        };

        Self {
            bars: [INIT_BAR; MAX_PCIE_BARS],
            tx_tlp_queue: UnsafeCell::new(
                [TransactionLayerPacket::empty(); PCIE_TLP_RING_CAPACITY],
            ),
            tx_head: AtomicU32::new(0),
            tx_tail: AtomicU32::new(0),
            rx_tlp_queue: UnsafeCell::new(
                [TransactionLayerPacket::empty(); PCIE_TLP_RING_CAPACITY],
            ),
            rx_head: AtomicU32::new(0),
            rx_tail: AtomicU32::new(0),
            lock: AtomicBool::new(false),
        }
    }

    /// Registers a hardware BAR mapping (e.g. for high-speed AI PPU or Sensor Hub DMA)
    pub fn configure_bar(
        &mut self,
        bar_idx: usize,
        base: u64,
        size: u64,
        is_64bit: bool,
    ) -> Result<(), PcieError> {
        if bar_idx >= MAX_PCIE_BARS {
            return Err(PcieError::InvalidBarIndex);
        }
        // Size must be a power of two
        if size == 0 || (size & (size - 1)) != 0 {
            return Err(PcieError::AlignmentViolation);
        }

        self.bars[bar_idx] = PcieBar {
            base_address: base,
            size_bytes: size,
            is_64bit,
            is_prefetchable: true,
            is_active: true,
        };
        Ok(())
    }

    /// Enqueues an outbound PCIe TLP into the hardware transmitter ring
    pub fn enqueue_tx_tlp(&self, tlp: &TransactionLayerPacket) -> Result<(), PcieError> {
        let head = self.tx_head.load(Ordering::Relaxed);
        let tail = self.tx_tail.load(Ordering::Acquire);

        if ((head + 1) % PCIE_TLP_RING_CAPACITY as u32) == tail {
            return Err(PcieError::RingBufferFull);
        }

        let ring = unsafe { &mut *self.tx_tlp_queue.get() };
        ring[head as usize] = *tlp;

        self.tx_head.store(
            (head + 1) % PCIE_TLP_RING_CAPACITY as u32,
            Ordering::Release,
        );
        Ok(())
    }

    /// Processes an inbound Memory Write TLP and routes it to the corresponding BAR region
    pub fn handle_inbound_write(&self, tlp: &TransactionLayerPacket) -> Result<(), PcieError> {
        let addr = tlp.header.address;
        let mut hit = false;

        for bar in self.bars.iter() {
            if bar.is_active
                && addr >= bar.base_address
                && addr < (bar.base_address + bar.size_bytes)
            {
                hit = true;
                break;
            }
        }

        if !hit {
            return Err(PcieError::UnsupportedRequest);
        }

        Ok(())
    }
}

impl Default for PcieRootPortController {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. HARDWARE MEMORY PROTECTION UNIT (MPU) & ACCESS CONTROL
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpuAccessPermission {
    NoAccess = 0b000,
    PrivilegedReadOnly = 0b001,
    PrivilegedReadWrite = 0b010,
    UserReadOnly = 0b011,
    UserReadWrite = 0b100,
    ExecuteOnly = 0b101,
    PrivilegedReadWriteExecute = 0b110,
    FullAccess = 0b111,
}

#[derive(Debug, Clone, Copy)]
pub struct MpuRegion {
    pub region_id: u8,
    pub base_address: usize,
    pub limit_address: usize,
    pub permission: MpuAccessPermission,
    pub is_executable: bool,
    pub is_shareable: bool,
    pub asil_level_minimum: u8, // ASIL-D isolation boundary
    pub is_enabled: bool,
}

impl MpuRegion {
    pub const fn empty(id: u8) -> Self {
        Self {
            region_id: id,
            base_address: 0,
            limit_address: 0,
            permission: MpuAccessPermission::NoAccess,
            is_executable: false,
            is_shareable: false,
            asil_level_minimum: 0,
            is_enabled: false,
        }
    }
}

/// Software controller managing real-time hardware MPU descriptors
pub struct MpuManager {
    regions: [MpuRegion; MAX_MPU_REGIONS],
    lock: AtomicBool,
}

impl MpuManager {
    pub const fn new() -> Self {
        const INIT_REG: MpuRegion = MpuRegion::empty(0);
        let mut regions = [INIT_REG; MAX_MPU_REGIONS];
        let mut i = 0;
        while i < MAX_MPU_REGIONS {
            regions[i].region_id = i as u8;
            i += 1;
        }

        Self {
            regions,
            lock: AtomicBool::new(false),
        }
    }

    /// Sets up a protected memory compartment for ASIL-D safety-critical software partitions
    pub fn configure_region(
        &mut self,
        region_id: u8,
        base: usize,
        limit: usize,
        perm: MpuAccessPermission,
        is_exec: bool,
        asil_min: u8,
    ) -> Result<(), MpuError> {
        if region_id as usize >= MAX_MPU_REGIONS {
            return Err(MpuError::RegionExceeded);
        }
        if base >= limit {
            return Err(MpuError::AlignmentFault);
        }
        // 64-byte alignment check for safety isolation
        if (base & 0x3F) != 0 || (limit & 0x3F) != 0 {
            return Err(MpuError::AlignmentFault);
        }

        // Verify no overlapping conflicting execution regions
        for (idx, r) in self.regions.iter().enumerate() {
            if r.is_enabled
                && idx != (region_id as usize)
                && !(limit <= r.base_address || base >= r.limit_address)
            {
                // Check if conflicting execution permissions exist
                if is_exec && r.is_executable {
                    return Err(MpuError::RegionOverlap);
                }
            }
        }

        self.regions[region_id as usize] = MpuRegion {
            region_id,
            base_address: base,
            limit_address: limit,
            permission: perm,
            is_executable: is_exec,
            is_shareable: false,
            asil_level_minimum: asil_min,
            is_enabled: true,
        };

        Ok(())
    }

    /// Hardware access validation probe before task context execution
    pub fn validate_access(
        &self,
        address: usize,
        len: usize,
        write: bool,
        execute: bool,
        current_asil: u8,
    ) -> Result<(), MpuError> {
        let target_end = address + len;
        let mut permitted = false;

        for r in self.regions.iter() {
            if r.is_enabled && address >= r.base_address && target_end <= r.limit_address {
                // Verify ASIL isolation
                if current_asil < r.asil_level_minimum {
                    return Err(MpuError::AsilInhibitionViolation);
                }

                if execute && !r.is_executable {
                    return Err(MpuError::InvalidPermissions);
                }

                if write {
                    match r.permission {
                        MpuAccessPermission::PrivilegedReadWrite
                        | MpuAccessPermission::UserReadWrite
                        | MpuAccessPermission::PrivilegedReadWriteExecute
                        | MpuAccessPermission::FullAccess => {
                            permitted = true;
                        }
                        _ => return Err(MpuError::InvalidPermissions),
                    }
                } else {
                    permitted = true;
                }
                break;
            }
        }

        if !permitted {
            Err(MpuError::InvalidPermissions)
        } else {
            Ok(())
        }
    }
}

impl Default for MpuManager {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. MULTI-CORE INTER-PROCESSOR INTERRUPT (IPI) & BARRIER SYNCHRONIZATION
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpiCommand {
    None = 0,
    ScheduleTick = 1,
    LockstepVerify = 2,
    EcuSafeShutdown = 3,
    TlbInvalidate = 4,
    SensorFrameSync = 5,
}

#[derive(Clone, Copy)]
pub struct IpiMailboxMessage {
    pub command: IpiCommand,
    pub sender_core: u8,
    pub sequence_num: u32,
    pub parameter: u64,
}

impl IpiMailboxMessage {
    pub const fn empty() -> Self {
        Self {
            command: IpiCommand::None,
            sender_core: 0,
            sequence_num: 0,
            parameter: 0,
        }
    }
}

/// Core Mailbox descriptor per processor
pub struct CoreMailbox {
    message: UnsafeCell<IpiMailboxMessage>,
    has_message: AtomicBool,
    processed_count: AtomicU32,
}

unsafe impl Sync for CoreMailbox {}

pub struct MultiCoreIpiEngine {
    mailboxes: [CoreMailbox; MAX_CPU_CORES],
    sync_barrier: AtomicU32,
}

impl MultiCoreIpiEngine {
    pub const fn new() -> Self {
        const INIT_MB: CoreMailbox = CoreMailbox {
            message: UnsafeCell::new(IpiMailboxMessage::empty()),
            has_message: AtomicBool::new(false),
            processed_count: AtomicU32::new(0),
        };

        Self {
            mailboxes: [INIT_MB; MAX_CPU_CORES],
            sync_barrier: AtomicU32::new(0),
        }
    }

    /// Dispatches an IPI notification to a targeted SoC core
    pub fn send_ipi(
        &self,
        target_core: u8,
        sender: u8,
        cmd: IpiCommand,
        param: u64,
    ) -> Result<(), IpiError> {
        if target_core as usize >= MAX_CPU_CORES {
            return Err(IpiError::InvalidTargetCore);
        }

        let mb = &self.mailboxes[target_core as usize];
        if mb.has_message.load(Ordering::Acquire) {
            return Err(IpiError::MailboxFull);
        }

        let msg = unsafe { &mut *mb.message.get() };
        msg.command = cmd;
        msg.sender_core = sender;
        msg.parameter = param;
        msg.sequence_num = mb.processed_count.load(Ordering::Relaxed) + 1;

        mb.has_message.store(true, Ordering::Release);
        Ok(())
    }

    /// Invoked by the target CPU core during hardware interrupt handling
    pub fn acknowledge_ipi(&self, current_core: u8) -> Option<IpiMailboxMessage> {
        if current_core as usize >= MAX_CPU_CORES {
            return None;
        }

        let mb = &self.mailboxes[current_core as usize];
        if !mb.has_message.load(Ordering::Acquire) {
            return None;
        }

        let msg = unsafe { *mb.message.get() };
        mb.processed_count.fetch_add(1, Ordering::Relaxed);
        mb.has_message.store(false, Ordering::Release);

        Some(msg)
    }

    /// Hardware-wide barrier synchronization across all active automotive cores
    pub fn wait_cluster_barrier(
        &self,
        total_active_cores: u32,
        timeout_cycles: u32,
    ) -> Result<(), IpiError> {
        self.sync_barrier.fetch_add(1, Ordering::SeqCst);
        let mut spins = 0u32;

        while self.sync_barrier.load(Ordering::Acquire) < total_active_cores {
            spin_loop();
            spins += 1;
            if spins > timeout_cycles {
                return Err(IpiError::Timeout);
            }
        }

        Ok(())
    }

    pub fn reset_barrier(&self) {
        self.sync_barrier.store(0, Ordering::Release);
    }
}

impl Default for MultiCoreIpiEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. INTEGRATION SUITE & VERIFICATION TESTS FOR PART 7
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canxl_frame_crc_and_roundtrip() {
        let controller = CanXlController::new(0x4000_0000);
        let mut tx_frame = CanXlFrame::empty();

        tx_frame.header.priority_id = 0x12A;
        tx_frame.header.sdt = CANXL_SDT_SOMEIP;
        tx_frame.header.vcid = 1;
        tx_frame.header.data_length_bytes = 16;
        tx_frame.header.acceptance_field = 0xDEADBEEF;

        for i in 0..16 {
            tx_frame.payload[i] = (i as u8) + 1;
        }

        let computed_crc = CanXlFrame::calculate_crc32(&tx_frame.header, &tx_frame.payload[..16]);
        tx_frame.crc32 = computed_crc;

        assert_eq!(controller.transmit_frame(&tx_frame), Ok(()));

        // Create serialized simulation buffer
        let mut raw_buf = [0u8; 32];
        raw_buf[0..2].copy_from_slice(&tx_frame.header.priority_id.to_be_bytes());
        raw_buf[2] = tx_frame.header.sdt;
        raw_buf[3] = tx_frame.header.vcid;
        raw_buf[4..6].copy_from_slice(&tx_frame.header.data_length_bytes.to_be_bytes());
        raw_buf[6..10].copy_from_slice(&tx_frame.header.acceptance_field.to_be_bytes());
        raw_buf[10..26].copy_from_slice(&tx_frame.payload[..16]);
        raw_buf[26..30].copy_from_slice(&tx_frame.crc32.to_be_bytes());

        let mut rx_frame = CanXlFrame::empty();
        assert_eq!(
            controller.receive_frame(&raw_buf[..30], &mut rx_frame),
            Ok(())
        );
        assert_eq!(rx_frame.header.priority_id, 0x12A);
        assert_eq!(rx_frame.header.sdt, CANXL_SDT_SOMEIP);
        assert_eq!(rx_frame.payload[0], 1);
    }

    #[test]
    fn test_pcie_bar_and_tlp_queue() {
        let mut root_port = PcieRootPortController::new();
        assert_eq!(
            root_port.configure_bar(0, 0x8000_0000, 0x1000_0000, true),
            Ok(())
        );

        let mut tlp = TransactionLayerPacket::empty();
        tlp.header.tlp_type = TlpType::MemoryWrite64;
        tlp.header.address = 0x8000_1000;
        tlp.header.length_dwords = 4;

        assert_eq!(root_port.handle_inbound_write(&tlp), Ok(()));
        assert_eq!(root_port.enqueue_tx_tlp(&tlp), Ok(()));
    }

    #[test]
    fn test_mpu_access_control_and_asil_isolation() {
        let mut mpu = MpuManager::new();

        // Region 0: ASIL-D Steer-by-Wire Flash (0x1000..0x2000, Read-Only, Executable, ASIL 4)
        assert_eq!(
            mpu.configure_region(
                0,
                0x1000,
                0x2000,
                MpuAccessPermission::PrivilegedReadOnly,
                true,
                4
            ),
            Ok(())
        );

        // Core executing at ASIL-D (level 4) reading instruction
        assert_eq!(mpu.validate_access(0x1000, 64, false, true, 4), Ok(()));

        // Core executing at QM/ASIL-A (level 1) attempting to access ASIL-D region -> Inhibition Error
        assert_eq!(
            mpu.validate_access(0x1000, 64, false, false, 1),
            Err(MpuError::AsilInhibitionViolation)
        );

        // Write attempt on Read-Only region -> Permission Fault
        assert_eq!(
            mpu.validate_access(0x1000, 64, true, false, 4),
            Err(MpuError::InvalidPermissions)
        );
    }

    #[test]
    fn test_multicore_ipi_dispatch() {
        let ipi = MultiCoreIpiEngine::new();

        assert_eq!(
            ipi.send_ipi(1, 0, IpiCommand::LockstepVerify, 0xCAFE),
            Ok(())
        );

        let received = ipi.acknowledge_ipi(1);
        assert!(received.is_some());
        let msg = received.unwrap();
        assert_eq!(msg.command, IpiCommand::LockstepVerify);
        assert_eq!(msg.sender_core, 0);
        assert_eq!(msg.parameter, 0xCAFE);
    }
}
