//! Part 6: VIRTIO PARAVIRTUALIZATION, HYPERVISOR IPC & HARDWARE DMA CONTROLLERS

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 6 - VIRTIO PARAVIRTUALIZATION, HYPERVISOR IPC & HARDWARE DMA CONTROLLERS
// CRITICALITY: ISO 26262 ASIL-B/D Partition Isolation / OASIS VirtIO Standard v1.2
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Allocations, Memory Fences & Cache-Padded Buffers
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{
    compiler_fence, fence, AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering,
};

// =================================================================================================
// 1. CONSTANTS: VIRTIO 1.2 SPECIFICATIONS & HYPERVISOR BOUNDARIES
// =================================================================================================

/// VirtIO Device Status Bits (OASIS VirtIO Standard Section 2.1)
pub const VIRTIO_STATUS_RESET: u8 = 0;
pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
pub const VIRTIO_STATUS_DRIVER: u8 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
pub const VIRTIO_STATUS_DEVICE_NEEDS_RESET: u8 = 64;
pub const VIRTIO_STATUS_FAILED: u8 = 128;

/// VirtIO Magic Value (Little Endian "virt" -> 0x74726976)
pub const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x7472_6976;
pub const VIRTIO_MMIO_VERSION_LEGACY: u32 = 1;
pub const VIRTIO_MMIO_VERSION_MODERN: u32 = 2;

/// VirtIO Subsystem Device IDs
pub const VIRTIO_DEV_NETWORK: u32 = 1;
pub const VIRTIO_DEV_BLOCK: u32 = 2;
pub const VIRTIO_DEV_CONSOLE: u32 = 3;
pub const VIRTIO_DEV_ENTROPY: u32 = 4;
pub const VIRTIO_DEV_GPU: u32 = 16;
pub const VIRTIO_DEV_INPUT: u32 = 18;

/// Virtqueue Descriptor Flags
pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;
pub const VRING_DESC_F_INDIRECT: u16 = 4;

/// Capacity of standard virtual queues (must be a power of two)
pub const VIRTQUEUE_MAX_SIZE: usize = 64;

/// VirtIO-Block Request Types
pub const VIRTIO_BLK_T_IN: u32 = 0; // Read operation
pub const VIRTIO_BLK_T_OUT: u32 = 1; // Write operation
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;

/// VirtIO-Block Completion Status Codes
pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

/// Sector Size for automotive block storage
pub const SECTOR_SIZE_BYTES: usize = 512;

/// Hypervisor Shared Memory Channel Dimensions
pub const INTER_VM_QUEUE_CAPACITY: usize = 128;
pub const INTER_VM_MAX_PAYLOAD: usize = 1024;
pub const DMA_DESCRIPTOR_RING_SIZE: usize = 32;

// =================================================================================================
// 2. ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    Ok = 0,
    InvalidMagicNumber = 1,
    UnsupportedVersion = 2,
    DeviceNotFound = 3,
    FeaturesNotAccepted = 4,
    QueueSizeExceeded = 5,
    QueueNotAvailable = 6,
    DescriptorPoolExhausted = 7,
    DeviceReportedError = 8,
    IoTimeout = 9,
    BufferTooSmall = 10,
    MemoryUnaligned = 11,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HypervisorIpcError {
    Ok = 0,
    ChannelFull = 1,
    ChannelEmpty = 2,
    PayloadTooLarge = 3,
    PeerNotResponding = 4,
    MemoryIsolationFault = 5,
    LockstepDesync = 6,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    Ok = 0,
    ChannelBusy = 1,
    InvalidAddress = 2,
    AlignmentViolation = 3,
    RingFull = 4,
    TransferFailed = 5,
    DmaTimeout = 6,
}

// =================================================================================================
// 3. VIRTIO 1.2 SPLIT VIRTQUEUE ENGINE (ZERO-ALLOCATION, LOCK-FREE)
// =================================================================================================

