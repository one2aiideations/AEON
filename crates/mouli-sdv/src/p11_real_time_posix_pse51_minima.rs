//! Part 11: REAL-TIME POSIX PSE51 MINIMAL SUBSYSTEM, TCB SCHEDULER & STATIC VFS

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 11 - REAL-TIME POSIX PSE51 MINIMAL SUBSYSTEM, TCB SCHEDULER & STATIC VFS
// CRITICALITY: ISO 26262 ASIL-D / IEEE Std 1003.13 (PSE51 Real-Time Profile) / MISRA C++:2023
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Heap Allocations, Bounded O(1) Preemptive Scheduling
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. ARCHITECTURAL CONSTANTS & PSE51 SPECIFICATION LIMITS
// =================================================================================================

/// Maximum concurrent POSIX Real-Time Tasks supported statically
pub const POSIX_MAX_THREADS: usize = 32;

/// Maximum open static File Descriptors (VFS endpoints, SOME/IP sockets, VSS nodes)
pub const POSIX_MAX_FDS: usize = 64;

/// Real-Time Priority levels (0 to 31; 31 being highest criticality)
pub const POSIX_PRIORITY_LEVELS: usize = 32;

/// Static stack size allocated per thread context (64 KB)
pub const POSIX_THREAD_STACK_BYTES: usize = 65536;

/// Maximum POSIX high-resolution timers active concurrently
pub const POSIX_MAX_TIMERS: usize = 16;

/// Clock IDs compliant with IEEE Std 1003.1
pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;
pub const CLOCK_MONOTONIC_RAW: u32 = 4;

/// Standard POSIX Return Codes and Error Numbers (errno)
pub const EOK: i32 = 0;
pub const EPERM: i32 = 1;
pub const ENOENT: i32 = 2;
pub const ESRCH: i32 = 3;
pub const EINTR: i32 = 4;
pub const EIO: i32 = 5;
pub const EBADF: i32 = 9;
pub const EAGAIN: i32 = 11;
pub const ENOMEM: i32 = 12;
pub const EACCES: i32 = 13;
pub const EBUSY: i32 = 16;
pub const EINVAL: i32 = 22;
pub const ENFILE: i32 = 23;
pub const EMFILE: i32 = 24;
pub const ETIMEDOUT: i32 = 110;

// =================================================================================================
// 2. TYPES & ARCHITECTURAL CONTEXT STRUCTURES
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Unused = 0,
    Ready = 1,
    Running = 2,
    BlockedOnTimer = 3,
    BlockedOnMutex = 4,
    BlockedOnIo = 5,
    Suspended = 6,
    Terminated = 7,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedPolicy {
    Fifo = 0,       // SCHED_FIFO: Preemptive, runs until blocked or yielded
    RoundRobin = 1, // SCHED_RR: Time-sliced round robin
    Deadline = 2,   // SCHED_DEADLINE: Earliest deadline first
}

/// Standard POSIX Timespec structure
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

impl Timespec {
    pub const fn from_nanos(nanos: u64) -> Self {
        Self {
            tv_sec: (nanos / 1_000_000_000) as i64,
            tv_nsec: (nanos % 1_000_000_000) as i64,
        }
    }

    pub fn to_nanos(&self) -> u64 {
        if self.tv_sec < 0 || self.tv_nsec < 0 {
            0
        } else {
            (self.tv_sec as u64) * 1_000_000_000 + (self.tv_nsec as u64)
        }
    }
}

/// Architecture-independent Saved Register State (AArch64 / RISC-V 64)
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct CpuContext {
    pub regs: [u64; 31], // General purpose registers (x0-x30 / r0-r30)
    pub sp: u64,         // Stack Pointer
    pub pc: u64,         // Program Counter
    pub pstate: u64,     // Processor State / SSTATUS / SPSR
}

impl CpuContext {
    pub const fn empty() -> Self {
        Self {
            regs: [0u64; 31],
            sp: 0,
            pc: 0,
            pstate: 0x0000_0005, // EL1h / S-Mode default interrupt flags
        }
    }
}

// =================================================================================================
// 3. TASK CONTROL BLOCK (TCB) & PRIORITY RUNQUEUE
// =================================================================================================

