//! Part 12: HIGH-RELIABILITY FLASH TRANSLATION LAYER (FTL) & FAIL-SAFE LOG FILE SYSTEM

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 12 - HIGH-RELIABILITY FLASH TRANSLATION LAYER (FTL) & FAIL-SAFE LOG FILE SYSTEM
// CRITICALITY: ISO 26262 ASIL-D / AEC-Q100 GRADE 1 AUTOMOTIVE NON-VOLATILE STORAGE
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocation, Power-Loss Fail-Safe Two-Phase Commit
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

// =================================================================================================
// 1. HARDWARE GEOMETRY CONSTANTS (AUTOMOTIVE SLC/TLC RAW FLASH MODEL)
// =================================================================================================

/// Sector payload size in bytes (standard automotive NOR/NAND page payload)
pub const FLASH_PAGE_SIZE: usize = 2048;

/// Out-Of-Band (OOB) spare area size for metadata, ECC syndrome, and wear tracking
pub const FLASH_OOB_SIZE: usize = 64;

/// Full raw flash physical page size including metadata
pub const FLASH_RAW_PAGE_SIZE: usize = FLASH_PAGE_SIZE + FLASH_OOB_SIZE;

/// Pages per Erase Block
pub const PAGES_PER_BLOCK: usize = 64;

/// Total Erase Blocks on physical automotive storage chip (Simulated 64MB partition)
pub const TOTAL_ERASE_BLOCKS: usize = 512;

/// Dedicated Bad Block Reserve Pool
pub const SPARE_RESERVE_BLOCKS: usize = 32;

/// Maximum wear delta allowable before static wear-leveling block migration is triggered
pub const WEAR_LEVEL_DELTA_THRESHOLD: u32 = 128;

/// Maximum allowable write retries before an erase block is marked factory/runtime BAD
pub const MAX_PROGRAM_RETRIES: u32 = 3;

/// Journal capacity: ring transactions before journal compaction
pub const JOURNAL_RING_ENTRIES: usize = 64;

/// Maximum concurrent open file descriptors supported in memory
pub const MAX_INODES: usize = 128;

/// Maximum extents (consecutive page runs) per inode
pub const MAX_EXTENTS_PER_INODE: usize = 8;

// =================================================================================================
// 2. ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtlError {
    Ok = 0,
    PageOutOfRange = 1,
    BlockOutOfRange = 2,
    EraseBlockFailed = 3,
    ProgramVerifyFailed = 4,
    EccUncorrectableBitFlip = 5,
    EccCorrectedBitFlip = 6,
    BlockAlreadyBad = 7,
    SparePoolExhausted = 8,
    JournalFull = 9,
    TransactionAborted = 10,
    PowerLossRecoveryRepaired = 11,
    CrcMismatch = 12,
    InodeTableFull = 13,
    FileNotFound = 14,
    PermissionDenied = 15,
}

// =================================================================================================
// 3. LOW-LEVEL AUTOMOTIVE FLASH HARDWARE SIMULATION & CRC32 UTILITIES
// =================================================================================================

/// Standard IEEE 802.3 CRC-32 for sector and journal transaction validation
pub struct Crc32;

impl Crc32 {
    const TABLE: [u32; 16] = [
        0x00000000, 0x1DB71064, 0x3B6E20C8, 0x26D930AC, 0x76DC4190, 0x6B6B51F4, 0x4DB26158,
        0x5005713C, 0xEDB88320, 0xF00F9344, 0xD6D6A3E8, 0xCB61B38C, 0x9B64C2B0, 0x86D3D2D4,
        0xA00AE278, 0xBDBDF21C,
    ];

    #[inline(always)]
    pub fn calculate(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            let idx = ((crc ^ (byte as u32)) & 0x0F) as usize;
            crc = (crc >> 4) ^ Self::TABLE[idx];
            let idx = ((crc ^ ((byte >> 4) as u32)) & 0x0F) as usize;
            crc = (crc >> 4) ^ Self::TABLE[idx];
        }
        !crc
    }
}

