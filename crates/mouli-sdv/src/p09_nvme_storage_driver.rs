//! Part 9: NVME STORAGE DRIVER, EDR BLACKBOX, ASIL-D CRASHDUMP & CRYPTO ACCELERATOR

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 9 - NVME STORAGE DRIVER, EDR BLACKBOX, ASIL-D CRASHDUMP & CRYPTO ACCELERATOR
// CRITICALITY: ISO 26262 ASIL-D / UN ECE R160 (EDR) / NIST SP 800-90A
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Allocation, Cacheline Aligned Ring Queues
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS: NVME PROTOCOL, EDR BUFFERS & SHA-256 CONFIGURATION
// =================================================================================================

/// NVMe Submission/Completion Queue Depth
pub const NVME_QUEUE_DEPTH: usize = 64;

/// Standard NVMe LBA block size (4096 bytes)
pub const NVME_BLOCK_SIZE: usize = 4096;

/// UN ECE R160 Event Data Recorder (EDR) recording capacity:
/// 500 samples @ 100Hz = 5 seconds of high-fidelity pre-crash dynamics
pub const EDR_BUFFER_SAMPLES: usize = 500;

/// Post-crash recording window: 100 samples @ 100Hz = 1 second
pub const EDR_POST_CRASH_SAMPLES: usize = 100;

/// Crashdump Core Dump Maximum Frames
pub const CRASHDUMP_MAX_CPU_REGISTERS: usize = 32;
pub const CRASHDUMP_STACK_SNIPPET_BYTES: usize = 512;

/// SHA-256 Constants
pub const SHA256_DIGEST_LENGTH: usize = 32;
pub const SHA256_BLOCK_SIZE: usize = 64;

// =================================================================================================
// 2. ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    Ok = 0,
    QueueFull = 1,
    QueueEmpty = 2,
    CompletionTimeout = 3,
    CommandAborted = 4,
    InvalidNamespace = 5,
    DmaAlignmentFault = 6,
    ControllerFatalStatus = 7,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdrError {
    Ok = 0,
    MemoryLocked = 1,
    SensorIncoherent = 2,
    StorageCommitFailed = 3,
    InvalidTriggerSequence = 4,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashTriggerType {
    None = 0,
    AirbagDeployment = 1,
    LongitudinalDecelExceeded = 2, // Deceleration > 2.0g
    LateralDecelExceeded = 3,      // Lateral Decel > 1.8g
    RolloverThresholdExceeded = 4, // Roll Angle > 45 degrees
    ManualEmergencyEcall = 5,
    AsilDWatchdogPanic = 6,
}

// =================================================================================================
// 3. HARDWARE-ALIGNED NVME 2.0 PROTOCOL ENGINE
// =================================================================================================

/// Standard 64-byte NVMe Submission Queue Entry (SQE)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvmeSubmissionQueueEntry {
    pub cdw0: u32, // Opcode, FUSE, CID
    pub nsid: u32, // Namespace Identifier
    pub reserved0: u64,
    pub mptr: u64,  // Metadata Pointer
    pub prp1: u64,  // Physical Region Page 1 (DMA Address)
    pub prp2: u64,  // Physical Region Page 2 (DMA Address)
    pub cdw10: u32, // Starting LBA (Lower 32-bits)
    pub cdw11: u32, // Starting LBA (Upper 32-bits)
    pub cdw12: u32, // Number of Logical Blocks (0-based)
    pub cdw13: u32, // Dataset Management / Storage Flags
    pub cdw14: u32,
    pub cdw15: u32,
}