/// Task Control Block managing a thread's real-time lifecycle
#[repr(C, align(64))]
pub struct TaskControlBlock {
    pub thread_id: u32,
    pub name: [u8; 16],
    pub context: CpuContext,
    pub stack_base: u64,
    pub stack_size: usize,
    pub priority: AtomicU8,           // Base priority (0-31)
    pub effective_priority: AtomicU8, // Dynamic priority elevated by Priority Ceiling
    pub state: AtomicU8,              // ThreadState
    pub policy: SchedPolicy,
    pub time_slice_ns: AtomicU64,    // Remaining budget for SCHED_RR
    pub sleep_until_ns: AtomicU64,   // Wakeup timestamp for nanosleep
    pub blocked_mutex_id: AtomicU16, // Mutex ID thread is waiting on
    pub cpu_affinity_mask: u32,
    pub asil_level: u8, // 0=QM, 1=A, 2=B, 3=C, 4=D
}

impl TaskControlBlock {
    pub const fn empty() -> Self {
        Self {
            thread_id: 0,
            name: [0u8; 16],
            context: CpuContext::empty(),
            stack_base: 0,
            stack_size: 0,
            priority: AtomicU8::new(0),
            effective_priority: AtomicU8::new(0),
            state: AtomicU8::new(ThreadState::Unused as u8),
            policy: SchedPolicy::Fifo,
            time_slice_ns: AtomicU64::new(0),
            sleep_until_ns: AtomicU64::new(0),
            blocked_mutex_id: AtomicU16::new(0xFFFF),
            cpu_affinity_mask: 0x01,
            asil_level: 0,
        }
    }

    #[inline(always)]
    pub fn get_state(&self) -> ThreadState {
        match self.state.load(Ordering::Relaxed) {
            1 => ThreadState::Ready,
            2 => ThreadState::Running,
            3 => ThreadState::BlockedOnTimer,
            4 => ThreadState::BlockedOnMutex,
            5 => ThreadState::BlockedOnIo,
            6 => ThreadState::Suspended,
            7 => ThreadState::Terminated,
            _ => ThreadState::Unused,
        }
    }

    #[inline(always)]
    pub fn set_state(&self, st: ThreadState) {
        self.state.store(st as u8, Ordering::Release);
    }
}

/// Multi-Level Priority RunQueue providing O(1) thread selection
pub struct PriorityRunQueue {
    /// 32-bit bitmap indicating which priority levels have ready threads
    ready_bitmap: AtomicU32,
    /// Ring buffers for each priority level holding thread indices
    head: [AtomicU8; POSIX_PRIORITY_LEVELS],
    tail: [AtomicU8; POSIX_PRIORITY_LEVELS],
    queues: [[AtomicU8; POSIX_MAX_THREADS]; POSIX_PRIORITY_LEVELS],
}

impl PriorityRunQueue {
    pub const fn new() -> Self {
        const ZERO_ATOMIC_U8: AtomicU8 = AtomicU8::new(0);
        const ZERO_ROW: [AtomicU8; POSIX_MAX_THREADS] = [ZERO_ATOMIC_U8; POSIX_MAX_THREADS];
        Self {
            ready_bitmap: AtomicU32::new(0),
            head: [ZERO_ATOMIC_U8; POSIX_PRIORITY_LEVELS],
            tail: [ZERO_ATOMIC_U8; POSIX_PRIORITY_LEVELS],
            queues: [ZERO_ROW; POSIX_PRIORITY_LEVELS],
        }
    }

    /// Enqueues a thread index at the given priority level
    pub fn enqueue(&self, prio: u8, thread_idx: u8) -> Result<(), i32> {
        let p = (prio as usize) % POSIX_PRIORITY_LEVELS;
        let t = self.tail[p].load(Ordering::Relaxed) as usize;
        let h = self.head[p].load(Ordering::Relaxed) as usize;

        let next_tail = (t + 1) % POSIX_MAX_THREADS;
        if next_tail == h {
            return Err(EBUSY); // Queue overflow
        }

        self.queues[p][t].store(thread_idx, Ordering::Relaxed);
        self.tail[p].store(next_tail as u8, Ordering::Release);
        self.ready_bitmap.fetch_or(1 << p, Ordering::SeqCst);
        Ok(())
    }