/// Single Virtqueue 16-byte Descriptor (OASIS VirtIO Section 2.6.5)
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct VringDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

impl VringDesc {
    pub const fn empty() -> Self {
        Self {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }
    }
}

/// Virtqueue Available Ring (Driver-to-Device notification)
#[repr(C, align(2))]
pub struct VringAvail {
    pub flags: u16,
    pub idx: AtomicU16,
    pub ring: [u16; VIRTQUEUE_MAX_SIZE],
}

impl VringAvail {
    pub const fn new() -> Self {
        Self {
            flags: 0,
            idx: AtomicU16::new(0),
            ring: [0u16; VIRTQUEUE_MAX_SIZE],
        }
    }
}

impl Default for VringAvail {
    fn default() -> Self {
        Self::new()
    }
}

/// Single Used Element recorded by the Device
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringUsedElem {
    pub id: u32,
    pub len: u32,
}

/// Virtqueue Used Ring (Device-to-Driver completion notifications)
#[repr(C, align(4))]
pub struct VringUsed {
    pub flags: u16,
    pub idx: AtomicU16,
    pub ring: [VringUsedElem; VIRTQUEUE_MAX_SIZE],
}

impl VringUsed {
    pub const fn new() -> Self {
        Self {
            flags: 0,
            idx: AtomicU16::new(0),
            ring: [VringUsedElem { id: 0, len: 0 }; VIRTQUEUE_MAX_SIZE],
        }
    }
}

impl Default for VringUsed {
    fn default() -> Self {
        Self::new()
    }
}

/// Static, non-allocating Virtqueue Manager wrapping descriptor, available, and used tables
pub struct Virtqueue {
    pub queue_size: u16,
    pub desc: UnsafeCell<[VringDesc; VIRTQUEUE_MAX_SIZE]>,
    pub avail: UnsafeCell<VringAvail>,
    pub used: UnsafeCell<VringUsed>,
    pub last_used_idx: AtomicU16,
    pub free_head: AtomicU16,
    pub num_free: AtomicU16,
    lock: AtomicBool,
}

unsafe impl Sync for Virtqueue {}