impl NvmeSubmissionQueueEntry {
    pub const fn empty() -> Self {
        Self {
            cdw0: 0,
            nsid: 0,
            reserved0: 0,
            mptr: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    pub fn make_write(
        cid: u16,
        nsid: u32,
        start_lba: u64,
        block_count: u16,
        dma_addr: u64,
    ) -> Self {
        let opcode = 0x01u32; // NVMe NVM Write Opcode
        let cdw0 = opcode | ((cid as u32) << 16);
        Self {
            cdw0,
            nsid,
            reserved0: 0,
            mptr: 0,
            prp1: dma_addr,
            prp2: 0,
            cdw10: (start_lba & 0xFFFF_FFFF) as u32,
            cdw11: (start_lba >> 32) as u32,
            cdw12: (block_count.saturating_sub(1)) as u32,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    pub fn make_read(cid: u16, nsid: u32, start_lba: u64, block_count: u16, dma_addr: u64) -> Self {
        let opcode = 0x02u32; // NVMe NVM Read Opcode
        let cdw0 = opcode | ((cid as u32) << 16);
        Self {
            cdw0,
            nsid,
            reserved0: 0,
            mptr: 0,
            prp1: dma_addr,
            prp2: 0,
            cdw10: (start_lba & 0xFFFF_FFFF) as u32,
            cdw11: (start_lba >> 32) as u32,
            cdw12: (block_count.saturating_sub(1)) as u32,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    pub fn make_flush(cid: u16, nsid: u32) -> Self {
        let opcode = 0x00u32; // NVMe NVM Flush Opcode
        let cdw0 = opcode | ((cid as u32) << 16);
        Self {
            cdw0,
            nsid,
            reserved0: 0,
            mptr: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }
}

/// Standard 16-byte NVMe Completion Queue Entry (CQE)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvmeCompletionQueueEntry {
    pub command_specific: u32,
    pub reserved: u32,
    pub sq_head: u16,
    pub sq_id: u16,
    pub command_id: u16,
    pub status: u16, // Phase Tag (Bit 0) & Status Code
}

impl NvmeCompletionQueueEntry {
    pub const fn empty() -> Self {
        Self {
            command_specific: 0,
            reserved: 0,
            sq_head: 0,
            sq_id: 0,
            command_id: 0,
            status: 0,
        }
    }

    #[inline(always)]
    pub fn is_successful(&self) -> bool {
        // Status code is in bits 1..15; 0 indicates success
        (self.status >> 1) == 0
    }

    #[inline(always)]
    pub fn phase_tag(&self) -> bool {
        (self.status & 0x0001) != 0
    }
}

/// Cacheline-Aligned NVMe I/O Controller Interface
#[repr(C, align(64))]
pub struct NvmeIoController {
    sq_entries: UnsafeCell<[NvmeSubmissionQueueEntry; NVME_QUEUE_DEPTH]>,
    cq_entries: UnsafeCell<[NvmeCompletionQueueEntry; NVME_QUEUE_DEPTH]>,
    sq_tail: AtomicU16,
    cq_head: AtomicU16,
    cq_phase: AtomicBool,
    cid_counter: AtomicU16,
    doorbell_sq_reg: AtomicU32,
    doorbell_cq_reg: AtomicU32,
}

// `NvmeIoController` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl NvmeIoController {
    pub const fn new() -> Self {
        Self {
            sq_entries: UnsafeCell::new([NvmeSubmissionQueueEntry::empty(); NVME_QUEUE_DEPTH]),
            cq_entries: UnsafeCell::new([NvmeCompletionQueueEntry::empty(); NVME_QUEUE_DEPTH]),
            sq_tail: AtomicU16::new(0),
            cq_head: AtomicU16::new(0),
            cq_phase: AtomicBool::new(true), // Initial expected hardware phase tag
            cid_counter: AtomicU16::new(1),
            doorbell_sq_reg: AtomicU32::new(0),
            doorbell_cq_reg: AtomicU32::new(0),
        }
    }

    /// Submits a write command to the NVMe Submission Queue
    pub fn submit_write(
        &self,
        nsid: u32,
        start_lba: u64,
        block_count: u16,
        dma_addr: u64,
    ) -> Result<u16, StorageError> {
        if !dma_addr.is_multiple_of(64) {
            return Err(StorageError::DmaAlignmentFault);
        }

        let cid = self.cid_counter.fetch_add(1, Ordering::Relaxed);
        let sqe = NvmeSubmissionQueueEntry::make_write(cid, nsid, start_lba, block_count, dma_addr);

        let current_tail = self.sq_tail.load(Ordering::Relaxed) as usize;
        let next_tail = (current_tail + 1) % NVME_QUEUE_DEPTH;

        let entries = unsafe { &mut *self.sq_entries.get() };
        entries[current_tail] = sqe;

        self.sq_tail.store(next_tail as u16, Ordering::Release);
        // Trigger MMIO Doorbell write
        self.doorbell_sq_reg
            .store(next_tail as u32, Ordering::SeqCst);

        Ok(cid)
    }

    /// Polls the completion queue for a specific command identifier
    pub fn poll_completion(&self, expected_cid: u16, max_spins: u32) -> Result<(), StorageError> {
        let mut spins = 0u32;
        let cq = unsafe { &*self.cq_entries.get() };

        loop {
            let head = self.cq_head.load(Ordering::Relaxed) as usize;
            let cqe = &cq[head];
            let expected_phase = self.cq_phase.load(Ordering::Relaxed);

            // Check if the entry phase matches current cycle
            if cqe.phase_tag() == expected_phase && cqe.command_id == expected_cid {
                // Advance CQ Head
                let next_head = (head + 1) % NVME_QUEUE_DEPTH;
                if next_head == 0 {
                    // Roll phase
                    self.cq_phase.store(!expected_phase, Ordering::Relaxed);
                }
                self.cq_head.store(next_head as u16, Ordering::Release);
                self.doorbell_cq_reg
                    .store(next_head as u32, Ordering::SeqCst);

                return if cqe.is_successful() {
                    Ok(())
                } else {
                    Err(StorageError::CommandAborted)
                };
            }

            spins += 1;
            if spins >= max_spins {
                return Err(StorageError::CompletionTimeout);
            }
            spin_loop();
        }
    }
}

impl Default for NvmeIoController {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 4. UN ECE R160 EVENT DATA RECORDER (AUTOMOTIVE BLACKBOX)
// =================================================================================================

/// Vehicle telemetry record captured every 10ms (100Hz)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdrSampleRecord {
    pub timestamp_us: u64,
    pub vehicle_speed_kph: f32,
    pub accel_longitudinal_g: f32,
    pub accel_lateral_g: f32,
    pub yaw_rate_deg_per_s: f32,
    pub steering_wheel_angle_deg: f32,
    pub service_brake_pressure_kpa: f32,
    pub accelerator_pedal_pct: f32,
    pub driver_seatbelt_fastened: bool,
    pub passenger_seatbelt_fastened: bool,
    pub abs_active: bool,
    pub esc_active: bool,
    pub aeb_active: bool,
}

impl EdrSampleRecord {
    pub const fn empty() -> Self {
        Self {
            timestamp_us: 0,
            vehicle_speed_kph: 0.0,
            accel_longitudinal_g: 0.0,
            accel_lateral_g: 0.0,
            yaw_rate_deg_per_s: 0.0,
            steering_wheel_angle_deg: 0.0,
            service_brake_pressure_kpa: 0.0,
            accelerator_pedal_pct: 0.0,
            driver_seatbelt_fastened: false,
            passenger_seatbelt_fastened: false,
            abs_active: false,
            esc_active: false,
            aeb_active: false,
        }
    }
}

/// UN ECE R160 Crash Record Header
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EdrCrashSummary {
    pub trigger_timestamp_us: u64,
    pub trigger_reason: CrashTriggerType,
    pub delta_v_longitudinal_kph: f32,
    pub delta_v_lateral_kph: f32,
    pub max_deceleration_g: f32,
    pub total_samples_recorded: u32,
    pub is_memory_locked: bool,
}

/// Event Data Recorder Engine with Pre/Post Crash Freeze Isolation
pub struct EventDataRecorder {
    circular_buffer: UnsafeCell<[EdrSampleRecord; EDR_BUFFER_SAMPLES]>,
    write_head: AtomicU16,
    trigger_state: AtomicU8, // CrashTriggerType
    post_crash_counter: AtomicU16,
    is_locked: AtomicBool,
    summary: UnsafeCell<EdrCrashSummary>,
}

// `EventDataRecorder` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl EventDataRecorder {
    pub const fn new() -> Self {
        Self {
            circular_buffer: UnsafeCell::new([EdrSampleRecord::empty(); EDR_BUFFER_SAMPLES]),
            write_head: AtomicU16::new(0),
            trigger_state: AtomicU8::new(CrashTriggerType::None as u8),
            post_crash_counter: AtomicU16::new(0),
            is_locked: AtomicBool::new(false),
            summary: UnsafeCell::new(EdrCrashSummary {
                trigger_timestamp_us: 0,
                trigger_reason: CrashTriggerType::None,
                delta_v_longitudinal_kph: 0.0,
                delta_v_lateral_kph: 0.0,
                max_deceleration_g: 0.0,
                total_samples_recorded: 0,
                is_memory_locked: false,
            }),
        }
    }

    /// Ingests real-time vehicle telemetry frame at 100Hz
    pub fn push_sample(&self, record: &EdrSampleRecord) -> Result<(), EdrError> {
        if self.is_locked.load(Ordering::Acquire) {
            return Err(EdrError::MemoryLocked);
        }

        let head = self.write_head.load(Ordering::Relaxed) as usize;
        let buf = unsafe { &mut *self.circular_buffer.get() };
        buf[head] = *record;

        let next_head = (head + 1) % EDR_BUFFER_SAMPLES;
        self.write_head.store(next_head as u16, Ordering::Release);

        // Manage post-crash counting if an active trigger was latched
        let trig = self.trigger_state.load(Ordering::Relaxed);
        if trig != (CrashTriggerType::None as u8) {
            // `fetch_add` returns the count BEFORE this sample, so add one to include it: the
            // window is complete exactly when EDR_POST_CRASH_SAMPLES samples have followed the
            // trigger.
            let recorded = self
                .post_crash_counter
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            if recorded >= (EDR_POST_CRASH_SAMPLES as u16) {
                // Post-crash window completed: Permanently lock blackbox memory
                self.is_locked.store(true, Ordering::SeqCst);
                let summ = unsafe { &mut *self.summary.get() };
                summ.is_memory_locked = true;
            }
        }

        // Automatic kinematic trigger check: Deceleration threshold evaluation
        if trig == (CrashTriggerType::None as u8) {
            if record.accel_longitudinal_g < -2.0 {
                self.latch_trigger(
                    CrashTriggerType::LongitudinalDecelExceeded,
                    record.timestamp_us,
                );
            } else if record.accel_lateral_g.abs() > 1.8 {
                self.latch_trigger(CrashTriggerType::LateralDecelExceeded, record.timestamp_us);
            }
        }

        Ok(())
    }

    /// Latching crash trigger from airbag line or sensor arbitration
    pub fn latch_trigger(&self, reason: CrashTriggerType, timestamp_us: u64) {
        if self
            .trigger_state
            .compare_exchange(
                CrashTriggerType::None as u8,
                reason as u8,
                Ordering::SeqCst,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            let summ = unsafe { &mut *self.summary.get() };
            summ.trigger_timestamp_us = timestamp_us;
            summ.trigger_reason = reason;
        }
    }

    pub fn is_locked(&self) -> bool {
        self.is_locked.load(Ordering::Acquire)
    }

    pub fn get_summary(&self) -> EdrCrashSummary {
        unsafe { *self.summary.get() }
    }
}

impl Default for EventDataRecorder {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. ASIL-D HARDWARE CRASHDUMP & EMERGENCY PERSISTENCE
// =================================================================================================

/// Processor architectural context snapshot on CPU exception or watchdog fault
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpuRegisterSnapshot {
    pub faulting_pc: u64,
    pub stack_pointer: u64,
    pub processor_status: u64,
    pub general_purpose_regs: [u64; CRASHDUMP_MAX_CPU_REGISTERS],
}

impl CpuRegisterSnapshot {
    pub const fn empty() -> Self {
        Self {
            faulting_pc: 0,
            stack_pointer: 0,
            processor_status: 0,
            general_purpose_regs: [0u64; CRASHDUMP_MAX_CPU_REGISTERS],
        }
    }
}

/// Standalone Emergency Non-Volatile Flash Persistence Block
#[repr(C, align(64))]
pub struct CrashdumpPersistor {
    cpu_snapshot: UnsafeCell<CpuRegisterSnapshot>,
    stack_snippet: UnsafeCell<[u8; CRASHDUMP_STACK_SNIPPET_BYTES]>,
    is_committed: AtomicBool,
}

// `CrashdumpPersistor` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl CrashdumpPersistor {
    pub const fn new() -> Self {
        Self {
            cpu_snapshot: UnsafeCell::new(CpuRegisterSnapshot::empty()),
            stack_snippet: UnsafeCell::new([0u8; CRASHDUMP_STACK_SNIPPET_BYTES]),
            is_committed: AtomicBool::new(false),
        }
    }

    /// Captures register state during hard panic without invoking dynamic heap allocations
    /// Records the CPU registers and a snippet of the stack at the moment of a panic.
    ///
    /// # Safety
    /// If `raw_stack_ptr` is non-null it must be valid for reads of `CRASHDUMP_STACK_SNIPPET_BYTES`
    /// bytes.
    pub unsafe fn capture_panic_state(&self, regs: &CpuRegisterSnapshot, raw_stack_ptr: *const u8) {
        let snap = unsafe { &mut *self.cpu_snapshot.get() };
        *snap = *regs;

        let stack_dst = unsafe { &mut *self.stack_snippet.get() };
        if !raw_stack_ptr.is_null() {
            for i in 0..CRASHDUMP_STACK_SNIPPET_BYTES {
                stack_dst[i] = unsafe { *raw_stack_ptr.add(i) };
            }
        }

        self.is_committed.store(true, Ordering::SeqCst);
    }

    pub fn is_dump_ready(&self) -> bool {
        self.is_committed.load(Ordering::Acquire)
    }
}

impl Default for CrashdumpPersistor {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. CRYPTOGRAPHIC ACCELERATOR: CONSTANT-TIME BARE-METAL SHA-256
// =================================================================================================

/// Non-allocating SHA-256 State Engine (FIPS PUB 180-4)
pub struct Sha256Engine {
    state: [u32; 8],
    total_len_bytes: u64,
    buffer: [u8; SHA256_BLOCK_SIZE],
    buffer_len: usize,
}

impl Sha256Engine {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    pub const fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            total_len_bytes: 0,
            buffer: [0u8; SHA256_BLOCK_SIZE],
            buffer_len: 0,
        }
    }

    #[inline(always)]
    fn rotr(x: u32, n: u32) -> u32 {
        x.rotate_right(n)
    }

    fn transform_block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }

        for i in 16..64 {
            let s0 = Self::rotr(w[i - 15], 7) ^ Self::rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
            let s1 = Self::rotr(w[i - 2], 17) ^ Self::rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..64 {
            let s1 = Self::rotr(e, 6) ^ Self::rotr(e, 11) ^ Self::rotr(e, 25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(Self::K[i])
                .wrapping_add(w[i]);
            let s0 = Self::rotr(a, 2) ^ Self::rotr(a, 13) ^ Self::rotr(a, 22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }

    pub fn update(&mut self, data: &[u8]) {
        self.total_len_bytes += data.len() as u64;
        let mut offset = 0;

        while offset < data.len() {
            let to_copy = core::cmp::min(data.len() - offset, SHA256_BLOCK_SIZE - self.buffer_len);
            self.buffer[self.buffer_len..self.buffer_len + to_copy]
                .copy_from_slice(&data[offset..offset + to_copy]);
            self.buffer_len += to_copy;
            offset += to_copy;

            if self.buffer_len == SHA256_BLOCK_SIZE {
                let blk = self.buffer;
                self.transform_block(&blk);
                self.buffer_len = 0;
            }
        }
    }

    pub fn finalize(mut self, out_digest: &mut [u8; SHA256_DIGEST_LENGTH]) {
        let bit_len = self.total_len_bytes * 8;
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;

        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..SHA256_BLOCK_SIZE].fill(0);
            let blk = self.buffer;
            self.transform_block(&blk);
            self.buffer.fill(0);
        } else {
            self.buffer[self.buffer_len..56].fill(0);
        }

        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let blk = self.buffer;
        self.transform_block(&blk);

        for (i, &val) in self.state.iter().enumerate() {
            out_digest[i * 4..i * 4 + 4].copy_from_slice(&val.to_be_bytes());
        }
    }
}

impl Default for Sha256Engine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. VERIFICATION SUITE & UNIT TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nvme_submission_and_completion() {
        let controller = NvmeIoController::new();
        let cid = controller
            .submit_write(1, 0x1000, 8, 0x8000_0000)
            .expect("Write submit ok");
        assert_eq!(cid, 1);

        // Emulate hardware CQ completion
        let cq = unsafe { &mut *controller.cq_entries.get() };
        cq[0] = NvmeCompletionQueueEntry {
            command_specific: 0,
            reserved: 0,
            sq_head: 1,
            sq_id: 1,
            command_id: cid,
            status: 0x0001, // Success (Status=0) + Phase Tag bit 0 = 1
        };

        let poll_res = controller.poll_completion(cid, 100);
        assert_eq!(poll_res, Ok(()));
    }

    #[test]
    fn test_edr_automatic_deceleration_trigger() {
        let edr = EventDataRecorder::new();
        assert!(!edr.is_locked());

        let mut rec = EdrSampleRecord::empty();
        rec.vehicle_speed_kph = 100.0;
        rec.accel_longitudinal_g = -0.5;
        edr.push_sample(&rec).expect("Sample ok");

        // Deceleration exceeds -2.0g trigger threshold
        rec.accel_longitudinal_g = -2.5;
        edr.push_sample(&rec).expect("Trigger sample ok");

        let summary = edr.get_summary();
        assert_eq!(
            summary.trigger_reason,
            CrashTriggerType::LongitudinalDecelExceeded
        );

        // Feed post-crash window to reach lock state
        for _ in 0..EDR_POST_CRASH_SAMPLES {
            let _ = edr.push_sample(&rec);
        }

        assert!(edr.is_locked());
        assert_eq!(edr.push_sample(&rec), Err(EdrError::MemoryLocked));
    }

    #[test]
    fn test_sha256_known_vector() {
        // Known vector: SHA256("abc") = ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad
        let mut engine = Sha256Engine::new();
        engine.update(b"abc");
        let mut digest = [0u8; 32];
        engine.finalize(&mut digest);

        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];

        assert_eq!(digest, expected);
    }

    #[test]
    fn edr_locks_after_exactly_the_post_crash_window() {
        let edr = EventDataRecorder::new();
        let mut rec = EdrSampleRecord::empty();
        rec.accel_longitudinal_g = -3.0;
        edr.push_sample(&rec).expect("trigger sample");
        // The trigger sample itself does not count towards the window.
        for i in 0..(EDR_POST_CRASH_SAMPLES - 1) {
            assert_eq!(edr.push_sample(&rec), Ok(()), "sample {i}");
            assert!(!edr.is_locked(), "locked too early at sample {i}");
        }
        assert_eq!(edr.push_sample(&rec), Ok(()));
        assert!(edr.is_locked(), "must lock once the window is complete");
        assert!(edr.get_summary().is_memory_locked);
        assert_eq!(edr.push_sample(&rec), Err(EdrError::MemoryLocked));
    }

    #[test]
    fn edr_does_not_lock_without_a_trigger() {
        let edr = EventDataRecorder::new();
        let rec = EdrSampleRecord::empty();
        for _ in 0..(EDR_BUFFER_SAMPLES * 2) {
            assert_eq!(edr.push_sample(&rec), Ok(()));
        }
        assert!(!edr.is_locked());
    }
}