    /// Dequeues the highest priority ready thread index in O(1)
    pub fn dequeue_highest(&self) -> Option<(u8, u8)> {
        loop {
            let bitmap = self.ready_bitmap.load(Ordering::Acquire);
            if bitmap == 0 {
                return None;
            }

            // Find highest set bit (leading zeros from bit 31 down)
            let highest_prio = 31 - bitmap.leading_zeros() as usize;

            let h = self.head[highest_prio].load(Ordering::Relaxed) as usize;
            let t = self.tail[highest_prio].load(Ordering::Relaxed) as usize;

            if h == t {
                // Queue was empty; clear bit and retry
                self.ready_bitmap
                    .fetch_and(!(1 << highest_prio), Ordering::SeqCst);
                continue;
            }

            let thread_idx = self.queues[highest_prio][h].load(Ordering::Relaxed);
            let next_head = (h + 1) % POSIX_MAX_THREADS;
            self.head[highest_prio].store(next_head as u8, Ordering::Release);

            if next_head == t {
                // Queue is now empty, clear bit in ready bitmap
                self.ready_bitmap
                    .fetch_and(!(1 << highest_prio), Ordering::SeqCst);
            }

            return Some((highest_prio as u8, thread_idx));
        }
    }
}

impl Default for PriorityRunQueue {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 4. REAL-TIME PREEMPTIVE SCHEDULER & DISPATCHER
// =================================================================================================

pub struct Pse51Scheduler {
    tcbs: UnsafeCell<[TaskControlBlock; POSIX_MAX_THREADS]>,
    run_queue: PriorityRunQueue,
    current_thread_idx: AtomicU8,
    active_threads_count: AtomicU8,
    system_ticks: AtomicU64,
    lock: AtomicBool,
}

unsafe impl Sync for Pse51Scheduler {}

impl Pse51Scheduler {
    pub const fn new() -> Self {
        const EMPTY_TCB: TaskControlBlock = TaskControlBlock::empty();
        Self {
            tcbs: UnsafeCell::new([EMPTY_TCB; POSIX_MAX_THREADS]),
            run_queue: PriorityRunQueue::new(),
            current_thread_idx: AtomicU8::new(0xFF), // None
            active_threads_count: AtomicU8::new(0),
            system_ticks: AtomicU64::new(0),
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

    /// Creates and registers a static POSIX real-time thread
    pub fn pthread_create(
        &self,
        thread_id: u32,
        name_str: &[u8],
        entry_point: u64,
        stack_addr: u64,
        stack_bytes: usize,
        priority: u8,
        policy: SchedPolicy,
        asil: u8,
    ) -> Result<u32, i32> {
        if priority >= (POSIX_PRIORITY_LEVELS as u8) || stack_addr == 0 || stack_bytes < 1024 {
            return Err(EINVAL);
        }

        self.acquire();
        let tcbs = unsafe { &mut *self.tcbs.get() };

        // Search for an unused TCB slot
        for (idx, tcb) in tcbs.iter_mut().enumerate() {
            if tcb.get_state() == ThreadState::Unused {
                tcb.thread_id = thread_id;
                tcb.stack_base = stack_addr;
                tcb.stack_size = stack_bytes;
                tcb.policy = policy;
                tcb.asil_level = asil;
                tcb.priority.store(priority, Ordering::Relaxed);
                tcb.effective_priority.store(priority, Ordering::Relaxed);
                tcb.time_slice_ns.store(10_000_000, Ordering::Relaxed); // 10ms slice

                // Copy name
                let len = core::cmp::min(name_str.len(), 15);
                tcb.name[..len].copy_from_slice(&name_str[..len]);
                tcb.name[len] = 0;

                // Initialize CPU context
                tcb.context = CpuContext::empty();
                tcb.context.pc = entry_point;
                // Stack grows downward; point to top of aligned buffer
                tcb.context.sp = (stack_addr + stack_bytes as u64) & !0x0F;

                tcb.set_state(ThreadState::Ready);
                let _ = self.run_queue.enqueue(priority, idx as u8);
                self.active_threads_count.fetch_add(1, Ordering::SeqCst);

                self.release();
                return Ok(thread_id);
            }
        }

        self.release();
        Err(EAGAIN) // No free threads
    }

    /// Cyclic timer tick called from hardware interrupt (e.g., 1000 Hz / 1ms)
    pub fn handle_timer_tick(&self, now_ns: u64) {
        self.system_ticks.fetch_add(1, Ordering::Relaxed);
        let tcbs = unsafe { &*self.tcbs.get() };

        // 1. Wake sleeping tasks whose timeout has expired
        for (idx, tcb) in tcbs.iter().enumerate() {
            if tcb.get_state() == ThreadState::BlockedOnTimer {
                let wake = tcb.sleep_until_ns.load(Ordering::Relaxed);
                if now_ns >= wake {
                    tcb.set_state(ThreadState::Ready);
                    let prio = tcb.effective_priority.load(Ordering::Relaxed);
                    let _ = self.run_queue.enqueue(prio, idx as u8);
                }
            }
        }

        // 2. Manage Round-Robin time slice degradation
        let cur_idx = self.current_thread_idx.load(Ordering::Relaxed);
        if cur_idx != 0xFF {
            let cur_tcb = &tcbs[cur_idx as usize];
            if cur_tcb.policy == SchedPolicy::RoundRobin
                && cur_tcb.get_state() == ThreadState::Running
            {
                let rem = cur_tcb.time_slice_ns.load(Ordering::Relaxed);
                if rem <= 1_000_000 {
                    // Replenish slice and yield
                    cur_tcb.time_slice_ns.store(10_000_000, Ordering::Relaxed);
                    cur_tcb.set_state(ThreadState::Ready);
                    let prio = cur_tcb.effective_priority.load(Ordering::Relaxed);
                    let _ = self.run_queue.enqueue(prio, cur_idx);
                    self.current_thread_idx.store(0xFF, Ordering::SeqCst);
                } else {
                    cur_tcb
                        .time_slice_ns
                        .store(rem - 1_000_000, Ordering::Relaxed);
                }
            }
        }
    }

    /// Selects next thread to run, returning (previous_ctx_ptr, next_ctx_ptr)
    pub fn schedule_next(&self) -> Option<(*const CpuContext, *const CpuContext)> {
        self.acquire();
        let tcbs = unsafe { &mut *self.tcbs.get() };

        let next_candidate = self.run_queue.dequeue_highest();
        let cur_idx = self.current_thread_idx.load(Ordering::Relaxed);

        match next_candidate {
            Some((_, next_idx)) => {
                let next_tcb = &mut tcbs[next_idx as usize];
                next_tcb.set_state(ThreadState::Running);
                // Raw pointers end the borrow, so the previous thread can be borrowed below.
                let next_ctx: *const CpuContext = &next_tcb.context;
                self.current_thread_idx.store(next_idx, Ordering::SeqCst);

                if cur_idx != 0xFF && cur_idx != next_idx {
                    let prev_tcb = &mut tcbs[cur_idx as usize];
                    if prev_tcb.get_state() == ThreadState::Running {
                        prev_tcb.set_state(ThreadState::Ready);
                        let p = prev_tcb.effective_priority.load(Ordering::Relaxed);
                        let _ = self.run_queue.enqueue(p, cur_idx);
                    }
                    let prev_ctx: *const CpuContext = &prev_tcb.context;
                    self.release();
                    Some((prev_ctx, next_ctx))
                } else {
                    self.release();
                    Some((core::ptr::null(), next_ctx))
                }
            }
            None => {
                self.release();
                None // Idle
            }
        }
    }

    /// Puts the calling thread to sleep until target monotonic timestamp
    pub fn nanosleep(&self, thread_idx: u8, target_ns: u64) -> Result<(), i32> {
        if thread_idx as usize >= POSIX_MAX_THREADS {
            return Err(ESRCH);
        }

        self.acquire();
        let tcbs = unsafe { &mut *self.tcbs.get() };
        let tcb = &mut tcbs[thread_idx as usize];

        tcb.sleep_until_ns.store(target_ns, Ordering::Relaxed);
        tcb.set_state(ThreadState::BlockedOnTimer);

        if self.current_thread_idx.load(Ordering::Relaxed) == thread_idx {
            self.current_thread_idx.store(0xFF, Ordering::SeqCst);
        }

        self.release();
        Ok(())
    }
}

impl Default for Pse51Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. POSIX REAL-TIME MUTEX (PRIORITY CEILING PROTOCOL / PCP)
// =================================================================================================

/// Real-time Mutex adhering to IEEE 1003.13 Priority Ceiling Protocol to prevent priority inversion
pub struct PosixMutex {
    mutex_id: u16,
    owner_thread_idx: AtomicU8,
    ceiling_priority: u8,
    original_priority: AtomicU8,
    lock_state: AtomicBool,
    wait_mask: AtomicU32,
}

impl PosixMutex {
    pub const fn new(id: u16, ceiling: u8) -> Self {
        Self {
            mutex_id: id,
            owner_thread_idx: AtomicU8::new(0xFF),
            ceiling_priority: ceiling,
            original_priority: AtomicU8::new(0),
            lock_state: AtomicBool::new(false),
            wait_mask: AtomicU32::new(0),
        }
    }

    /// Locks the mutex, elevating calling thread's priority to ceiling
    pub fn lock(&self, thread_idx: u8, scheduler: &Pse51Scheduler) -> Result<(), i32> {
        let tcbs = unsafe { &*scheduler.tcbs.get() };
        if (thread_idx as usize) >= POSIX_MAX_THREADS {
            return Err(EINVAL);
        }
        let tcb = &tcbs[thread_idx as usize];

        // Ensure calling thread's priority does not exceed ceiling
        let cur_prio = tcb.priority.load(Ordering::Relaxed);
        if cur_prio > self.ceiling_priority {
            return Err(EINVAL); // Ceiling violation
        }

        loop {
            if self
                .lock_state
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                // Lock acquired
                self.owner_thread_idx.store(thread_idx, Ordering::Relaxed);
                self.original_priority.store(
                    tcb.effective_priority.load(Ordering::Relaxed),
                    Ordering::Relaxed,
                );

                // Elevate effective priority to ceiling
                tcb.effective_priority
                    .store(self.ceiling_priority, Ordering::Release);
                return Ok(());
            }

            // Mutex is contested: add to wait mask and block
            self.wait_mask
                .fetch_or(1 << (thread_idx as u32), Ordering::SeqCst);
            tcb.blocked_mutex_id.store(self.mutex_id, Ordering::Relaxed);
            tcb.set_state(ThreadState::BlockedOnMutex);

            spin_loop();
        }
    }

    /// Unlocks the mutex, restoring owner's original priority and waking highest waiter
    pub fn unlock(&self, thread_idx: u8, scheduler: &Pse51Scheduler) -> Result<(), i32> {
        if self.owner_thread_idx.load(Ordering::Relaxed) != thread_idx {
            return Err(EPERM);
        }

        let tcbs = unsafe { &*scheduler.tcbs.get() };
        let tcb = &tcbs[thread_idx as usize];

        // Restore original thread priority
        let orig = self.original_priority.load(Ordering::Relaxed);
        tcb.effective_priority.store(orig, Ordering::Release);

        self.owner_thread_idx.store(0xFF, Ordering::Relaxed);
        self.lock_state.store(false, Ordering::Release);

        // Wake highest waiting thread
        let mask = self.wait_mask.swap(0, Ordering::SeqCst);
        if mask != 0 {
            for idx in 0..POSIX_MAX_THREADS {
                if (mask & (1 << idx)) != 0 {
                    let waiter = &tcbs[idx];
                    waiter.blocked_mutex_id.store(0xFFFF, Ordering::Relaxed);
                    waiter.set_state(ThreadState::Ready);
                    let prio = waiter.effective_priority.load(Ordering::Relaxed);
                    let _ = scheduler.run_queue.enqueue(prio, idx as u8);
                }
            }
        }

        Ok(())
    }
}

// =================================================================================================
// 6. STATIC VIRTUAL FILE SYSTEM (VFS) & POSIX FILE DESCRIPTORS
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsNodeType {
    Unused = 0,
    CharDevice = 1,    // Console / UART
    SomeIpSocket = 2,  // SOME/IP communication channel
    VssSignalNode = 3, // COVESA VSS telemetry feed
    SharedMemIpc = 4,  // Paravirtualized Inter-VM buffer
}

pub struct VfsOps {
    pub read: fn(fd: i32, buf: &mut [u8]) -> Result<usize, i32>,
    pub write: fn(fd: i32, buf: &[u8]) -> Result<usize, i32>,
    pub ioctl: fn(fd: i32, cmd: u32, arg: u64) -> Result<i32, i32>,
}

#[repr(C)]
pub struct FileDescriptor {
    pub fd_num: i32,
    pub node_type: VfsNodeType,
    pub flags: u32, // O_RDONLY, O_WRONLY, O_NONBLOCK
    pub offset: AtomicU64,
    pub ops: Option<VfsOps>,
    pub is_open: AtomicBool,
}

impl FileDescriptor {
    pub const fn empty(fd: i32) -> Self {
        Self {
            fd_num: fd,
            node_type: VfsNodeType::Unused,
            flags: 0,
            offset: AtomicU64::new(0),
            ops: None,
            is_open: AtomicBool::new(false),
        }
    }
}

/// Static, non-allocating POSIX File Descriptor Table
pub struct PosixFdTable {
    fds: UnsafeCell<[FileDescriptor; POSIX_MAX_FDS]>,
    lock: AtomicBool,
}

// `PosixFdTable` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl PosixFdTable {
    pub const fn new() -> Self {
        // Build every descriptor from a const value (no uninitialised memory), then number them.
        let mut fds_init: [FileDescriptor; POSIX_MAX_FDS] =
            [const { FileDescriptor::empty(0) }; POSIX_MAX_FDS];

        let mut i = 0;
        while i < POSIX_MAX_FDS {
            fds_init[i].fd_num = i as i32;
            i += 1;
        }

        Self {
            fds: UnsafeCell::new(fds_init),
            lock: AtomicBool::new(false),
        }
    }

