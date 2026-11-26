//! Part 1: BARE-METAL HAL, MEMORY MODEL, ASIL-D LOCKSTEP SUPERVISOR & KERNEL SCHEDULER

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 1 - BARE-METAL HAL, MEMORY MODEL, ASIL-D LOCKSTEP SUPERVISOR & KERNEL SCHEDULER
// CRITICALITY: ISO 26262 ASIL-D / IEC 61508 SIL-3 / AUTOSAR Adaptive Aligned
// TARGET ARCH: aarch64-unknown-none (Cortex-A78AE / Cortex-R82) & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Dynamic Allocation, Non-Panicking Static Allocators
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::MaybeUninit;
#[cfg(not(feature = "std"))]
use core::panic::PanicInfo;
use core::sync::atomic::{
    compiler_fence, fence, AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};

// =================================================================================================
// 1. HARDWARE TOPOLOGY, CACHELINE PADDING & FUNDAMENTAL ARCHITECTURAL CONSTANTS
// =================================================================================================

/// Cache-line dimension across contemporary automotive ARMv8-A/R and RISC-V compute cores.
/// Used to eliminate false sharing across SMP processing units.
pub const CACHELINE_ALIGN_BYTES: usize = 64;

/// Maximum number of symmetric execution cores managed under the monolith
pub const MAX_CPU_CORES: usize = 8;

/// Static task table capacity for the Time-Triggered Real-Time Scheduler
pub const MAX_SCHEDULER_TASKS: usize = 64;

/// Hardware watchdog maximum jitter allowance (in microseconds) before ASIL-D trip
pub const WATCHDOG_HARD_DEADLINE_MICROS: u64 = 10_000; // 10ms

/// Maximum memory size (in bytes) of the static kernel scratchpad arena
pub const KERNEL_STATIC_ARENA_SIZE_BYTES: usize = 1024 * 1024 * 4; // 4 MiB non-heap static pool

/// Pre-allocated ring buffer slot capacity (must be a power of two)
pub const HARDWARE_RING_BUFFER_CAPACITY: usize = 1024;

/// CRC-32 IEEE 802.3 Ethernet / AUTOSAR Polynomial
pub const CRC32_POLYNOMIAL: u32 = 0xEDB88320;

/// CRC-8 SAE J1850 Automotive Polynomial
pub const CRC8_SAE_J1850_POLY: u8 = 0x1D;

