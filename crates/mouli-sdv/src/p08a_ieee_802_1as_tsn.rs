//! Part 8a: IEEE 802.1AS TSN (GPTP/QBV), AUTOSAR DEM FAULT MEMORY & ARA::EXEC RUNTIME

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 8 - IEEE 802.1AS TSN (GPTP/QBV), AUTOSAR DEM FAULT MEMORY & ARA::EXEC RUNTIME
// CRITICALITY: ISO 26262 ASIL-D / IEEE 802.1AS-2020 / AUTOSAR Adaptive R22-11
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocation, Nanosecond-Precision Real-Time Clock
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS: TIME-SENSITIVE NETWORKING, FAULT MEMORY & EXECUTION CONTROL
// =================================================================================================

/// Maximum capacity of the Time-Aware Shaper (TAS) Gate Control List (GCL)
pub const MAX_TAS_GCL_ENTRIES: usize = 16;

/// Maximum number of Diagnostic Trouble Codes (DTCs) in non-volatile memory
pub const MAX_DEM_EVENT_ENTRIES: usize = 64;

/// Size of standard Diagnostic Freeze Frame snapshot buffer in bytes
pub const DEM_FREEZE_FRAME_SIZE: usize = 32;

/// Maximum number of managed execution processes under the Adaptive Runtime
pub const MAX_ADAPTIVE_PROCESSES: usize = 16;

/// PTP Domain number for automotive audio/video and control streams
pub const DEFAULT_PTP_DOMAIN: u8 = 0;

/// Nanoseconds per second conversion constant
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