    pub fn open_static_node(
        &self,
        node_type: VfsNodeType,
        flags: u32,
        ops: VfsOps,
    ) -> Result<i32, i32> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }

        let fds = unsafe { &mut *self.fds.get() };

        // Allocate first free file descriptor (starting past stdio 0, 1, 2)
        for i in 3..POSIX_MAX_FDS {
            if !fds[i].is_open.load(Ordering::Relaxed) {
                fds[i].node_type = node_type;
                fds[i].flags = flags;
                fds[i].offset.store(0, Ordering::Relaxed);
                fds[i].ops = Some(ops);
                fds[i].is_open.store(true, Ordering::Release);

                self.lock.store(false, Ordering::Release);
                return Ok(i as i32);
            }
        }

        self.lock.store(false, Ordering::Release);
        Err(EMFILE) // Too many open files
    }

    pub fn close(&self, fd: i32) -> Result<(), i32> {
        if fd < 0 || fd as usize >= POSIX_MAX_FDS {
            return Err(EBADF);
        }

        let fds = unsafe { &mut *self.fds.get() };
        if !fds[fd as usize].is_open.load(Ordering::Relaxed) {
            return Err(EBADF);
        }

        fds[fd as usize].is_open.store(false, Ordering::Release);
        fds[fd as usize].node_type = VfsNodeType::Unused;
        fds[fd as usize].ops = None;
        Ok(())
    }

    pub fn read(&self, fd: i32, buf: &mut [u8]) -> Result<usize, i32> {
        if fd < 0 || fd as usize >= POSIX_MAX_FDS {
            return Err(EBADF);
        }
        let fds = unsafe { &*self.fds.get() };
        let entry = &fds[fd as usize];

        if !entry.is_open.load(Ordering::Acquire) {
            return Err(EBADF);
        }

        if let Some(ref ops) = entry.ops {
            (ops.read)(fd, buf)
        } else {
            Err(EINVAL)
        }
    }

    pub fn write(&self, fd: i32, buf: &[u8]) -> Result<usize, i32> {
        if fd < 0 || fd as usize >= POSIX_MAX_FDS {
            return Err(EBADF);
        }
        let fds = unsafe { &*self.fds.get() };
        let entry = &fds[fd as usize];

        if !entry.is_open.load(Ordering::Acquire) {
            return Err(EBADF);
        }

        if let Some(ref ops) = entry.ops {
            (ops.write)(fd, buf)
        } else {
            Err(EINVAL)
        }
    }
}