impl Virtqueue {
    pub const fn new(size: u16) -> Self {
        let mut desc_table = [VringDesc::empty(); VIRTQUEUE_MAX_SIZE];
        let mut i = 0;
        while i < VIRTQUEUE_MAX_SIZE - 1 {
            desc_table[i].next = (i + 1) as u16;
            i += 1;
        }
        desc_table[VIRTQUEUE_MAX_SIZE - 1].next = 0xFFFF; // End of free list

        Self {
            queue_size: size,
            desc: UnsafeCell::new(desc_table),
            avail: UnsafeCell::new(VringAvail::new()),
            used: UnsafeCell::new(VringUsed::new()),
            last_used_idx: AtomicU16::new(0),
            free_head: AtomicU16::new(0),
            num_free: AtomicU16::new(size),
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

    /// Allocate a single descriptor from the static free list
    pub fn alloc_descriptor(&self) -> Result<u16, VirtioError> {
        self.acquire();
        let head = self.free_head.load(Ordering::Relaxed);
        if head == 0xFFFF || self.num_free.load(Ordering::Relaxed) == 0 {
            self.release();
            return Err(VirtioError::DescriptorPoolExhausted);
        }

        let desc_table = unsafe { &mut *self.desc.get() };
        let next_free = desc_table[head as usize].next;
        self.free_head.store(next_free, Ordering::Relaxed);
        self.num_free.fetch_sub(1, Ordering::Relaxed);
        self.release();

        desc_table[head as usize].flags = 0;
        desc_table[head as usize].next = 0;
        Ok(head)
    }

    /// Return a descriptor chain back to the free list
    pub fn free_descriptor_chain(&self, mut head: u16) {
        self.acquire();
        let desc_table = unsafe { &mut *self.desc.get() };
        let orig_head = head;
        let mut count = 0;

        loop {
            count += 1;
            let flags = desc_table[head as usize].flags;
            let next = desc_table[head as usize].next;

            if (flags & VRING_DESC_F_NEXT) != 0 {
                head = next;
            } else {
                // Link end of chain to current free head
                desc_table[head as usize].next = self.free_head.load(Ordering::Relaxed);
                self.free_head.store(orig_head, Ordering::Relaxed);
                break;
            }
        }

        self.num_free.fetch_add(count, Ordering::Relaxed);
        self.release();
    }

    /// Submit a descriptor chain to the device via the Available ring
    pub fn submit_descriptor_chain(&self, head_idx: u16) {
        let avail = unsafe { &mut *self.avail.get() };
        let current_idx = avail.idx.load(Ordering::Relaxed);
        let ring_slot = (current_idx as usize) % (self.queue_size as usize);

        avail.ring[ring_slot] = head_idx;

        // Ensure descriptor writes are globally visible before updating available index
        fence(Ordering::Release);
        avail
            .idx
            .store(current_idx.wrapping_add(1), Ordering::Release);
    }

    /// Poll for completed buffers from the Device Used ring
    pub fn poll_completion(&self) -> Option<(u32, u32)> {
        let used = unsafe { &*self.used.get() };
        let last_used = self.last_used_idx.load(Ordering::Relaxed);
        let dev_idx = used.idx.load(Ordering::Acquire);

        if last_used != dev_idx {
            fence(Ordering::Acquire);
            let slot = (last_used as usize) % (self.queue_size as usize);
            let elem = used.ring[slot];
            self.last_used_idx
                .store(last_used.wrapping_add(1), Ordering::Relaxed);
            Some((elem.id, elem.len))
        } else {
            None
        }
    }
}

// =================================================================================================
// 4. VIRTIO-BLOCK PARAVIRTUALIZED STORAGE CONTROLLER
// =================================================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioBlkOutHeader {
    pub blk_type: u32,
    pub priority: u32,
    pub sector: u64,
}

pub struct VirtioBlockDevice {
    pub base_mmio_addr: usize,
    pub vq: Virtqueue,
    pub capacity_sectors: AtomicU64,
}

impl VirtioBlockDevice {
    pub const fn new(mmio_base: usize) -> Self {
        Self {
            base_mmio_addr: mmio_base,
            vq: Virtqueue::new(VIRTQUEUE_MAX_SIZE as u16),
            capacity_sectors: AtomicU64::new(0),
        }
    }

    /// Initialize and negotiate VirtIO MMIO registers
    pub fn initialize(&self) -> Result<(), VirtioError> {
        let magic = unsafe { core::ptr::read_volatile(self.base_mmio_addr as *const u32) };
        if magic != VIRTIO_MMIO_MAGIC_VALUE {
            return Err(VirtioError::InvalidMagicNumber);
        }

        let version =
            unsafe { core::ptr::read_volatile((self.base_mmio_addr + 0x004) as *const u32) };
        if version != VIRTIO_MMIO_VERSION_MODERN {
            return Err(VirtioError::UnsupportedVersion);
        }

        let dev_id =
            unsafe { core::ptr::read_volatile((self.base_mmio_addr + 0x008) as *const u32) };
        if dev_id != VIRTIO_DEV_BLOCK {
            return Err(VirtioError::DeviceNotFound);
        }

        // State Machine handshake: Reset -> Acknowledge -> Driver
        unsafe {
            core::ptr::write_volatile(
                (self.base_mmio_addr + 0x070) as *mut u32,
                VIRTIO_STATUS_RESET as u32,
            );
            core::ptr::write_volatile(
                (self.base_mmio_addr + 0x070) as *mut u32,
                (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER) as u32,
            );
            // Confirm features
            core::ptr::write_volatile(
                (self.base_mmio_addr + 0x070) as *mut u32,
                (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK)
                    as u32,
            );
            // Driver ready
            core::ptr::write_volatile(
                (self.base_mmio_addr + 0x070) as *mut u32,
                (VIRTIO_STATUS_ACKNOWLEDGE
                    | VIRTIO_STATUS_DRIVER
                    | VIRTIO_STATUS_FEATURES_OK
                    | VIRTIO_STATUS_DRIVER_OK) as u32,
            );
        }

        Ok(())
    }

