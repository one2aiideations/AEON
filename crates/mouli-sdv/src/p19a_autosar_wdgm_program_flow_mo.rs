//! Part 19a: AUTOSAR WDGM PROGRAM FLOW MONITORING, ISO 15765-2 ISO-TP & E2E PROTECTION

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 19 - AUTOSAR WDGM PROGRAM FLOW MONITORING, ISO 15765-2 ISO-TP & E2E PROTECTION
// CRITICALITY: ISO 26262 ASIL-D / AUTOSAR Classic & Adaptive Safety Specifications
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Heap Allocations, Atomic State Tracking
// =================================================================================================

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS & SYSTEM CONFIGURATION
// =================================================================================================

/// Maximum Supervised Entities (SEs) tracked by the Watchdog Manager
pub const WDGM_MAX_SUPERVISED_ENTITIES: usize = 16;

/// Maximum Checkpoints allowed per Supervised Entity
pub const WDGM_MAX_CHECKPOINTS: usize = 8;

/// Maximum allowed program flow transitions per Supervised Entity graph
pub const WDGM_MAX_GRAPH_TRANSITIONS: usize = 16;

/// ISO-TP Maximum PDU buffer size (4095 bytes for classic ISO 15765-2)
pub const ISOTP_MAX_PAYLOAD_SIZE: usize = 4095;

/// ISO-TP CAN Frame Data Field Length (8 bytes classic, up to 64 bytes CAN FD)
pub const ISOTP_MAX_CAN_FRAME_LEN: usize = 64;

/// AUTOSAR E2E Profile Data ID Configuration
pub const E2E_PROFILE01_DATA_ID: u16 = 0x5A5A;
pub const E2E_PROFILE04_DATA_ID: u32 = 0x1234_5678;