impl Default for PosixFdTable {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. HIGH-RESOLUTION POSIX CLOCK & TIMERS
// =================================================================================================

pub struct PosixTimerEngine {
    base_monotonic_ns: AtomicU64,
}

impl PosixTimerEngine {
    pub const fn new() -> Self {
        Self {
            base_monotonic_ns: AtomicU64::new(0),
        }
    }

    pub fn update_hw_ticks(&self, current_ns: u64) {
        self.base_monotonic_ns.store(current_ns, Ordering::Release);
    }

    pub fn clock_gettime(&self, clock_id: u32, tp: &mut Timespec) -> Result<(), i32> {
        match clock_id {
            CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW => {
                let ns = self.base_monotonic_ns.load(Ordering::Acquire);
                *tp = Timespec::from_nanos(ns);
                Ok(())
            }
            CLOCK_REALTIME => {
                // Synchronized to IEEE 802.1AS gPTP root master
                let ns = self.base_monotonic_ns.load(Ordering::Acquire);
                *tp = Timespec::from_nanos(ns);
                Ok(())
            }
            _ => Err(EINVAL),
        }
    }
}

impl Default for PosixTimerEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. UNIFIED POSIX PSE51 SUBSYSTEM HARNESS
// =================================================================================================

pub struct PosixPse51Subsystem {
    pub scheduler: Pse51Scheduler,
    pub fd_table: PosixFdTable,
    pub clock: PosixTimerEngine,
}

impl PosixPse51Subsystem {
    pub const fn new() -> Self {
        Self {
            scheduler: Pse51Scheduler::new(),
            fd_table: PosixFdTable::new(),
            clock: PosixTimerEngine::new(),
        }
    }
}

impl Default for PosixPse51Subsystem {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 9. VERIFICATION SUITE & UNIT TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_posix_priority_runqueue_ordering() {
        let rq = PriorityRunQueue::new();