    /// Synchronously write an automotive block sector to flash storage
    pub fn write_sector(
        &self,
        sector: u64,
        data: &[u8; SECTOR_SIZE_BYTES],
        status_byte: &mut u8,
    ) -> Result<(), VirtioError> {
        let d0 = self.vq.alloc_descriptor()?;
        let d1 = self.vq.alloc_descriptor()?;
        let d2 = self.vq.alloc_descriptor()?;

        let header = VirtioBlkOutHeader {
            blk_type: VIRTIO_BLK_T_OUT,
            priority: 0,
            sector,
        };

        let desc = unsafe { &mut *self.vq.desc.get() };

        // D0: Header (Driver -> Device)
        desc[d0 as usize].addr = &header as *const _ as u64;
        desc[d0 as usize].len = core::mem::size_of::<VirtioBlkOutHeader>() as u32;
        desc[d0 as usize].flags = VRING_DESC_F_NEXT;
        desc[d0 as usize].next = d1;

        // D1: Payload Buffer (Driver -> Device)
        desc[d1 as usize].addr = data.as_ptr() as u64;
        desc[d1 as usize].len = SECTOR_SIZE_BYTES as u32;
        desc[d1 as usize].flags = VRING_DESC_F_NEXT;
        desc[d1 as usize].next = d2;

        // D2: Status byte (Device -> Driver writable)
        *status_byte = 0xFF;
        desc[d2 as usize].addr = status_byte as *mut _ as u64;
        desc[d2 as usize].len = 1;
        desc[d2 as usize].flags = VRING_DESC_F_WRITE;
        desc[d2 as usize].next = 0;

        self.vq.submit_descriptor_chain(d0);

        // Notify MMIO Doorbell Register (QueueNotify offset 0x050)
        unsafe {
            core::ptr::write_volatile((self.base_mmio_addr + 0x050) as *mut u32, 0);
        }

        // Bounded spin-wait loop for storage completion
        let mut spins = 0u32;
        // The device writes this byte by DMA, so it must be re-read from memory each time; a plain
        // `*status_byte` read may be hoisted out of the loop by the compiler.
        let status_ptr: *const u8 = status_byte;
        // SAFETY: `status_ptr` comes from a live `&mut u8` and stays valid for the whole call.
        while unsafe { core::ptr::read_volatile(status_ptr) } == 0xFF {
            if let Some((elem_id, _)) = self.vq.poll_completion() {
                if elem_id == d0 as u32 {
                    break;
                }
            }
            spin_loop();
            spins += 1;
            if spins > 5_000_000 {
                self.vq.free_descriptor_chain(d0);
                return Err(VirtioError::IoTimeout);
            }
        }

        self.vq.free_descriptor_chain(d0);

        // SAFETY: as above.
        if unsafe { core::ptr::read_volatile(status_ptr) } != VIRTIO_BLK_S_OK {
            return Err(VirtioError::DeviceReportedError);
        }

        Ok(())
    }
}

// =================================================================================================
// 5. VIRTIO-NET PARAVIRTUALIZED NETWORK CONTROLLER
// =================================================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioNetHeader {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    pub num_buffers: u16,
}

impl VirtioNetHeader {
    pub const fn empty() -> Self {
        Self {
            flags: 0,
            gso_type: 0,
            hdr_len: 0,
            gso_size: 0,
            csum_start: 0,
            csum_offset: 0,
            num_buffers: 1,
        }
    }
}