// =================================================================================================
// 2. ERROR DEFINITIONS
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WdgmError {
    Ok = 0,
    InvalidEntityId = 1,
    InvalidCheckpointId = 2,
    AliveSupervisionFailed = 3,
    DeadlineSupervisionMinViolated = 4,
    DeadlineSupervisionMaxViolated = 5,
    ProgramFlowTransitionInvalid = 6,
    GlobalSupervisionStatusDeactivated = 7,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoTpError {
    Ok = 0,
    BufferOverflow = 1,
    InvalidPduType = 2,
    SequenceNumberMismatch = 3,
    FlowControlWaitTimeout = 4,
    FlowControlOverflow = 5,
    TransmissionInProgress = 6,
    PayloadTooLarge = 7,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum E2eStatus {
    Ok = 0,
    InvalidCrc = 1,
    CounterMismatch = 2,
    RepeatedMessage = 3,
    DataIdMismatch = 4,
    FrameIncomplete = 5,
}

// =================================================================================================
// 3. AUTOSAR WATCHDOG MANAGER (WDGM) ASIL-D SUPERVISOR
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WdgmEntityStatus {
    Deactivated = 0,
    Ok = 1,
    Failed = 2,
    Expired = 3,
}

/// Permissible directed transition between Checkpoints in an execution thread
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowTransition {
    pub source_checkpoint_id: u8,
    pub target_checkpoint_id: u8,
}

/// Deadline parameters between two checkpoints
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineConstraint {
    pub start_checkpoint_id: u8,
    pub end_checkpoint_id: u8,
    pub min_cycles: u64,
    pub max_cycles: u64,
}

/// Supervised Entity Runtime State tracking Alive, Deadline, and Flow states
pub struct SupervisedEntity {
    pub id: u8,
    pub status: AtomicU8,
    pub expected_alive_indications: u32,
    pub alive_counter: AtomicU32,
    pub last_reached_checkpoint: AtomicU8,
    pub last_checkpoint_timestamp: AtomicU64,
    pub transitions: [Option<FlowTransition>; WDGM_MAX_GRAPH_TRANSITIONS],
    pub deadline: Option<DeadlineConstraint>,
}

impl SupervisedEntity {
    pub const fn new(
        id: u8,
        expected_alive: u32,
        transitions: [Option<FlowTransition>; WDGM_MAX_GRAPH_TRANSITIONS],
        deadline: Option<DeadlineConstraint>,
    ) -> Self {
        Self {
            id,
            status: AtomicU8::new(WdgmEntityStatus::Ok as u8),
            expected_alive_indications: expected_alive,
            alive_counter: AtomicU32::new(0),
            last_reached_checkpoint: AtomicU8::new(0xFF), // None reached
            last_checkpoint_timestamp: AtomicU64::new(0),
            transitions,
            deadline,
        }
    }

    /// Evaluates Program Flow and Deadline conditions when reaching a checkpoint
    pub fn reach_checkpoint(
        &self,
        checkpoint_id: u8,
        timestamp_cycles: u64,
    ) -> Result<(), WdgmError> {
        if self.status.load(Ordering::Relaxed) == (WdgmEntityStatus::Expired as u8) {
            return Err(WdgmError::GlobalSupervisionStatusDeactivated);
        }

        let previous_cp = self.last_reached_checkpoint.load(Ordering::Relaxed);

        // 1. Validate Program Flow Transition
        if previous_cp != 0xFF {
            let mut valid_transition = false;
            for t in self.transitions.iter().flatten() {
                if t.source_checkpoint_id == previous_cp && t.target_checkpoint_id == checkpoint_id
                {
                    valid_transition = true;
                    break;
                }
            }

            if !valid_transition {
                self.status
                    .store(WdgmEntityStatus::Expired as u8, Ordering::SeqCst);
                return Err(WdgmError::ProgramFlowTransitionInvalid);
            }
        }

        // 2. Validate Deadline Supervision
        if let Some(deadline) = self.deadline {
            if previous_cp == deadline.start_checkpoint_id
                && checkpoint_id == deadline.end_checkpoint_id
            {
                let start_time = self.last_checkpoint_timestamp.load(Ordering::Relaxed);
                let elapsed = timestamp_cycles.saturating_sub(start_time);

                if elapsed < deadline.min_cycles {
                    self.status
                        .store(WdgmEntityStatus::Failed as u8, Ordering::SeqCst);
                    return Err(WdgmError::DeadlineSupervisionMinViolated);
                }
                if elapsed > deadline.max_cycles {
                    self.status
                        .store(WdgmEntityStatus::Expired as u8, Ordering::SeqCst);
                    return Err(WdgmError::DeadlineSupervisionMaxViolated);
                }
            }
        }

        // Update state
        self.last_reached_checkpoint
            .store(checkpoint_id, Ordering::Release);
        self.last_checkpoint_timestamp
            .store(timestamp_cycles, Ordering::Release);
        self.alive_counter.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    /// Performs periodic alive-cycle supervision verification
    pub fn evaluate_alive_period(&self, allowed_margin: u32) -> Result<(), WdgmError> {
        let actual_alive = self.alive_counter.swap(0, Ordering::SeqCst);
        let min_expected = self
            .expected_alive_indications
            .saturating_sub(allowed_margin);
        let max_expected = self
            .expected_alive_indications
            .saturating_add(allowed_margin);

        if actual_alive < min_expected || actual_alive > max_expected {
            self.status
                .store(WdgmEntityStatus::Failed as u8, Ordering::SeqCst);
            return Err(WdgmError::AliveSupervisionFailed);
        }

        Ok(())
    }
}

/// Global ASIL-D Watchdog Manager Cluster
pub struct WatchdogManager {
    entities: [Option<SupervisedEntity>; WDGM_MAX_SUPERVISED_ENTITIES],
    failed_reference_counter: AtomicU16,
    is_watchdog_inhibited: AtomicBool,
}

impl WatchdogManager {
    pub const fn new(entities: [Option<SupervisedEntity>; WDGM_MAX_SUPERVISED_ENTITIES]) -> Self {
        Self {
            entities,
            failed_reference_counter: AtomicU16::new(0),
            is_watchdog_inhibited: AtomicBool::new(false),
        }
    }

    pub fn checkpoint_reached(
        &self,
        entity_id: usize,
        cp_id: u8,
        timestamp: u64,
    ) -> Result<(), WdgmError> {
        if entity_id >= WDGM_MAX_SUPERVISED_ENTITIES {
            return Err(WdgmError::InvalidEntityId);
        }

        if let Some(ref entity) = self.entities[entity_id] {
            let res = entity.reach_checkpoint(cp_id, timestamp);
            if res.is_err() {
                self.failed_reference_counter.fetch_add(1, Ordering::SeqCst);
                self.is_watchdog_inhibited.store(true, Ordering::SeqCst);
            }
            res
        } else {
            Err(WdgmError::InvalidEntityId)
        }
    }

    pub fn is_system_healthy(&self) -> bool {
        !self.is_watchdog_inhibited.load(Ordering::Acquire)
    }
}

// =================================================================================================
// 4. ISO 15765-2 (ISO-TP) AUTOMOTIVE TRANSPORT PROTOCOL LAYER
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoTpFrameType {
    SingleFrame = 0x00,
    FirstFrame = 0x10,
    ConsecutiveFrame = 0x20,
    FlowControl = 0x30,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowStatus {
    ContinueToSend = 0,
    Wait = 1,
    Overflow = 2,
}

/// ISO-TP Reception State Machine
pub struct IsoTpReceiver {
    buffer: UnsafeCell<[u8; ISOTP_MAX_PAYLOAD_SIZE]>,
    expected_len: AtomicU16,
    received_len: AtomicU16,
    next_sn: AtomicU8,
    block_size: AtomicU8,
    st_min_ms: AtomicU8,
    is_active: AtomicBool,
}

// `IsoTpReceiver` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl IsoTpReceiver {
    pub const fn new() -> Self {
        Self {
            buffer: UnsafeCell::new([0u8; ISOTP_MAX_PAYLOAD_SIZE]),
            expected_len: AtomicU16::new(0),
            received_len: AtomicU16::new(0),
            next_sn: AtomicU8::new(1),
            block_size: AtomicU8::new(8),
            st_min_ms: AtomicU8::new(10),
            is_active: AtomicBool::new(false),
        }
    }

    /// Ingests an incoming CAN/CAN-FD physical frame into the ISO-TP state machine
    pub fn process_can_frame(
        &self,
        frame: &[u8],
        frame_len: usize,
    ) -> Result<Option<usize>, IsoTpError> {
        if frame_len == 0 || frame.len() < frame_len {
            return Err(IsoTpError::InvalidPduType);
        }

        let pci = frame[0];
        let frame_type = pci & 0xF0;

        match frame_type {
            // Single Frame (SF)
            0x00 => {
                let sf_dl = (pci & 0x0F) as usize;
                if sf_dl == 0 || sf_dl > (frame_len - 1) {
                    return Err(IsoTpError::BufferOverflow);
                }
                // SAFETY: `IsoTpReceiver` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the reassembly buffer is alive.
                let buf = unsafe { &mut *self.buffer.get() };
                buf[..sf_dl].copy_from_slice(&frame[1..1 + sf_dl]);
                self.received_len.store(sf_dl as u16, Ordering::Release);
                self.expected_len.store(sf_dl as u16, Ordering::Release);
                self.is_active.store(false, Ordering::Release);
                Ok(Some(sf_dl))
            }

            // First Frame (FF)
            0x10 => {
                if frame_len < 8 {
                    return Err(IsoTpError::InvalidPduType);
                }
                let ff_dl = (((pci & 0x0F) as usize) << 8) | (frame[1] as usize);
                if ff_dl > ISOTP_MAX_PAYLOAD_SIZE {
                    return Err(IsoTpError::FlowControlOverflow);
                }

                let payload_in_ff = frame_len - 2;
                // SAFETY: `IsoTpReceiver` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the reassembly buffer is alive.
                let buf = unsafe { &mut *self.buffer.get() };
                buf[..payload_in_ff].copy_from_slice(&frame[2..2 + payload_in_ff]);

                self.expected_len.store(ff_dl as u16, Ordering::SeqCst);
                self.received_len
                    .store(payload_in_ff as u16, Ordering::SeqCst);
                self.next_sn.store(1, Ordering::SeqCst);
                self.is_active.store(true, Ordering::SeqCst);

                Ok(None) // Assembly ongoing
            }

            // Consecutive Frame (CF)
            0x20 => {
                if !self.is_active.load(Ordering::Acquire) {
                    return Err(IsoTpError::InvalidPduType);
                }

                let sn = pci & 0x0F;
                let expected_sn = self.next_sn.load(Ordering::Relaxed);
                if sn != expected_sn {
                    self.is_active.store(false, Ordering::SeqCst);
                    return Err(IsoTpError::SequenceNumberMismatch);
                }

                let current_len = self.received_len.load(Ordering::Relaxed) as usize;
                let total_len = self.expected_len.load(Ordering::Relaxed) as usize;
                let remaining = total_len.saturating_sub(current_len);
                let payload_bytes = core::cmp::min(frame_len - 1, remaining);

                // SAFETY: `IsoTpReceiver` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the reassembly buffer is alive.
                let buf = unsafe { &mut *self.buffer.get() };
                buf[current_len..current_len + payload_bytes]
                    .copy_from_slice(&frame[1..1 + payload_bytes]);

                let new_len = current_len + payload_bytes;
                self.received_len.store(new_len as u16, Ordering::SeqCst);
                self.next_sn
                    .store((expected_sn + 1) & 0x0F, Ordering::Relaxed);

                if new_len >= total_len {
                    self.is_active.store(false, Ordering::SeqCst);
                    Ok(Some(total_len))
                } else {
                    Ok(None)
                }
            }

            // Flow Control (FC)
            0x30 => Ok(None),

            _ => Err(IsoTpError::InvalidPduType),
        }
    }

    /// Generates Flow Control (FC) frame parameters for transmitter pacing
    pub fn build_flow_control_frame(&self, fs: FlowStatus, out_frame: &mut [u8; 8]) {
        out_frame[0] = 0x30 | (fs as u8);
        out_frame[1] = self.block_size.load(Ordering::Relaxed);
        out_frame[2] = self.st_min_ms.load(Ordering::Relaxed);
        out_frame[3..8].fill(0xAA); // CAN padding byte
    }

    pub fn get_payload<'a>(&self, out_buf: &'a mut [u8]) -> &'a [u8] {
        let len = self.received_len.load(Ordering::Acquire) as usize;
        // SAFETY: `IsoTpReceiver` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the reassembly buffer is alive.
        // It is only read here.
        let buf = unsafe { &*self.buffer.get() };
        let to_copy = core::cmp::min(len, out_buf.len());
        out_buf[..to_copy].copy_from_slice(&buf[..to_copy]);
        &out_buf[..to_copy]
    }
}

impl Default for IsoTpReceiver {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. AUTOSAR END-TO-END (E2E) SAFETY PROTECTION PROFILES
// =================================================================================================

/// CRC-8/SAE-J1850 (polynomial 0x1D, initial value 0xFF, final XOR 0xFF).
///
/// The lookup table is generated at compile time from the polynomial, not typed in by hand.
pub struct Crc8J1850;

impl Crc8J1850 {
    const TABLE: [u8; 256] = Self::build_table();

    const fn build_table() -> [u8; 256] {
        let mut table = [0u8; 256];
        let mut i = 0;
        while i < 256 {
            let mut crc = i as u8;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 0x80 != 0 {
                    (crc << 1) ^ 0x1D
                } else {
                    crc << 1
                };
                bit += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    }

    /// CRC of `chunks` taken in order, as if they were one contiguous buffer.
    pub fn compute(chunks: &[&[u8]]) -> u8 {
        let mut crc = 0xFFu8;
        for chunk in chunks {
            for b in *chunk {
                crc = Self::TABLE[usize::from(crc ^ b)];
            }
        }
        crc ^ 0xFF
    }
}

/// Smallest E2E Profile 01 frame: CRC byte, counter byte and at least one data byte.
pub const E2E_P01_MIN_LEN: usize = 3;

/// AUTOSAR E2E Profile 01: byte 0 is a CRC-8 over the data ID (low byte, then high byte) and
/// bytes 1.., and the low nibble of byte 1 is a 4-bit message counter.
pub struct E2eProfile01 {
    data_id: u16,
    last_counter: Option<u8>,
}

impl E2eProfile01 {
    /// A checker or protector for one data ID.
    pub const fn new(data_id: u16) -> Self {
        Self {
            data_id,
            last_counter: None,
        }
    }

    fn crc(&self, frame: &[u8]) -> u8 {
        let id = self.data_id.to_le_bytes();
        Crc8J1850::compute(&[&id, frame.get(1..).unwrap_or(&[])])
    }

    /// Writes the counter (low 4 bits of `counter`) and CRC into `frame`.
    /// Returns [`E2eStatus::FrameIncomplete`] when the frame is too short.
    pub fn protect(&self, frame: &mut [u8], counter: u8) -> E2eStatus {
        if frame.len() < E2E_P01_MIN_LEN {
            return E2eStatus::FrameIncomplete;
        }
        frame[1] = (frame[1] & 0xF0) | (counter & 0x0F);
        frame[0] = self.crc(frame);
        E2eStatus::Ok
    }

    /// Checks a received frame. A frame with a good CRC and the next counter value is `Ok`; the
    /// same counter again is `RepeatedMessage`; any other jump is `CounterMismatch`. The first
    /// valid frame is always accepted. Counter state only advances on a valid CRC.
    pub fn check(&mut self, frame: &[u8]) -> E2eStatus {
        if frame.len() < E2E_P01_MIN_LEN {
            return E2eStatus::FrameIncomplete;
        }
        if frame[0] != self.crc(frame) {
            return E2eStatus::InvalidCrc;
        }
        let counter = frame[1] & 0x0F;
        let status = match self.last_counter {
            None => E2eStatus::Ok,
            Some(prev) if counter == prev => E2eStatus::RepeatedMessage,
            Some(prev) if counter == (prev + 1) & 0x0F => E2eStatus::Ok,
            Some(_) => E2eStatus::CounterMismatch,
        };
        self.last_counter = Some(counter);
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_j1850_matches_the_published_check_value() {
        // Catalogue of parametrised CRC algorithms: CRC-8/SAE-J1850 check = 0x4B.
        assert_eq!(Crc8J1850::compute(&[b"123456789"]), 0x4B);
        assert_eq!(Crc8J1850::compute(&[b"1234", b"56789"]), 0x4B);
        assert_eq!(Crc8J1850::compute(&[]), 0x00);
    }

    #[test]
    fn generated_table_has_the_known_start_and_end() {
        assert_eq!(
            &Crc8J1850::TABLE[..8],
            &[0x00, 0x1D, 0x3A, 0x27, 0x74, 0x69, 0x4E, 0x53]
        );
        // Independently computed (bitwise CRC of byte 0xFF) = 0xC4.
        assert_eq!(Crc8J1850::TABLE[255], 0xC4);
    }

    #[test]
    fn protect_then_check_round_trips_and_counts() {
        let tx = E2eProfile01::new(E2E_PROFILE01_DATA_ID);
        let mut rx = E2eProfile01::new(E2E_PROFILE01_DATA_ID);
        for n in 0..40u8 {
            let mut frame = [0u8, 0xA0, 0x11, 0x22];
            assert_eq!(tx.protect(&mut frame, n), E2eStatus::Ok);
            assert_eq!(frame[1] >> 4, 0xA, "upper nibble untouched");
            assert_eq!(rx.check(&frame), E2eStatus::Ok, "frame {n}");
        }
    }

    #[test]
    fn detects_corruption_repeats_and_jumps() {
        let tx = E2eProfile01::new(7);
        let mut rx = E2eProfile01::new(7);
        let mk = |c: u8| {
            let mut f = [0u8, 0, 9, 9];
            tx.protect(&mut f, c);
            f
        };
        assert_eq!(rx.check(&mk(3)), E2eStatus::Ok);
        assert_eq!(rx.check(&mk(3)), E2eStatus::RepeatedMessage);
        assert_eq!(rx.check(&mk(5)), E2eStatus::CounterMismatch);
        assert_eq!(rx.check(&mk(6)), E2eStatus::Ok);
        let mut bad = mk(7);
        bad[2] ^= 1;
        assert_eq!(rx.check(&bad), E2eStatus::InvalidCrc);
        // A corrupted frame must not advance the counter.
        assert_eq!(rx.check(&mk(7)), E2eStatus::Ok);
        // Wrong data ID is caught by the CRC.
        let mut other = E2eProfile01::new(8);
        assert_eq!(other.check(&mk(1)), E2eStatus::InvalidCrc);
    }

    #[test]
    fn counter_wraps_from_15_to_0() {
        let tx = E2eProfile01::new(1);
        let mut rx = E2eProfile01::new(1);
        let mk = |c: u8| {
            let mut f = [0u8, 0, 1];
            tx.protect(&mut f, c);
            f
        };
        assert_eq!(rx.check(&mk(15)), E2eStatus::Ok);
        assert_eq!(rx.check(&mk(0)), E2eStatus::Ok);
    }

    #[test]
    fn short_frames_are_incomplete_not_panics() {
        let tx = E2eProfile01::new(1);
        let mut rx = E2eProfile01::new(1);
        for len in 0..E2E_P01_MIN_LEN {
            let mut f = [0u8; 4];
            assert_eq!(tx.protect(&mut f[..len], 0), E2eStatus::FrameIncomplete);
            assert_eq!(rx.check(&f[..len]), E2eStatus::FrameIncomplete);
        }
    }
}