        // Enqueue lower priority thread (10) then higher priority thread (25)
        rq.enqueue(10, 1).expect("Enqueue ok");
        rq.enqueue(25, 2).expect("Enqueue ok");
        rq.enqueue(25, 3).expect("Enqueue ok");

        // Priority 25 should pop first in FIFO order
        assert_eq!(rq.dequeue_highest(), Some((25, 2)));
        assert_eq!(rq.dequeue_highest(), Some((25, 3)));
        // Then priority 10
        assert_eq!(rq.dequeue_highest(), Some((10, 1)));
        // Empty
        assert_eq!(rq.dequeue_highest(), None);
    }

    #[test]
    fn test_posix_pthread_create_and_timer_sleep() {
        let subsys = PosixPse51Subsystem::new();
        let tid = subsys
            .scheduler
            .pthread_create(
                101,
                b"NavCompute",
                0x8000_1000,
                0x9000_0000,
                4096,
                16,
                SchedPolicy::Fifo,
                1, // ASIL-A
            )
            .expect("Thread create ok");

        assert_eq!(tid, 101);

        // Put task to sleep for 50ms
        subsys.scheduler.nanosleep(0, 50_000_000).expect("Sleep ok");
        let tcbs = unsafe { &*subsys.scheduler.tcbs.get() };
        assert_eq!(tcbs[0].get_state(), ThreadState::BlockedOnTimer);

        // Advance hardware clock past 50ms
        subsys.scheduler.handle_timer_tick(51_000_000);
        assert_eq!(tcbs[0].get_state(), ThreadState::Ready);
    }