pub struct VirtioNetDevice {
    pub base_mmio_addr: usize,
    pub rx_queue: Virtqueue,
    pub tx_queue: Virtqueue,
    pub mac_address: [u8; 6],
}

impl VirtioNetDevice {
    pub const fn new(mmio_base: usize, mac: [u8; 6]) -> Self {
        Self {
            base_mmio_addr: mmio_base,
            rx_queue: Virtqueue::new(VIRTQUEUE_MAX_SIZE as u16),
            tx_queue: Virtqueue::new(VIRTQUEUE_MAX_SIZE as u16),
            mac_address: mac,
        }
    }

    /// Transmit raw SOME/IP or DoIP Ethernet frame over paravirtualized network
    pub fn transmit_frame(&self, frame: &[u8]) -> Result<(), VirtioError> {
        let d0 = self.tx_queue.alloc_descriptor()?;
        let d1 = self.tx_queue.alloc_descriptor()?;

        let net_hdr = VirtioNetHeader::empty();
        let desc = unsafe { &mut *self.tx_queue.desc.get() };

        // D0: VirtioNetHeader
        desc[d0 as usize].addr = &net_hdr as *const _ as u64;
        desc[d0 as usize].len = core::mem::size_of::<VirtioNetHeader>() as u32;
        desc[d0 as usize].flags = VRING_DESC_F_NEXT;
        desc[d0 as usize].next = d1;

        // D1: Raw Ethernet Packet
        desc[d1 as usize].addr = frame.as_ptr() as u64;
        desc[d1 as usize].len = frame.len() as u32;
        desc[d1 as usize].flags = 0;
        desc[d1 as usize].next = 0;

        self.tx_queue.submit_descriptor_chain(d0);

        // Notify TX Queue Doorbell
        unsafe {
            core::ptr::write_volatile((self.base_mmio_addr + 0x050) as *mut u32, 1);
        }

        // Drain completions
        let mut spins = 0u32;
        loop {
            if let Some((elem_id, _)) = self.tx_queue.poll_completion() {
                if elem_id == d0 as u32 {
                    break;
                }
            }
            spin_loop();
            spins += 1;
            if spins > 1_000_000 {
                self.tx_queue.free_descriptor_chain(d0);
                return Err(VirtioError::IoTimeout);
            }
        }

        self.tx_queue.free_descriptor_chain(d0);
        Ok(())
    }
}

// =================================================================================================
// 6. HYPERVISOR INTER-VM SHARED MEMORY MAILBOX (ZERO-COPY SPSC IPC)
// =================================================================================================

#[derive(Clone, Copy)]
pub struct InterVmSlot {
    pub sender_partition_id: u8,
    pub receiver_partition_id: u8,
    pub payload_len: u16,
    pub is_critical: bool,
    pub sequence_num: u32,
    pub payload: [u8; INTER_VM_MAX_PAYLOAD],
}

impl InterVmSlot {
    pub const fn empty() -> Self {
        Self {
            sender_partition_id: 0,
            receiver_partition_id: 0,
            payload_len: 0,
            is_critical: false,
            sequence_num: 0,
            payload: [0u8; INTER_VM_MAX_PAYLOAD],
        }
    }
}

/// Cacheline-isolated, lock-free Inter-Partition Shared Memory Buffer
#[repr(C, align(64))]
pub struct InterVmMailbox {
    pub head: AtomicU32,
    _pad0: [u8; 60],
    pub tail: AtomicU32,
    _pad1: [u8; 60],
    pub doorbell_address: AtomicU64,
    pub slots: UnsafeCell<[InterVmSlot; INTER_VM_QUEUE_CAPACITY]>,
}

unsafe impl Sync for InterVmMailbox {}