// =================================================================================================
// 2. ERROR TAXONOMY & CLOCK STATE DEFINITIONS
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsnError {
    Ok = 0,
    ClockDriftExceeded = 1,
    SyncTimeout = 2,
    PeerDelayOutlier = 3,
    GateControlScheduleViolation = 4,
    InvalidPtpFrame = 5,
    PortHardwareNotReady = 6,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemError {
    Ok = 0,
    DtcNotFound = 1,
    FaultMemoryExhausted = 2,
    DebounceCounterOverflow = 3,
    FreezeFrameLocked = 4,
    IntegrityCheckFailed = 5,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtpPortRole {
    Disabled = 0,
    Master = 1,
    Slave = 2,
    Passive = 3,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExecutionState {
    Idle = 0,
    Starting = 1,
    Running = 2,
    Stopping = 3,
    Terminated = 4,
    QuarantineFault = 5,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtcStatusMask {
    TestFailed = 0x01,
    TestFailedThisOperationCycle = 0x02,
    PendingDtc = 0x04,
    ConfirmedDtc = 0x08,
    TestNotCompletedSinceLastClear = 0x10,
    TestFailedSinceLastClear = 0x20,
    TestNotCompletedThisOperationCycle = 0x40,
    WarningIndicatorRequested = 0x80,
}

// =================================================================================================
// 3. IEEE 802.1AS GENERALIZED PRECISION TIME PROTOCOL (gPTP) ENGINE
// =================================================================================================

#[derive(Clone, Copy, Default)]
pub struct PtpTimestamp {
    pub seconds: u64,
    pub nanoseconds: u32,
}

impl PtpTimestamp {
    pub const fn new(seconds: u64, nanoseconds: u32) -> Self {
        Self {
            seconds,
            nanoseconds,
        }
    }

    pub fn to_nanos(&self) -> u64 {
        self.seconds * NANOS_PER_SEC + (self.nanoseconds as u64)
    }

    pub fn from_nanos(total_nanos: u64) -> Self {
        Self {
            seconds: total_nanos / NANOS_PER_SEC,
            nanoseconds: (total_nanos % NANOS_PER_SEC) as u32,
        }
    }
}

pub struct PtpServoController {
    current_time_nanos: AtomicU64,
    drift_ppb: AtomicI64, // Parts per billion frequency adjustment
    last_sync_origin_nanos: AtomicU64,
    peer_delay_nanos: AtomicU64,
    port_role: AtomicU8,
    domain: u8,
}

impl PtpServoController {
    pub const fn new(role: PtpPortRole, domain: u8) -> Self {
        Self {
            current_time_nanos: AtomicU64::new(0),
            drift_ppb: AtomicI64::new(0),
            last_sync_origin_nanos: AtomicU64::new(0),
            peer_delay_nanos: AtomicU64::new(0),
            port_role: AtomicU8::new(role as u8),
            domain,
        }
    }

    pub fn get_wall_clock(&self) -> PtpTimestamp {
        PtpTimestamp::from_nanos(self.current_time_nanos.load(Ordering::Acquire))
    }

    pub fn advance_local_clock(&self, delta_nanos: u64) {
        let drift = self.drift_ppb.load(Ordering::Relaxed);
        let adjustment = ((delta_nanos as i64) * drift) / 1_000_000_000;
        let adjusted_delta = (delta_nanos as i64).saturating_add(adjustment);

        if adjusted_delta > 0 {
            self.current_time_nanos
                .fetch_add(adjusted_delta as u64, Ordering::Release);
        }
    }

    /// Computes Peer Delay exchange (t1: Request TX, t2: Request RX, t3: Response TX, t4: Response RX)
    pub fn update_peer_delay(
        &self,
        t1: PtpTimestamp,
        t2: PtpTimestamp,
        t3: PtpTimestamp,
        t4: PtpTimestamp,
    ) -> Result<u64, TsnError> {
        let t1_n = t1.to_nanos();
        let t2_n = t2.to_nanos();
        let t3_n = t3.to_nanos();
        let t4_n = t4.to_nanos();

        if t4_n < t1_n || t3_n < t2_n {
            return Err(TsnError::PeerDelayOutlier);
        }

        let propagation_delay = ((t4_n - t1_n).saturating_sub(t3_n - t2_n)) / 2;

        // Automotive threshold: physical link propagation should not exceed 100 microseconds
        if propagation_delay > 100_000 {
            return Err(TsnError::PeerDelayOutlier);
        }

        self.peer_delay_nanos
            .store(propagation_delay, Ordering::Release);
        Ok(propagation_delay)
    }

    /// Ingests Master Sync and Follow_Up timestamps to calculate offset and drift
    pub fn process_sync_followup(
        &self,
        master_tx: PtpTimestamp,
        slave_rx: PtpTimestamp,
    ) -> Result<(), TsnError> {
        if self.port_role.load(Ordering::Acquire) != PtpPortRole::Slave as u8 {
            return Ok(());
        }

        let p_delay = self.peer_delay_nanos.load(Ordering::Relaxed);
        let corrected_master = master_tx.to_nanos() + p_delay;
        let local_rx = slave_rx.to_nanos();

        let time_error = (corrected_master as i64) - (local_rx as i64);

        // Clamping PI control servo loop for clock drift
        let kp: i64 = 700; // Proportional gain
        let new_drift = (time_error * kp) / 1000;
        self.drift_ppb
            .store(new_drift.clamp(-500_000, 500_000), Ordering::Relaxed);

        // Immediate hard step if error is greater than 1 millisecond
        if time_error.abs() > 1_000_000 {
            self.current_time_nanos
                .store(corrected_master, Ordering::SeqCst);
        }

        self.last_sync_origin_nanos
            .store(corrected_master, Ordering::Release);
        Ok(())
    }
}

// =================================================================================================
// 4. IEEE 802.1Qbv TIME-AWARE SHAPER (TAS) GATE CONTROL SCHEDULE
// =================================================================================================

#[derive(Clone, Copy)]
pub struct GateControlEntry {
    pub gate_states: u8, // 8-bit mask representing 8 traffic class queues (1 = Open, 0 = Closed)
    pub time_interval_nanos: u32,
}

pub struct TimeAwareShaper {
    gcl: [GateControlEntry; MAX_TAS_GCL_ENTRIES],
    gcl_length: usize,
    cycle_time_nanos: u64,
    base_time_nanos: u64,
}

impl TimeAwareShaper {
    pub const fn new(base_time: u64) -> Self {
        const EMPTY_ENTRY: GateControlEntry = GateControlEntry {
            gate_states: 0xFF,
            time_interval_nanos: 0,
        };

        Self {
            gcl: [EMPTY_ENTRY; MAX_TAS_GCL_ENTRIES],
            gcl_length: 0,
            cycle_time_nanos: 0,
            base_time_nanos: base_time,
        }
    }

    pub fn configure_schedule(&mut self, entries: &[GateControlEntry]) -> Result<(), TsnError> {
        if entries.is_empty() || entries.len() > MAX_TAS_GCL_ENTRIES {
            return Err(TsnError::GateControlScheduleViolation);
        }

        let mut total_cycle = 0u64;
        for (i, entry) in entries.iter().enumerate() {
            self.gcl[i] = *entry;
            total_cycle += entry.time_interval_nanos as u64;
        }

        self.gcl_length = entries.len();
        self.cycle_time_nanos = total_cycle;
        Ok(())
    }

    /// Evaluates which traffic class queues are permitted to transmit at a specific timestamp
    pub fn get_active_gate_mask(&self, current_time_nanos: u64) -> u8 {
        if self.cycle_time_nanos == 0 || self.gcl_length == 0 {
            return 0xFF; // Pass-through when unconfigured
        }

        if current_time_nanos < self.base_time_nanos {
            return self.gcl[0].gate_states;
        }

        let time_in_cycle = (current_time_nanos - self.base_time_nanos) % self.cycle_time_nanos;
        let mut accumulated_time = 0u64;

        for i in 0..self.gcl_length {
            accumulated_time += self.gcl[i].time_interval_nanos as u64;
            if time_in_cycle < accumulated_time {
                return self.gcl[i].gate_states;
            }
        }

        0xFF
    }
}

// =================================================================================================
// 5. AUTOSAR DIAGNOSTIC EVENT MANAGER (DEM) & FAULT PERSISTENCE
// =================================================================================================

#[derive(Clone, Copy)]
pub struct DiagnosticEventRecord {
    pub dtc_id: u32,
    pub status_mask: u8,
    pub fault_counter: i16,
    pub occurrence_count: u16,
    pub freeze_frame: [u8; DEM_FREEZE_FRAME_SIZE],
}

impl DiagnosticEventRecord {
    pub const fn empty() -> Self {
        Self {
            dtc_id: 0,
            status_mask: DtcStatusMask::TestNotCompletedSinceLastClear as u8,
            fault_counter: 0,
            occurrence_count: 0,
            freeze_frame: [0u8; DEM_FREEZE_FRAME_SIZE],
        }
    }
}

pub struct DiagnosticEventManager {
    records: UnsafeCell<[DiagnosticEventRecord; MAX_DEM_EVENT_ENTRIES]>,
    record_count: AtomicU32,
    lock: AtomicBool,
}

// SAFETY: the interior-mutable `records` array is only accessed between `acquire` and `release`
// of the same spin lock (Acquire/Release), so accesses are mutually exclusive and ordered. A thread
// that panics while holding the lock leaves it locked.
unsafe impl Sync for DiagnosticEventManager {}

impl DiagnosticEventManager {
    pub const DEBOUNCE_FAIL_LIMIT: i16 = 10;
    pub const DEBOUNCE_PASS_LIMIT: i16 = -10;

    pub const fn new() -> Self {
        Self {
            records: UnsafeCell::new([DiagnosticEventRecord::empty(); MAX_DEM_EVENT_ENTRIES]),
            record_count: AtomicU32::new(0),
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

    /// Reports health of a Diagnostic Trouble Code with step-debouncing logic
    pub fn report_dtc_status(
        &self,
        dtc: u32,
        is_failing: bool,
        freeze_snapshot: &[u8],
    ) -> Result<u8, DemError> {
        self.acquire();
        // SAFETY: the lock taken by `acquire` is held until `release`, so this is the only
        // reference to `records`.
        let records = unsafe { &mut *self.records.get() };
        let count = self.record_count.load(Ordering::Relaxed) as usize;

        let mut target_idx = None;
        for i in 0..count {
            if records[i].dtc_id == dtc {
                target_idx = Some(i);
                break;
            }
        }

        let idx = match target_idx {
            Some(i) => i,
            None => {
                if count >= MAX_DEM_EVENT_ENTRIES {
                    self.release();
                    return Err(DemError::FaultMemoryExhausted);
                }
                records[count].dtc_id = dtc;
                self.record_count
                    .store((count + 1) as u32, Ordering::Relaxed);
                count
            }
        };

        let rec = &mut records[idx];

        if is_failing {
            rec.fault_counter = rec.fault_counter.saturating_add(1);
            if rec.fault_counter >= Self::DEBOUNCE_FAIL_LIMIT {
                rec.status_mask |= DtcStatusMask::TestFailed as u8;
                rec.status_mask |= DtcStatusMask::ConfirmedDtc as u8;
                rec.occurrence_count = rec.occurrence_count.saturating_add(1);

                // Capture freeze frame
                let len = freeze_snapshot.len().min(DEM_FREEZE_FRAME_SIZE);
                rec.freeze_frame[..len].copy_from_slice(&freeze_snapshot[..len]);
            }
        } else {
            rec.fault_counter = rec.fault_counter.saturating_sub(1);
            if rec.fault_counter <= Self::DEBOUNCE_PASS_LIMIT {
                rec.status_mask &= !(DtcStatusMask::TestFailed as u8);
            }
        }

        rec.status_mask &= !(DtcStatusMask::TestNotCompletedSinceLastClear as u8);
        let final_mask = rec.status_mask;
        self.release();

        Ok(final_mask)
    }

    pub fn clear_dtc(&self, dtc: u32) -> Result<(), DemError> {
        self.acquire();
        // SAFETY: the lock taken by `acquire` is held until `release`, so this is the only
        // reference to `records`.
        let records = unsafe { &mut *self.records.get() };
        let count = self.record_count.load(Ordering::Relaxed) as usize;

        for i in 0..count {
            if records[i].dtc_id == dtc {
                records[i].status_mask = DtcStatusMask::TestNotCompletedSinceLastClear as u8;
                records[i].fault_counter = 0;
                records[i].freeze_frame.fill(0);
                self.release();
                return Ok(());
            }
        }

        self.release();
        Err(DemError::DtcNotFound)
    }
}

impl Default for DiagnosticEventManager {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. AUTOSAR ADAPTIVE RUNTIME FOR APPLICATIONS (`ara::exec` ENGINE)
// =================================================================================================

#[derive(Clone, Copy)]
pub struct ProcessManifest {
    pub process_id: u16,
    pub max_runtime_budget_micros: u32,
    pub priority: u8,
    pub asil_level: u8, // 0 = QM, 1 = ASIL-A, ..., 4 = ASIL-D
}

pub struct AdaptiveProcessSlot {
    pub manifest: ProcessManifest,
    pub state: AtomicU8,
    pub cycle_time_elapsed_micros: AtomicU32,
    pub execution_budget_exceeded: AtomicBool,
}

pub struct ExecutionManager {
    processes: [AdaptiveProcessSlot; MAX_ADAPTIVE_PROCESSES],
    active_process_count: AtomicU32,
    function_group_state: AtomicU8,
}

impl ExecutionManager {
    pub const fn new() -> Self {
        const INIT_SLOT: AdaptiveProcessSlot = AdaptiveProcessSlot {
            manifest: ProcessManifest {
                process_id: 0,
                max_runtime_budget_micros: 0,
                priority: 0,
                asil_level: 0,
            },
            state: AtomicU8::new(ProcessExecutionState::Idle as u8),
            cycle_time_elapsed_micros: AtomicU32::new(0),
            execution_budget_exceeded: AtomicBool::new(false),
        };

        Self {
            processes: [INIT_SLOT; MAX_ADAPTIVE_PROCESSES],
            active_process_count: AtomicU32::new(0),
            function_group_state: AtomicU8::new(0), // 0 = Off, 1 = Verify, 2 = Running
        }
    }

    pub fn register_process(&mut self, manifest: ProcessManifest) -> Result<(), ()> {
        let count = self.active_process_count.load(Ordering::Relaxed) as usize;
        if count >= MAX_ADAPTIVE_PROCESSES {
            return Err(());
        }

        self.processes[count].manifest = manifest;
        self.processes[count]
            .state
            .store(ProcessExecutionState::Idle as u8, Ordering::Relaxed);
        self.active_process_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn set_process_state(&self, process_id: u16, state: ProcessExecutionState) {
        let count = self.active_process_count.load(Ordering::Relaxed) as usize;
        for i in 0..count {
            if self.processes[i].manifest.process_id == process_id {
                self.processes[i]
                    .state
                    .store(state as u8, Ordering::Release);
                break;
            }
        }
    }

    /// Evaluates runtime budget violations across execution slots
    pub fn step_execution_budget_monitor(&self, delta_micros: u32) {
        let count = self.active_process_count.load(Ordering::Relaxed) as usize;
        for i in 0..count {
            let p = &self.processes[i];
            if p.state.load(Ordering::Acquire) == ProcessExecutionState::Running as u8 {
                // `fetch_add` returns the PREVIOUS total, which would flag an overrun a whole step
                // late, so take the new total instead. Saturating, so the counter can never wrap
                // around and hide an overrun.
                let mut current = p.cycle_time_elapsed_micros.load(Ordering::Relaxed);
                let elapsed = loop {
                    let updated = current.saturating_add(delta_micros);
                    match p.cycle_time_elapsed_micros.compare_exchange_weak(
                        current,
                        updated,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break updated,
                        Err(actual) => current = actual,
                    }
                };
                if elapsed > p.manifest.max_runtime_budget_micros {
                    p.execution_budget_exceeded.store(true, Ordering::SeqCst);
                    p.state.store(
                        ProcessExecutionState::QuarantineFault as u8,
                        Ordering::Release,
                    );
                }
            }
        }
    }

    pub fn reset_cycle_timings(&self) {
        let count = self.active_process_count.load(Ordering::Relaxed) as usize;
        for i in 0..count {
            self.processes[i]
                .cycle_time_elapsed_micros
                .store(0, Ordering::Relaxed);
        }
    }
}

impl Default for ExecutionManager {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. PART 8 INTEGRATION SUITE & VERIFICATION TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ptp_timestamp_conversion() {
        let ts = PtpTimestamp::new(5, 500_000_000);
        assert_eq!(ts.to_nanos(), 5_500_000_000);
        let reconverted = PtpTimestamp::from_nanos(5_500_000_000);
        assert_eq!(reconverted.seconds, 5);
        assert_eq!(reconverted.nanoseconds, 500_000_000);
    }

    #[test]
    fn test_tas_gate_schedule_cycle() {
        let mut tas = TimeAwareShaper::new(1000);
        let entries = [
            GateControlEntry {
                gate_states: 0b0000_0001,
                time_interval_nanos: 100,
            }, // Priority 0 open
            GateControlEntry {
                gate_states: 0b1000_0000,
                time_interval_nanos: 200,
            }, // Priority 7 open
        ];
        assert!(tas.configure_schedule(&entries).is_ok());

        assert_eq!(tas.get_active_gate_mask(1050), 0b0000_0001);
        assert_eq!(tas.get_active_gate_mask(1150), 0b1000_0000);
        assert_eq!(tas.get_active_gate_mask(1350), 0b0000_0001); // Next cycle
    }

    #[test]
    fn test_dem_debouncing_and_confirmation() {
        let dem = DiagnosticEventManager::new();
        let dtc = 0xC10001; // Example Chassis Steer-by-Wire fault
        let dummy_snapshot = [0xAA; 16];

        for _ in 0..DiagnosticEventManager::DEBOUNCE_FAIL_LIMIT - 1 {
            let mask = dem.report_dtc_status(dtc, true, &dummy_snapshot).unwrap();
            assert_eq!(mask & DtcStatusMask::ConfirmedDtc as u8, 0);
        }

        let confirmed_mask = dem.report_dtc_status(dtc, true, &dummy_snapshot).unwrap();
        assert_ne!(confirmed_mask & DtcStatusMask::ConfirmedDtc as u8, 0);
    }

    #[test]
    fn test_adaptive_execution_budget_quarantine() {
        let mut exec = ExecutionManager::new();
        let manifest = ProcessManifest {
            process_id: 101,
            max_runtime_budget_micros: 50,
            priority: 10,
            asil_level: 4,
        };

        assert!(exec.register_process(manifest).is_ok());
        exec.set_process_state(101, ProcessExecutionState::Running);

        exec.step_execution_budget_monitor(40);
        assert_eq!(
            exec.processes[0].state.load(Ordering::Acquire),
            ProcessExecutionState::Running as u8
        );

        exec.step_execution_budget_monitor(20);
        assert_eq!(
            exec.processes[0].state.load(Ordering::Acquire),
            ProcessExecutionState::QuarantineFault as u8
        );
    }

    #[test]
    fn budget_overrun_is_flagged_on_the_step_that_causes_it() {
        let mut exec = ExecutionManager::new();
        let manifest = ProcessManifest {
            process_id: 7,
            max_runtime_budget_micros: 50,
            priority: 1,
            asil_level: 1,
        };
        assert!(exec.register_process(manifest).is_ok());
        exec.set_process_state(7, ProcessExecutionState::Running);
        // Exactly at the budget is allowed; one microsecond over is not.
        exec.step_execution_budget_monitor(50);
        assert_eq!(
            exec.processes[0].state.load(Ordering::Acquire),
            ProcessExecutionState::Running as u8
        );
        exec.step_execution_budget_monitor(1);
        assert_eq!(
            exec.processes[0].state.load(Ordering::Acquire),
            ProcessExecutionState::QuarantineFault as u8
        );
        assert!(exec.processes[0]
            .execution_budget_exceeded
            .load(Ordering::SeqCst));
    }

    #[test]
    fn a_huge_step_cannot_wrap_the_counter_and_hide_an_overrun() {
        let mut exec = ExecutionManager::new();
        let manifest = ProcessManifest {
            process_id: 8,
            max_runtime_budget_micros: 50,
            priority: 1,
            asil_level: 1,
        };
        assert!(exec.register_process(manifest).is_ok());
        exec.set_process_state(8, ProcessExecutionState::Running);
        exec.step_execution_budget_monitor(u32::MAX);
        assert_eq!(
            exec.processes[0].state.load(Ordering::Acquire),
            ProcessExecutionState::QuarantineFault as u8
        );
    }

    #[test]
    fn resetting_the_cycle_gives_a_fresh_budget() {
        let mut exec = ExecutionManager::new();
        let manifest = ProcessManifest {
            process_id: 9,
            max_runtime_budget_micros: 50,
            priority: 1,
            asil_level: 1,
        };
        assert!(exec.register_process(manifest).is_ok());
        exec.set_process_state(9, ProcessExecutionState::Running);
        exec.step_execution_budget_monitor(40);
        exec.reset_cycle_timings();
        exec.step_execution_budget_monitor(40);
        assert_eq!(
            exec.processes[0].state.load(Ordering::Acquire),
            ProcessExecutionState::Running as u8
        );
    }
}