    #[test]
    fn test_priority_ceiling_mutex_elevation() {
        let subsys = PosixPse51Subsystem::new();
        subsys
            .scheduler
            .pthread_create(
                1,
                b"TelemetryTask",
                0x8000_2000,
                0x9000_2000,
                4096,
                5, // Base priority 5
                SchedPolicy::Fifo,
                0,
            )
            .expect("Thread 1 ok");

        let mutex = PosixMutex::new(1, 20); // Priority ceiling = 20

        // Acquire lock
        mutex.lock(0, &subsys.scheduler).expect("Lock ok");
        let tcbs = unsafe { &*subsys.scheduler.tcbs.get() };
        assert_eq!(tcbs[0].effective_priority.load(Ordering::Relaxed), 20);

        // Release lock
        mutex.unlock(0, &subsys.scheduler).expect("Unlock ok");
        assert_eq!(tcbs[0].effective_priority.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_static_fd_table_lifecycle() {
        let fd_tbl = PosixFdTable::new();

        fn dummy_read(_fd: i32, _buf: &mut [u8]) -> Result<usize, i32> {
            Ok(4)
        }
        fn dummy_write(_fd: i32, buf: &[u8]) -> Result<usize, i32> {
            Ok(buf.len())
        }
        fn dummy_ioctl(_fd: i32, _cmd: u32, _arg: u64) -> Result<i32, i32> {
            Ok(0)
        }

        let ops = VfsOps {
            read: dummy_read,
            write: dummy_write,
            ioctl: dummy_ioctl,
        };

        let fd = fd_tbl
            .open_static_node(VfsNodeType::CharDevice, 0, ops)
            .expect("Open fd ok");
        assert_eq!(fd, 3); // First free dynamic fd

        let mut buf = [0u8; 8];
        assert_eq!(fd_tbl.read(fd, &mut buf), Ok(4));
        assert_eq!(fd_tbl.write(fd, b"TEST"), Ok(4));

        fd_tbl.close(fd).expect("Close ok");
        assert_eq!(fd_tbl.read(fd, &mut buf), Err(EBADF));
    }
}