// =================================================================================================
// 2. ERROR TAXONOMY & SAFETY LEVEL DEFINITIONS (ISO 26262 / MISRA-RUST)
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelError {
    Ok = 0,
    MemoryExhaustion = 1,
    AlignmentViolation = 2,
    LockstepCoreDesync = 3,
    WatchdogDeadlineMissed = 4,
    RingBufferOverflow = 5,
    RingBufferUnderflow = 6,
    TaskDeadlineMissed = 7,
    TaskPriorityInversion = 8,
    IllegalTaskStateTransition = 9,
    HardwareFaultTrapped = 10,
    InvalidMemoryAccess = 11,
    HypervisorMailboxTimeout = 12,
    ActuatorTorqueRunaway = 13,
    BrakePressureMismatch = 14,
    SteeringAngleRateExceeded = 15,
    AsilDowngradeForbidden = 16,
    StaticTableFull = 17,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AsilLevel {
    Qm = 0,
    AsilA = 1,
    AsilB = 2,
    AsilC = 3,
    AsilD = 4,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeSystemMode {
    Init = 0,
    NominalOperation = 1,
    DegradedSafeState = 2,
    EmergencyStopActuated = 3,
    FatalHardwareLockdown = 4,
}

// =================================================================================================
// 3. BARE-METAL RUNTIME, PANIC HANDLING & HARDWARE BARRIERS
// =================================================================================================

#[inline(always)]
pub fn memory_barrier() {
    fence(Ordering::SeqCst);
    compiler_fence(Ordering::SeqCst);
}

#[inline(always)]
pub fn read_cycle_counter() -> u64 {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: reading the virtual counter touches no memory and no stack (`nomem, nostack`) and
    // has no side effects. It is only valid when the counter is readable at the privilege level
    // this runs at (EL1 always; EL0 only if the kernel enabled it), which the integrator must
    // ensure: on a target where it is not, the read traps.
    unsafe {
        let val: u64;
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `rdcycle` reads a counter, touching no memory or stack and with no side effects.
    // It is only valid when the cycle counter is accessible at this privilege level (the
    // `mcounteren` bit set); where it is not, the instruction traps.
    unsafe {
        let val: u64;
        core::arch::asm!("rdcycle {}", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    {
        static MOCK_TICK: AtomicU64 = AtomicU64::new(0);
        MOCK_TICK.fetch_add(10, Ordering::Relaxed)
    }
}

/// Panic handler for bare-metal builds. With the `std` feature the standard library provides
/// its own, so this only exists in `no_std` builds.
#[cfg(not(feature = "std"))]
#[panic_handler]
fn automotive_panic_handler(info: &PanicInfo) -> ! {
    // 1. Immediately drop actuator commands by signaling safe hardware state
    let emergency_reg = 0x4000_0000 as *mut u32;
    // SAFETY: this is only sound on a target whose memory map has a writable 32-bit register at
    // 0x4000_0000 (an assumption of this design, not something the code can check). A volatile
    // write is used so the compiler keeps it. On a different memory map this write is undefined
    // behaviour, so integrators must adjust the address; we are already panicking, so there is no
    // state left to protect.
    unsafe {
        // Assert hardware emergency disconnect register if memory-mapped I/O is present
        core::ptr::write_volatile(emergency_reg, 0xDEAD_BEEF);
    }

    // 2. Lock CPU core in deterministic spin-loop to avoid undefined execution
    loop {
        spin_loop();
    }
}

// =================================================================================================
// 4. MEMORY MANAGEMENT: CACHE-ALIGNED STATIC ARENA & FIXED SLAB ALLOCATOR
// =================================================================================================

#[repr(C, align(64))]
pub struct StaticMemoryArena<const SIZE: usize> {
    storage: UnsafeCell<[u8; SIZE]>,
    allocated_bytes: AtomicUsize,
    allocation_count: AtomicUsize,
}

// SAFETY: the only interior-mutable field is `storage`, and `allocate` never creates a reference
// to it: it reserves a range by a compare-exchange on `allocated_bytes` and hands out the address.
// Distinct calls therefore get disjoint ranges, and the counters are atomics. Whoever uses a
// returned pointer is responsible for it being used as the range they were given.
unsafe impl<const SIZE: usize> Sync for StaticMemoryArena<SIZE> {}

impl<const SIZE: usize> StaticMemoryArena<SIZE> {
    pub const fn new() -> Self {
        Self {
            storage: UnsafeCell::new([0u8; SIZE]),
            allocated_bytes: AtomicUsize::new(0),
            allocation_count: AtomicUsize::new(0),
        }
    }

    /// Allocates a raw slice from the static arena guaranteed to align to ALIGN
    pub fn allocate(
        &self,
        layout_size: usize,
        layout_align: usize,
    ) -> Result<*mut u8, KernelError> {
        loop {
            let current = self.allocated_bytes.load(Ordering::Relaxed);
            let unaligned_ptr = (self.storage.get() as usize) + current;
            let aligned_ptr = (unaligned_ptr + (layout_align - 1)) & !(layout_align - 1);
            let padding = aligned_ptr - unaligned_ptr;
            let next_offset = current + padding + layout_size;

            if next_offset > SIZE {
                return Err(KernelError::MemoryExhaustion);
            }

            if self
                .allocated_bytes
                .compare_exchange_weak(current, next_offset, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.allocation_count.fetch_add(1, Ordering::Relaxed);
                return Ok(aligned_ptr as *mut u8);
            }
            spin_loop();
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.allocated_bytes.load(Ordering::Relaxed)
    }

    pub fn remaining_bytes(&self) -> usize {
        SIZE.saturating_sub(self.used_bytes())
    }
}

impl<const SIZE: usize> Default for StaticMemoryArena<SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. CACHELINE-PADDED LOCK-FREE SPSC RING BUFFER (ZERO-COPY IPC)
// =================================================================================================

#[repr(C, align(64))]
pub struct CacheAlignedSpscRing<T: Copy, const CAP: usize> {
    head: AtomicUsize,
    _pad_head: [u8; CACHELINE_ALIGN_BYTES - core::mem::size_of::<AtomicUsize>()],
    tail: AtomicUsize,
    _pad_tail: [u8; CACHELINE_ALIGN_BYTES - core::mem::size_of::<AtomicUsize>()],
    buffer: [UnsafeCell<MaybeUninit<T>>; CAP],
}

// SAFETY: sound only under the single-producer, single-consumer rule: at most one thread at a time
// calls `try_push` and at most one calls `try_pop`. Then the producer writes only the slot at
// `head` (not yet published), the consumer reads only slots below `head` (published by the
// Release store, seen by the Acquire load), and `head` and `tail` are atomics. Nothing in the
// types enforces the rule: two producers would write the same slot, which is a data race. `T`
// is `Send` because values move from the producer's thread to the consumer's.
unsafe impl<T: Copy + Send, const CAP: usize> Sync for CacheAlignedSpscRing<T, CAP> {}

impl<T: Copy + Default, const CAP: usize> CacheAlignedSpscRing<T, CAP> {
    pub const fn new() -> Self {
        const {
            assert!(
                CAP > 0 && (CAP & (CAP - 1)) == 0,
                "Capacity must be an exact power of two"
            );
        }
        // Slots start uninitialised and are only read after a push has written them.
        let buffer: [UnsafeCell<MaybeUninit<T>>; CAP] =
            [const { UnsafeCell::new(MaybeUninit::uninit()) }; CAP];

        Self {
            head: AtomicUsize::new(0),
            _pad_head: [0u8; CACHELINE_ALIGN_BYTES - core::mem::size_of::<AtomicUsize>()],
            tail: AtomicUsize::new(0),
            _pad_tail: [0u8; CACHELINE_ALIGN_BYTES - core::mem::size_of::<AtomicUsize>()],
            buffer,
        }
    }

    #[inline(always)]
    pub fn try_push(&self, item: T) -> Result<(), KernelError> {
        let h = self.head.load(Ordering::Relaxed);
        let t = self.tail.load(Ordering::Acquire);

        if h.wrapping_sub(t) >= CAP {
            return Err(KernelError::RingBufferOverflow);
        }

        let idx = h & (CAP - 1);
        // SAFETY: the check above shows head - tail < CAP, so the slot at `idx` is not one the
        // consumer can still read; and by the single-producer rule nobody else writes it.
        unsafe {
            (*self.buffer[idx].get()).write(item);
        }

        self.head.store(h.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    #[inline(always)]
    pub fn try_pop(&self) -> Result<T, KernelError> {
        let t = self.tail.load(Ordering::Relaxed);
        let h = self.head.load(Ordering::Acquire);

        if t == h {
            return Err(KernelError::RingBufferUnderflow);
        }

        let idx = t & (CAP - 1);
        // SAFETY: head > tail, so this slot was written by a push that the Acquire load saw.
        let item = unsafe { (*self.buffer[idx].get()).assume_init_read() };

        self.tail.store(t.wrapping_add(1), Ordering::Release);
        Ok(item)
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        let h = self.head.load(Ordering::Relaxed);
        let t = self.tail.load(Ordering::Relaxed);
        h.wrapping_sub(t)
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Copy + Default, const CAP: usize> Default for CacheAlignedSpscRing<T, CAP> {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. ASIL-D WATCHDOG BOUNDED SPINLOCK
// =================================================================================================

pub struct BoundedSpinLock {
    locked: AtomicBool,
    owner_core: AtomicU8,
}

impl BoundedSpinLock {
    pub const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            owner_core: AtomicU8::new(0xFF),
        }
    }

    #[inline(always)]
    pub fn lock(&self, core_id: u8, max_spins: u32) -> Result<(), KernelError> {
        let mut spins = 0;
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
            spins += 1;
            if spins >= max_spins {
                return Err(KernelError::WatchdogDeadlineMissed);
            }
        }
        self.owner_core.store(core_id, Ordering::Relaxed);
        Ok(())
    }

    #[inline(always)]
    pub fn unlock(&self) {
        self.owner_core.store(0xFF, Ordering::Relaxed);
        self.locked.store(false, Ordering::Release);
    }
}

impl Default for BoundedSpinLock {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. CHECKSUM COMPUTATION ENGINES (CRC-32 & CRC-8)
// =================================================================================================

pub struct AutomotiveChecksum;

impl AutomotiveChecksum {
    pub fn crc32(buffer: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in buffer {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = if (crc & 1) != 0 { CRC32_POLYNOMIAL } else { 0 };
                crc = (crc >> 1) ^ mask;
            }
        }
        !crc
    }

    pub fn crc8_sae_j1850(buffer: &[u8]) -> u8 {
        let mut crc: u8 = 0xFF;
        for &byte in buffer {
            crc ^= byte;
            for _ in 0..8 {
                if (crc & 0x80) != 0 {
                    crc = (crc << 1) ^ CRC8_SAE_J1850_POLY;
                } else {
                    crc <<= 1;
                }
            }
        }
        !crc
    }
}

// =================================================================================================
// 8. ISO 26262 ASIL-D DUAL-CORE LOCKSTEP & HARDWARE SAFETY WATCHDOG
// =================================================================================================

#[repr(C)]
pub struct LockstepHeartbeatFrame {
    pub core_id: u8,
    pub cycle_step: u64,
    pub computed_crc: u32,
    pub timestamp_micros: u64,
}

pub struct AsilLockstepSupervisor {
    core_heartbeats: [AtomicU64; MAX_CPU_CORES],
    core_crc_shadow: [AtomicU32; MAX_CPU_CORES],
    system_mode: AtomicU8,
    fault_counter: AtomicU32,
    lock: BoundedSpinLock,
}

impl AsilLockstepSupervisor {
    pub const fn new() -> Self {
        Self {
            core_heartbeats: [const { AtomicU64::new(0) }; MAX_CPU_CORES],
            core_crc_shadow: [const { AtomicU32::new(0) }; MAX_CPU_CORES],
            system_mode: AtomicU8::new(SafeSystemMode::Init as u8),
            fault_counter: AtomicU32::new(0),
            lock: BoundedSpinLock::new(),
        }
    }

    pub fn submit_lockstep_step(&self, frame: LockstepHeartbeatFrame) -> Result<(), KernelError> {
        let id = frame.core_id as usize;
        if id >= MAX_CPU_CORES {
            return Err(KernelError::InvalidMemoryAccess);
        }

        self.core_heartbeats[id].store(frame.timestamp_micros, Ordering::Release);
        self.core_crc_shadow[id].store(frame.computed_crc, Ordering::Release);

        // Core 0 & Core 1 evaluate in lockstep pair
        if id == 1 {
            let crc_core0 = self.core_crc_shadow[0].load(Ordering::Acquire);
            let crc_core1 = frame.computed_crc;

            if crc_core0 != crc_core1 {
                self.fault_counter.fetch_add(1, Ordering::SeqCst);
                self.system_mode
                    .store(SafeSystemMode::DegradedSafeState as u8, Ordering::SeqCst);
                return Err(KernelError::LockstepCoreDesync);
            }
        }

        Ok(())
    }

    pub fn verify_safety_health(
        &self,
        current_time_micros: u64,
    ) -> Result<SafeSystemMode, KernelError> {
        let mode_raw = self.system_mode.load(Ordering::Acquire);
        if mode_raw >= (SafeSystemMode::EmergencyStopActuated as u8) {
            return Ok(SafeSystemMode::EmergencyStopActuated);
        }

        // Verify Core 0 (primary ASIL-D executive)
        let last_c0 = self.core_heartbeats[0].load(Ordering::Relaxed);
        if current_time_micros.saturating_sub(last_c0) > WATCHDOG_HARD_DEADLINE_MICROS {
            self.system_mode.store(
                SafeSystemMode::EmergencyStopActuated as u8,
                Ordering::SeqCst,
            );
            return Err(KernelError::WatchdogDeadlineMissed);
        }

        Ok(match mode_raw {
            0 => SafeSystemMode::Init,
            1 => SafeSystemMode::NominalOperation,
            2 => SafeSystemMode::DegradedSafeState,
            _ => SafeSystemMode::FatalHardwareLockdown,
        })
    }

    pub fn escalate_to_emergency_stop(&self) {
        self.system_mode.store(
            SafeSystemMode::EmergencyStopActuated as u8,
            Ordering::SeqCst,
        );
    }
}

impl Default for AsilLockstepSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 9. TIME-TRIGGERED REAL-TIME TASK SCHEDULER (TTA)
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Dormant = 0,
    Ready = 1,
    Running = 2,
    Waiting = 3,
    Suspended = 4,
}

pub type TaskEntrypoint = fn(task_id: u32, timestamp_micros: u64) -> Result<(), KernelError>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SchedulerTaskDescriptor {
    pub task_id: u32,
    pub priority: u8,
    pub asil_level: AsilLevel,
    pub period_micros: u32,
    pub wcet_micros: u32,
    pub entrypoint: Option<TaskEntrypoint>,
    pub last_start_micros: u64,
    pub execution_count: u64,
    pub state: TaskState,
}

impl Default for SchedulerTaskDescriptor {
    fn default() -> Self {
        Self {
            task_id: 0,
            priority: 0,
            asil_level: AsilLevel::Qm,
            period_micros: 1000,
            wcet_micros: 200,
            entrypoint: None,
            last_start_micros: 0,
            execution_count: 0,
            state: TaskState::Dormant,
        }
    }
}

pub struct TimeTriggeredScheduler {
    tasks: [UnsafeCell<SchedulerTaskDescriptor>; MAX_SCHEDULER_TASKS],
    task_count: AtomicUsize,
    current_tick_micros: AtomicU64,
    lock: BoundedSpinLock,
}

// SAFETY: the only interior-mutable field is `tasks`. `register_task` holds `lock` and writes only
// the slot at `task_count`, then publishes it with a SeqCst store of `task_count + 1`;
// `run_scheduler_cycle` reads only slots below the `task_count` it loaded with Acquire, so the two
// never touch the same slot. Sound only if at most one thread at a time calls
// `run_scheduler_cycle` (it takes `&mut` to each slot without a lock); two concurrent cycles
// would alias those `&mut`.
unsafe impl Sync for TimeTriggeredScheduler {}

impl TimeTriggeredScheduler {
    pub const fn new() -> Self {
        const INIT_DESC: UnsafeCell<SchedulerTaskDescriptor> =
            UnsafeCell::new(SchedulerTaskDescriptor {
                task_id: 0,
                priority: 0,
                asil_level: AsilLevel::Qm,
                period_micros: 1000,
                wcet_micros: 200,
                entrypoint: None,
                last_start_micros: 0,
                execution_count: 0,
                state: TaskState::Dormant,
            });

        Self {
            tasks: [INIT_DESC; MAX_SCHEDULER_TASKS],
            task_count: AtomicUsize::new(0),
            current_tick_micros: AtomicU64::new(0),
            lock: BoundedSpinLock::new(),
        }
    }

    pub fn register_task(
        &self,
        task_id: u32,
        priority: u8,
        asil: AsilLevel,
        period_micros: u32,
        wcet_micros: u32,
        entry: TaskEntrypoint,
    ) -> Result<(), KernelError> {
        self.lock.lock(0, 10_000)?;
        let count = self.task_count.load(Ordering::Relaxed);
        if count >= MAX_SCHEDULER_TASKS {
            self.lock.unlock();
            return Err(KernelError::StaticTableFull);
        }

        // SAFETY: `lock` is held, so no other `register_task` runs; the slot at `count` is not
        // yet published (`task_count` is still `count`), so `run_scheduler_cycle` cannot be
        // reading it, and no other reference to this slot exists.
        let desc = unsafe { &mut *self.tasks[count].get() };
        desc.task_id = task_id;
        desc.priority = priority;
        desc.asil_level = asil;
        desc.period_micros = period_micros;
        desc.wcet_micros = wcet_micros;
        desc.entrypoint = Some(entry);
        desc.state = TaskState::Ready;

        self.task_count.store(count + 1, Ordering::SeqCst);
        self.lock.unlock();
        Ok(())
    }

    pub fn run_scheduler_cycle(&self, current_time: u64) -> Result<u32, KernelError> {
        self.current_tick_micros
            .store(current_time, Ordering::Relaxed);
        let count = self.task_count.load(Ordering::Acquire);
        let mut executed_count = 0;

        for i in 0..count {
            // SAFETY: `i < count` and `count` was loaded with Acquire, so this slot was fully
            // written and published; `register_task` only writes slots at or above `task_count`.
            // The caller must not run two scheduler cycles at once (see the `Sync` impl).
            let task = unsafe { &mut *self.tasks[i].get() };

            if task.state == TaskState::Suspended {
                continue;
            }

            let elapsed = current_time.saturating_sub(task.last_start_micros);
            if elapsed >= task.period_micros as u64 {
                task.state = TaskState::Running;
                let start_time = read_cycle_counter();

                if let Some(entry) = task.entrypoint {
                    if let Err(e) = entry(task.task_id, current_time) {
                        task.state = TaskState::Suspended;
                        return Err(e);
                    }
                }

                let end_time = read_cycle_counter();
                task.last_start_micros = current_time;
                task.execution_count += 1;
                task.state = TaskState::Ready;
                executed_count += 1;
            }
        }

        Ok(executed_count)
    }
}

impl Default for TimeTriggeredScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 10. PART 1 VERIFICATION & INTEGRATION TEST HARNESS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_static_arena_allocation_alignment() {
        let arena: StaticMemoryArena<1024> = StaticMemoryArena::new();
        let ptr1 = arena.allocate(16, 8).expect("Allocation 1 failed");
        let ptr2 = arena.allocate(64, 64).expect("Allocation 2 failed");

        assert_eq!((ptr1 as usize) % 8, 0);
        assert_eq!((ptr2 as usize) % 64, 0);
        assert!(arena.used_bytes() >= 80);
    }

    #[test]
    fn test_spsc_ring_buffer_holds_its_full_capacity() {
        let ring: CacheAlignedSpscRing<u64, 16> = CacheAlignedSpscRing::new();
        assert!(ring.is_empty());

        for i in 0..16 {
            assert!(ring.try_push(i).is_ok());
        }
        assert_eq!(ring.len(), 16);
        assert_eq!(ring.try_push(99), Err(KernelError::RingBufferOverflow));

        for i in 0..16 {
            assert_eq!(ring.try_pop(), Ok(i));
        }
        assert_eq!(ring.try_pop(), Err(KernelError::RingBufferUnderflow));
        assert!(ring.is_empty());
    }

    #[test]
    fn test_spsc_ring_buffer_wraps_around_many_laps() {
        let ring: CacheAlignedSpscRing<u32, 8> = CacheAlignedSpscRing::new();
        let mut next_in = 0u32;
        let mut next_out = 0u32;
        // Push 5, pop 3, repeatedly: the indices lap the 8 slots many times.
        for _ in 0..1000 {
            for _ in 0..5 {
                if ring.try_push(next_in).is_ok() {
                    next_in += 1;
                }
            }
            for _ in 0..3 {
                if let Ok(v) = ring.try_pop() {
                    assert_eq!(v, next_out);
                    next_out += 1;
                }
            }
            assert_eq!(ring.len() as u32, next_in - next_out);
        }
        while let Ok(v) = ring.try_pop() {
            assert_eq!(v, next_out);
            next_out += 1;
        }
        assert_eq!(next_in, next_out);
    }

    #[test]
    fn test_spsc_ring_buffer_between_two_threads() {
        static RING: CacheAlignedSpscRing<u64, 64> = CacheAlignedSpscRing::new();
        const N: u64 = 200_000;
        let producer = std::thread::spawn(|| {
            let mut i = 0;
            while i < N {
                if RING.try_push(i).is_ok() {
                    i += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });
        let consumer = std::thread::spawn(|| {
            let mut expected = 0;
            while expected < N {
                match RING.try_pop() {
                    Ok(v) => {
                        assert_eq!(
                            v, expected,
                            "items must arrive in order, none lost or repeated"
                        );
                        expected += 1;
                    }
                    Err(_) => std::thread::yield_now(),
                }
            }
        });
        producer.join().unwrap();
        consumer.join().unwrap();
        assert!(RING.is_empty());
    }

    #[test]
    fn test_lockstep_fault_detection() {
        let supervisor = AsilLockstepSupervisor::new();

        // Normal lockstep match
        let f0 = LockstepHeartbeatFrame {
            core_id: 0,
            cycle_step: 1,
            computed_crc: 0x1234_5678,
            timestamp_micros: 1000,
        };
        let f1 = LockstepHeartbeatFrame {
            core_id: 1,
            cycle_step: 1,
            computed_crc: 0x1234_5678,
            timestamp_micros: 1000,
        };

        assert!(supervisor.submit_lockstep_step(f0).is_ok());
        assert!(supervisor.submit_lockstep_step(f1).is_ok());

        // Desynchronized lockstep mismatch
        let f1_mismatch = LockstepHeartbeatFrame {
            core_id: 1,
            cycle_step: 2,
            computed_crc: 0xDEAD_BEEF,
            timestamp_micros: 2000,
        };
        assert_eq!(
            supervisor.submit_lockstep_step(f1_mismatch),
            Err(KernelError::LockstepCoreDesync)
        );
    }

    static TEST_RUN_FLAG: AtomicBool = AtomicBool::new(false);

    fn sample_task(_id: u32, _ts: u64) -> Result<(), KernelError> {
        TEST_RUN_FLAG.store(true, Ordering::SeqCst);
        Ok(())
    }

    #[test]
    fn test_time_triggered_scheduler() {
        let sched = TimeTriggeredScheduler::new();
        sched
            .register_task(101, 1, AsilLevel::AsilD, 1000, 200, sample_task)
            .expect("Task registration failed");

        assert_eq!(sched.run_scheduler_cycle(1000), Ok(1));
        assert!(TEST_RUN_FLAG.load(Ordering::SeqCst));
    }
}

#[cfg(test)]
mod checksum_reference_vectors {
    use super::*;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 11 + 7) & 0xFF) as u8).collect()
    }

    #[test]
    fn crc32_matches_the_standard_check_value_and_zlib() {
        // The catalogue check value for CRC-32/ISO-HDLC, and zlib's CRC over generated data.
        assert_eq!(AutomotiveChecksum::crc32(b"123456789"), 0xCBF4_3926);
        for (n, want) in [
            (0usize, 0x0000_0000u32),
            (1, 0x4c66_7a2e),
            (7, 0x83cb_2f02),
            (64, 0x6a63_3fa4),
            (255, 0x68ac_9e4d),
        ] {
            assert_eq!(AutomotiveChecksum::crc32(&data(n)), want, "length {n}");
        }
    }

    #[test]
    fn crc8_sae_j1850_matches_the_standard_check_value() {
        assert_eq!(AutomotiveChecksum::crc8_sae_j1850(b"123456789"), 0x4B);
    }
}