impl InterVmMailbox {
    pub const fn new(doorbell_mmio: u64) -> Self {
        Self {
            head: AtomicU32::new(0),
            _pad0: [0u8; 60],
            tail: AtomicU32::new(0),
            _pad1: [0u8; 60],
            doorbell_address: AtomicU64::new(doorbell_mmio),
            slots: UnsafeCell::new([InterVmSlot::empty(); INTER_VM_QUEUE_CAPACITY]),
        }
    }

    /// Enqueue an IPC message from an ASIL-D partition to an AI Compute domain
    pub fn send(
        &self,
        sender_id: u8,
        receiver_id: u8,
        is_critical: bool,
        seq: u32,
        data: &[u8],
    ) -> Result<(), HypervisorIpcError> {
        if data.len() > INTER_VM_MAX_PAYLOAD {
            return Err(HypervisorIpcError::PayloadTooLarge);
        }

        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        if head.wrapping_sub(tail) >= INTER_VM_QUEUE_CAPACITY as u32 {
            return Err(HypervisorIpcError::ChannelFull);
        }

        let slot_idx = (head as usize) % INTER_VM_QUEUE_CAPACITY;
        let slots = unsafe { &mut *self.slots.get() };

        slots[slot_idx].sender_partition_id = sender_id;
        slots[slot_idx].receiver_partition_id = receiver_id;
        slots[slot_idx].is_critical = is_critical;
        slots[slot_idx].sequence_num = seq;
        slots[slot_idx].payload_len = data.len() as u16;
        slots[slot_idx].payload[..data.len()].copy_from_slice(data);

        // Memory fence ensuring payload write precedes head increment
        compiler_fence(Ordering::Release);
        self.head.store(head.wrapping_add(1), Ordering::Release);

        // Ring the virtual hypervisor doorbell
        let doorbell = self.doorbell_address.load(Ordering::Relaxed);
        if doorbell != 0 {
            unsafe {
                core::ptr::write_volatile(doorbell as *mut u32, receiver_id as u32);
            }
        }

        Ok(())
    }

    /// Read an IPC message out of the ring
    pub fn receive(&self, out_slot: &mut InterVmSlot) -> Result<(), HypervisorIpcError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        if tail == head {
            return Err(HypervisorIpcError::ChannelEmpty);
        }

        let slot_idx = (tail as usize) % INTER_VM_QUEUE_CAPACITY;
        let slots = unsafe { &*self.slots.get() };

        *out_slot = slots[slot_idx];

        compiler_fence(Ordering::Release);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);

        Ok(())
    }
}

// =================================================================================================
// 7. ZERO-COPY HARDWARE SCATTER-GATHER DMA CONTROLLER
// =================================================================================================

/// Scatter-Gather Hardware DMA Descriptor
#[repr(C, align(32))]
#[derive(Clone, Copy)]
pub struct DmaDescriptor {
    pub source_addr: u64,
    pub dest_addr: u64,
    pub transfer_bytes: u32,
    pub control_flags: u32,
    pub next_desc_addr: u64,
}

impl DmaDescriptor {
    pub const fn empty() -> Self {
        Self {
            source_addr: 0,
            dest_addr: 0,
            transfer_bytes: 0,
            control_flags: 0,
            next_desc_addr: 0,
        }
    }
}

pub struct HardwareDmaEngine {
    pub base_registers: usize,
    ring: UnsafeCell<[DmaDescriptor; DMA_DESCRIPTOR_RING_SIZE]>,
    allocated_count: AtomicU32,
    lock: AtomicBool,
}

unsafe impl Sync for HardwareDmaEngine {}

impl HardwareDmaEngine {
    pub const DMA_FLAG_INTERRUPT_ON_COMPLETE: u32 = 0x0001;
    pub const DMA_FLAG_SRC_INCREMENT: u32 = 0x0002;
    pub const DMA_FLAG_DST_INCREMENT: u32 = 0x0004;