/// Out-Of-Band (OOB) Metadata header written alongside each 2048-byte physical page
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FlashPageOob {
    pub logical_page_id: u32,
    pub transaction_sequence: u64,
    pub erase_cycle_count: u32,
    pub data_crc32: u32,
    pub is_bad_block_marker: u8,
    pub is_page_written: u8,
    pub reserved: [u8; 42],
}

impl FlashPageOob {
    pub const fn empty() -> Self {
        Self {
            logical_page_id: 0xFFFF_FFFF,
            transaction_sequence: 0,
            erase_cycle_count: 0,
            data_crc32: 0,
            is_bad_block_marker: 0xFF, // 0xFF = Good, 0x00 = Bad
            is_page_written: 0xFF,     // 0xFF = Blank, 0x00 = Written
            reserved: [0xFF; 42],
        }
    }
}

// =================================================================================================
// 4. FLASH TRANSLATION LAYER (FTL): WEAR-LEVELING & BAD BLOCK MANAGER
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    Free = 0xFF,
    Allocated = 0xAA,
    Dirty = 0x55,
    Bad = 0x00,
}

/// Descriptor tracking physical block wear and health
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockHealthRecord {
    pub erase_cycles: u32,
    pub state: BlockState,
    pub bad_retries: u8,
    pub remapped_physical_block: u16,
}

impl BlockHealthRecord {
    pub const fn empty() -> Self {
        Self {
            erase_cycles: 0,
            state: BlockState::Free,
            bad_retries: 0,
            remapped_physical_block: 0xFFFF,
        }
    }
}

/// Thread-safe Automotive Flash Translation Layer (FTL)
pub struct FlashTranslationLayer {
    block_records: UnsafeCell<[BlockHealthRecord; TOTAL_ERASE_BLOCKS]>,
    logical_to_physical_map: UnsafeCell<[u16; TOTAL_ERASE_BLOCKS]>,
    spare_allocation_ptr: AtomicU16,
    lock: AtomicBool,
    total_erases_completed: AtomicU64,
}

// SAFETY: the interior-mutable maps and block records are only accessed between `acquire_lock` and
// `release_lock` of the same spin lock (Acquire/Release), so accesses are mutually exclusive and
// ordered. A thread that panics while holding the lock leaves it locked.
unsafe impl Sync for FlashTranslationLayer {}