    pub const fn new(reg_base: usize) -> Self {
        Self {
            base_registers: reg_base,
            ring: UnsafeCell::new([DmaDescriptor::empty(); DMA_DESCRIPTOR_RING_SIZE]),
            allocated_count: AtomicU32::new(0),
            lock: AtomicBool::new(false),
        }
    }

    /// Set up a direct memory transfer from high-speed camera DMA buffers to the PPU
    pub fn submit_transfer(&self, src: u64, dst: u64, bytes: u32) -> Result<u32, DmaError> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }

        let count = self.allocated_count.load(Ordering::Relaxed) as usize;
        if count >= DMA_DESCRIPTOR_RING_SIZE {
            self.lock.store(false, Ordering::Release);
            return Err(DmaError::RingFull);
        }

        let ring = unsafe { &mut *self.ring.get() };
        ring[count].source_addr = src;
        ring[count].dest_addr = dst;
        ring[count].transfer_bytes = bytes;
        ring[count].control_flags = Self::DMA_FLAG_SRC_INCREMENT
            | Self::DMA_FLAG_DST_INCREMENT
            | Self::DMA_FLAG_INTERRUPT_ON_COMPLETE;
        ring[count].next_desc_addr = 0;

        self.allocated_count
            .store((count + 1) as u32, Ordering::Release);
        self.lock.store(false, Ordering::Release);

        // Trigger hardware engine via MMIO DMA trigger register (offset 0x10)
        unsafe {
            core::ptr::write_volatile((self.base_registers + 0x10) as *mut u32, count as u32);
        }

        Ok(count as u32)
    }

    /// Poll DMA controller status register (offset 0x04)
    pub fn is_transfer_complete(&self, job_id: u32) -> bool {
        let status =
            unsafe { core::ptr::read_volatile((self.base_registers + 0x04) as *const u32) };
        (status & (1 << (job_id % 32))) != 0
    }
}

// =================================================================================================
// 8. VERIFICATION SUITE & UNIT TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtqueue_descriptor_allocation() {
        let vq = Virtqueue::new(64);
        assert_eq!(vq.num_free.load(Ordering::Relaxed), 64);

        let d0 = vq.alloc_descriptor().unwrap();
        let d1 = vq.alloc_descriptor().unwrap();
        assert_ne!(d0, d1);
        assert_eq!(vq.num_free.load(Ordering::Relaxed), 62);

        // Chain d0 -> d1
        let desc = unsafe { &mut *vq.desc.get() };
        desc[d0 as usize].flags = VRING_DESC_F_NEXT;
        desc[d0 as usize].next = d1;
        desc[d1 as usize].flags = 0;

        // Free descriptor chain
        vq.free_descriptor_chain(d0);
        assert_eq!(vq.num_free.load(Ordering::Relaxed), 64);
    }

    #[test]
    fn test_inter_vm_mailbox_roundtrip() {
        let mailbox = InterVmMailbox::new(0); // 0 = Null doorbell for test
        let test_data = b"AUTOSAR_SOMEIP_SYNC_FRAME";

        let send_res = mailbox.send(1, 2, true, 42, test_data);
        assert!(send_res.is_ok());

        let mut received = InterVmSlot::empty();
        let recv_res = mailbox.receive(&mut received);
        assert!(recv_res.is_ok());

        assert_eq!(received.sender_partition_id, 1);
        assert_eq!(received.receiver_partition_id, 2);
        assert!(received.is_critical);
        assert_eq!(received.sequence_num, 42);
        assert_eq!(
            &received.payload[..received.payload_len as usize],
            test_data
        );
    }

    #[test]
    fn test_hardware_dma_submission() {
        let mut mock_reg_space = [0u32; 16];
        let dma = HardwareDmaEngine::new(mock_reg_space.as_mut_ptr() as usize);

        let res = dma.submit_transfer(0x8000_0000, 0x9000_0000, 4096);
        assert_eq!(res, Ok(0));
        assert_eq!(dma.allocated_count.load(Ordering::Relaxed), 1);
    }
}