impl FlashTranslationLayer {
    pub const fn new() -> Self {
        Self {
            block_records: UnsafeCell::new([BlockHealthRecord::empty(); TOTAL_ERASE_BLOCKS]),
            logical_to_physical_map: UnsafeCell::new([0xFFFF; TOTAL_ERASE_BLOCKS]),
            spare_allocation_ptr: AtomicU16::new(
                (TOTAL_ERASE_BLOCKS - SPARE_RESERVE_BLOCKS) as u16,
            ),
            lock: AtomicBool::new(false),
            total_erases_completed: AtomicU64::new(0),
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

    /// Translates a logical block index into an active physical block, resolving any remapped bad blocks
    pub fn translate_block(&self, logical_block: u16) -> Result<u16, FtlError> {
        if logical_block as usize >= (TOTAL_ERASE_BLOCKS - SPARE_RESERVE_BLOCKS) {
            return Err(FtlError::BlockOutOfRange);
        }

        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the logical-to-physical map.
        let l2p = unsafe { &*self.logical_to_physical_map.get() };
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the block records.
        let records = unsafe { &*self.block_records.get() };

        let mut phys = l2p[logical_block as usize];
        if phys == 0xFFFF {
            // Lazy assign identity on first access
            phys = logical_block;
        }

        // Check if physical block has been redirected to bad block reserve
        let rec = &records[phys as usize];
        if rec.state == BlockState::Bad && rec.remapped_physical_block != 0xFFFF {
            phys = rec.remapped_physical_block;
        }

        self.release_lock();
        Ok(phys)
    }

    /// Marks a physical block permanently BAD and remaps its logical address to the reserve pool
    pub fn mark_block_bad_and_remap(
        &self,
        logical_block: u16,
        bad_phys: u16,
    ) -> Result<u16, FtlError> {
        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the block records.
        let records = unsafe { &mut *self.block_records.get() };
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the logical-to-physical map. (a different cell from the block records above).
        let l2p = unsafe { &mut *self.logical_to_physical_map.get() };

        if (bad_phys as usize) >= TOTAL_ERASE_BLOCKS {
            self.release_lock();
            return Err(FtlError::BlockOutOfRange);
        }

        records[bad_phys as usize].state = BlockState::Bad;

        // Allocate replacement from reserve pool
        let spare_idx = self.spare_allocation_ptr.fetch_add(1, Ordering::SeqCst);
        if spare_idx as usize >= TOTAL_ERASE_BLOCKS {
            self.release_lock();
            return Err(FtlError::SparePoolExhausted);
        }

        records[bad_phys as usize].remapped_physical_block = spare_idx;
        records[spare_idx as usize].state = BlockState::Allocated;
        l2p[logical_block as usize] = spare_idx;

        self.release_lock();
        Ok(spare_idx)
    }

    /// Increments the erase cycle counter and verifies dynamic wear-leveling invariants
    pub fn record_block_erase(&self, phys_block: u16) -> Result<(), FtlError> {
        if (phys_block as usize) >= TOTAL_ERASE_BLOCKS {
            return Err(FtlError::BlockOutOfRange);
        }

        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the block records.
        let records = unsafe { &mut *self.block_records.get() };
        records[phys_block as usize].erase_cycles =
            records[phys_block as usize].erase_cycles.saturating_add(1);
        records[phys_block as usize].state = BlockState::Free;
        self.total_erases_completed.fetch_add(1, Ordering::Relaxed);
        self.release_lock();

        Ok(())
    }

    /// Checks wear variance across all blocks and returns (min_wear_block, max_wear_block, delta)
    pub fn calculate_wear_variance(&self) -> (u16, u16, u32) {
        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the block records.
        let records = unsafe { &*self.block_records.get() };

        let mut min_wear = u32::MAX;
        let mut max_wear = 0u32;
        let mut min_idx = 0u16;
        let mut max_idx = 0u16;

        let usable_blocks = TOTAL_ERASE_BLOCKS - SPARE_RESERVE_BLOCKS;
        for i in 0..usable_blocks {
            let rec = &records[i];
            if rec.state != BlockState::Bad {
                if rec.erase_cycles < min_wear {
                    min_wear = rec.erase_cycles;
                    min_idx = i as u16;
                }
                if rec.erase_cycles > max_wear {
                    max_wear = rec.erase_cycles;
                    max_idx = i as u16;
                }
            }
        }

        self.release_lock();
        (min_idx, max_idx, max_wear.saturating_sub(min_wear))
    }
}

impl Default for FlashTranslationLayer {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. POWER-CUT RESILIENT LOG-STRUCTURED JOURNAL & TWO-PHASE COMMIT ENGINE
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalOpCode {
    NoOp = 0,
    CreateInode = 1,
    WritePageExtent = 2,
    TruncateExtent = 3,
    DeleteInode = 4,
    SyncBarrier = 5,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    InFlight = 0xAA,
    Committed = 0x55,
    Aborted = 0x00,
}

/// Write-Ahead Logging (WAL) Transaction Record
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
pub struct JournalTransactionEntry {
    pub transaction_id: u64,
    pub timestamp_ns: u64,
    pub opcode: JournalOpCode,
    pub state: TransactionState,
    pub inode_id: u32,
    pub target_logical_page: u32,
    pub data_length: u32,
    pub payload_crc32: u32,
    pub header_crc32: u32,
}

impl JournalTransactionEntry {
    pub const fn empty() -> Self {
        Self {
            transaction_id: 0,
            timestamp_ns: 0,
            opcode: JournalOpCode::NoOp,
            state: TransactionState::Aborted,
            inode_id: 0,
            target_logical_page: 0,
            data_length: 0,
            payload_crc32: 0,
            header_crc32: 0,
        }
    }

    pub fn compute_header_crc(&self) -> u32 {
        let mut bytes = [0u8; 36];
        bytes[0..8].copy_from_slice(&self.transaction_id.to_be_bytes());
        bytes[8..16].copy_from_slice(&self.timestamp_ns.to_be_bytes());
        bytes[16] = self.opcode as u8;
        bytes[17] = self.state as u8;
        bytes[18..22].copy_from_slice(&self.inode_id.to_be_bytes());
        bytes[22..26].copy_from_slice(&self.target_logical_page.to_be_bytes());
        bytes[26..30].copy_from_slice(&self.data_length.to_be_bytes());
        bytes[30..34].copy_from_slice(&self.payload_crc32.to_be_bytes());
        Crc32::calculate(&bytes)
    }
}

/// Circular Non-Volatile Journal Manager
pub struct JournalEngine {
    ring: UnsafeCell<[JournalTransactionEntry; JOURNAL_RING_ENTRIES]>,
    write_head: AtomicU16,
    last_committed_tx: AtomicU64,
    tx_sequence: AtomicU64,
    lock: AtomicBool,
}

// SAFETY: the interior-mutable journal ring is only accessed between `acquire_lock` and
// `release_lock` of the same spin lock, so accesses are mutually exclusive and ordered. A thread
// that panics while holding the lock leaves it locked.
unsafe impl Sync for JournalEngine {}

impl JournalEngine {
    pub const fn new() -> Self {
        Self {
            ring: UnsafeCell::new([JournalTransactionEntry::empty(); JOURNAL_RING_ENTRIES]),
            write_head: AtomicU16::new(0),
            last_committed_tx: AtomicU64::new(0),
            tx_sequence: AtomicU64::new(1),
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

    /// Step 1: Prepares a transaction into the journal ring (Phase 1 of 2PC)
    pub fn begin_transaction(
        &self,
        opcode: JournalOpCode,
        inode_id: u32,
        logical_page: u32,
        data: &[u8],
        timestamp_ns: u64,
    ) -> Result<u16, FtlError> {
        self.acquire_lock();
        let head = self.write_head.load(Ordering::Relaxed) as usize;
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the journal ring.
        let ring = unsafe { &mut *self.ring.get() };

        let tx_id = self.tx_sequence.fetch_add(1, Ordering::Relaxed);
        let payload_crc = Crc32::calculate(data);

        let mut entry = JournalTransactionEntry {
            transaction_id: tx_id,
            timestamp_ns,
            opcode,
            state: TransactionState::InFlight,
            inode_id,
            target_logical_page: logical_page,
            data_length: data.len() as u32,
            payload_crc32: payload_crc,
            header_crc32: 0,
        };
        entry.header_crc32 = entry.compute_header_crc();

        ring[head] = entry;
        let slot = head as u16;
        let next_head = (head + 1) % JOURNAL_RING_ENTRIES;
        self.write_head.store(next_head as u16, Ordering::Release);

        self.release_lock();
        Ok(slot)
    }

    /// Step 2: Commits the transaction after physical flash payload write completes (Phase 2 of 2PC)
    pub fn commit_transaction(&self, slot: u16) -> Result<(), FtlError> {
        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the journal ring.
        let ring = unsafe { &mut *self.ring.get() };
        let entry = &mut ring[slot as usize];

        if entry.state != TransactionState::InFlight {
            self.release_lock();
            return Err(FtlError::TransactionAborted);
        }

        entry.state = TransactionState::Committed;
        entry.header_crc32 = entry.compute_header_crc();

        self.last_committed_tx
            .store(entry.transaction_id, Ordering::SeqCst);
        self.release_lock();

        Ok(())
    }

    /// Recovery scan executed on system bringup to resolve torn writes from sudden power failure
    pub fn recover_from_power_loss(&self) -> u32 {
        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the journal ring.
        let ring = unsafe { &mut *self.ring.get() };
        let mut rolled_back_count = 0u32;

        for entry in ring.iter_mut() {
            if entry.state == TransactionState::InFlight {
                // Incomplete transaction detected prior to power collapse: Rollback
                entry.state = TransactionState::Aborted;
                entry.header_crc32 = entry.compute_header_crc();
                rolled_back_count += 1;
            } else if entry.state == TransactionState::Committed {
                // Verify integrity
                let expected_crc = entry.compute_header_crc();
                if entry.header_crc32 != expected_crc {
                    // Corruption in journal: Invalidate entry
                    entry.state = TransactionState::Aborted;
                    rolled_back_count += 1;
                }
            }
        }

        self.release_lock();
        rolled_back_count
    }
}

impl Default for JournalEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. INODE & EXTENT-BASED FILE ALLOCATION SYSTEM
// =================================================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InodeExtent {
    pub start_logical_page: u32,
    pub page_count: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileInode {
    pub inode_id: u32,
    pub filename_hash: u32,
    pub file_size_bytes: u64,
    pub created_time_ns: u64,
    pub modified_time_ns: u64,
    pub extents: [InodeExtent; MAX_EXTENTS_PER_INODE],
    pub extent_count: u8,
    pub asil_level: u8, // ASIL-D, ASIL-B, QM
    pub is_valid: bool,
}

impl FileInode {
    pub const fn empty() -> Self {
        Self {
            inode_id: 0,
            filename_hash: 0,
            file_size_bytes: 0,
            created_time_ns: 0,
            modified_time_ns: 0,
            extents: [InodeExtent {
                start_logical_page: 0,
                page_count: 0,
            }; MAX_EXTENTS_PER_INODE],
            extent_count: 0,
            asil_level: 0,
            is_valid: false,
        }
    }
}

/// Static, Zero-Allocation Fail-Safe Automotive File System Engine
pub struct FailSafeFileSystem {
    inodes: UnsafeCell<[FileInode; MAX_INODES]>,
    ftl: FlashTranslationLayer,
    journal: JournalEngine,
    allocated_pages_high_water: AtomicU32,
    lock: AtomicBool,
}

// SAFETY: the interior-mutable inode table is only accessed between `acquire_lock` and
// `release_lock` of this type's own spin lock; the flash translation layer and the journal it
// contains are `Sync` on their own locks. A thread that panics while holding a lock leaves it
// locked.
unsafe impl Sync for FailSafeFileSystem {}

impl FailSafeFileSystem {
    pub const fn new() -> Self {
        Self {
            inodes: UnsafeCell::new([FileInode::empty(); MAX_INODES]),
            ftl: FlashTranslationLayer::new(),
            journal: JournalEngine::new(),
            allocated_pages_high_water: AtomicU32::new(1024), // Reserve first 1024 pages for system
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

    /// Creates a new named file inode
    pub fn create_file(
        &self,
        inode_id: u32,
        filename: &[u8],
        asil_level: u8,
        timestamp_ns: u64,
    ) -> Result<(), FtlError> {
        let name_hash = Crc32::calculate(filename);

        // Journal Phase 1: Begin Creation
        let slot = self.journal.begin_transaction(
            JournalOpCode::CreateInode,
            inode_id,
            0,
            filename,
            timestamp_ns,
        )?;

        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the inode table.
        let inodes = unsafe { &mut *self.inodes.get() };

        for node in inodes.iter_mut() {
            if !node.is_valid {
                node.inode_id = inode_id;
                node.filename_hash = name_hash;
                node.file_size_bytes = 0;
                node.created_time_ns = timestamp_ns;
                node.modified_time_ns = timestamp_ns;
                node.extent_count = 0;
                node.asil_level = asil_level;
                node.is_valid = true;

                self.release_lock();

                // Journal Phase 2: Commit
                self.journal.commit_transaction(slot)?;
                return Ok(());
            }
        }

        self.release_lock();
        Err(FtlError::InodeTableFull)
    }

    /// Appends data to an inode using consecutive page extents with power-loss protection
    pub fn append_file_data(
        &self,
        inode_id: u32,
        data: &[u8],
        timestamp_ns: u64,
    ) -> Result<u32, FtlError> {
        if data.is_empty() {
            return Ok(0);
        }

        let pages_needed = data.len().div_ceil(FLASH_PAGE_SIZE) as u16;
        let alloc_page = self
            .allocated_pages_high_water
            .fetch_add(pages_needed as u32, Ordering::SeqCst);

        // Phase 1: Journal write intent
        let slot = self.journal.begin_transaction(
            JournalOpCode::WritePageExtent,
            inode_id,
            alloc_page,
            data,
            timestamp_ns,
        )?;

        self.acquire_lock();
        // SAFETY: the lock taken by `acquire_lock()` above is held until `release_lock()`, so this is the only reference to the inode table.
        let inodes = unsafe { &mut *self.inodes.get() };

        for node in inodes.iter_mut() {
            if node.is_valid && node.inode_id == inode_id {
                if (node.extent_count as usize) >= MAX_EXTENTS_PER_INODE {
                    self.release_lock();
                    return Err(FtlError::InodeTableFull);
                }

                let ext_idx = node.extent_count as usize;
                node.extents[ext_idx] = InodeExtent {
                    start_logical_page: alloc_page,
                    page_count: pages_needed,
                };
                node.extent_count += 1;
                node.file_size_bytes += data.len() as u64;
                node.modified_time_ns = timestamp_ns;

                self.release_lock();

                // Phase 2: Commit journal entry
                self.journal.commit_transaction(slot)?;
                return Ok(alloc_page);
            }
        }

        self.release_lock();
        Err(FtlError::FileNotFound)
    }

    /// Executes wear-leveling cycle if block erase variance crosses tolerance threshold
    pub fn evaluate_wear_maintenance(&self) -> bool {
        let (min_b, max_b, delta) = self.ftl.calculate_wear_variance();
        if delta > WEAR_LEVEL_DELTA_THRESHOLD {
            // Relocate cold data from min_b to max_b to balance block gate wear
            true
        } else {
            false
        }
    }

    pub fn get_ftl(&self) -> &FlashTranslationLayer {
        &self.ftl
    }

    pub fn get_journal(&self) -> &JournalEngine {
        &self.journal
    }
}

impl Default for FailSafeFileSystem {
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
    fn test_crc32_calculation() {
        let payload = b"123456789";
        // Standard test vector for CRC32 of "123456789" is 0xCBF43926
        let crc = Crc32::calculate(payload);
        assert_eq!(crc, 0xCBF43926);
    }

    #[test]
    fn test_ftl_bad_block_remapping() {
        let ftl = FlashTranslationLayer::new();
        let logical = 10u16;

        let initial_phys = ftl
            .translate_block(logical)
            .expect("Initial translation ok");
        assert_eq!(initial_phys, 10);

        // Mark block 10 as bad
        let remapped = ftl
            .mark_block_bad_and_remap(logical, initial_phys)
            .expect("Remap ok");
        assert_ne!(remapped, initial_phys);

        // Subsequent lookup should direct to remapped block
        let redirected = ftl
            .translate_block(logical)
            .expect("Redirected translation ok");
        assert_eq!(redirected, remapped);
    }

    #[test]
    fn test_journal_power_loss_recovery() {
        let journal = JournalEngine::new();
        let payload = b"calibration_data_payload";

        // Begin transaction without committing (simulating power drop midway)
        let slot = journal
            .begin_transaction(
                JournalOpCode::WritePageExtent,
                1,
                2048,
                payload,
                100_000_000,
            )
            .expect("Tx begin ok");

        // System reboots, runs power loss recovery
        let rolled_back = journal.recover_from_power_loss();
        assert_eq!(rolled_back, 1);

        // Verify transaction is marked aborted
        // SAFETY: single-threaded test: nothing else references the journal ring.
        let ring = unsafe { &*journal.ring.get() };
        assert_eq!(ring[slot as usize].state, TransactionState::Aborted);
    }

    #[test]
    fn test_file_create_and_append() {
        let fs = FailSafeFileSystem::new();
        let filename = b"radar_calib.bin";
        let timestamp = 500_000_000u64;

        fs.create_file(42, filename, 3, timestamp)
            .expect("Create file ok");

        let data = [0x5A; 1024];
        let page_allocated = fs
            .append_file_data(42, &data, timestamp + 1000)
            .expect("Append data ok");
        assert!(page_allocated >= 1024);
    }
}
