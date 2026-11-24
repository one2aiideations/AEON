//! In-memory MBR and GPT partition-table parsing and editing.
//!
//! This module operates exclusively on caller-provided byte slices.
//! It supports 512-byte logical sectors, validates GPT header and partition
//! entry CRC32 checksums, detects malformed and overlapping partitions,
//! calculates aligned free space, and creates primary and backup GPT data.
//!
//! No filesystem access, external dependencies, or unsafe code are used.

use std::error::Error;
use std::fmt;

/// Logical sector size in bytes.
pub const SECTOR_SIZE: usize = 512;

/// Number of sectors required for the primary GPT region.
pub const GPT_PRIMARY_SECTORS: usize = 34;

/// Number of GPT partition entries supported.
pub const MAX_GPT_ENTRIES: u32 = 256;

/// Maximum supported GPT entry size in bytes.
pub const MAX_GPT_ENTRY_SIZE: u32 = 512;

/// Alignment in sectors corresponding to 1 MiB.
pub const PARTITION_ALIGNMENT: u64 = 2048;

/// EFI System Partition type GUID in GPT mixed-endian byte order.
pub const EFI_SYSTEM_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

/// Linux filesystem partition type GUID in GPT mixed-endian byte order.
pub const LINUX_FILESYSTEM_GUID: [u8; 16] = [
    0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4,
];

const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const MBR_SIGNATURE: [u8; 2] = [0x55, 0xaa];
const GPT_HEADER_MIN_SIZE: usize = 92;
const MBR_ENTRY_OFFSET: usize = 446;
const MBR_ENTRY_SIZE: usize = 16;

/// Errors returned by partition-table operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartError {
    /// The supplied buffer is too small.
    BufferTooSmall,
    /// The MBR signature is missing.
    InvalidMbrSignature,
    /// The GPT signature is missing.
    InvalidGptSignature,
    /// The protective MBR entry is missing.
    MissingProtectiveMbr,
    /// A GPT header contains invalid values.
    InvalidGptHeader,
    /// The GPT header checksum is invalid.
    HeaderCrcMismatch,
    /// The GPT partition-entry checksum is invalid.
    EntriesCrcMismatch,
    /// The GPT partition-entry array is outside the supplied buffer.
    EntriesOutOfBounds,
    /// The GPT partition-entry count or size is unsupported.
    InvalidEntryGeometry,
    /// A partition has invalid or out-of-range LBAs.
    InvalidPartitionRange,
    /// Two or more partitions overlap.
    OverlappingPartitions,
    /// No empty GPT partition-entry slot is available.
    NoFreeEntry,
    /// A partition type or unique GUID is invalid.
    InvalidGuid,
    /// The requested backup GPT location is invalid.
    InvalidBackupLocation,
    /// Arithmetic overflow occurred while calculating an offset or size.
    ArithmeticOverflow,
    /// A supplied partition name cannot be represented in the GPT name field.
    InvalidPartitionName,
}

impl fmt::Display for PartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::BufferTooSmall => "buffer is too small",
            Self::InvalidMbrSignature => "invalid MBR signature",
            Self::InvalidGptSignature => "invalid GPT signature",
            Self::MissingProtectiveMbr => "protective MBR entry is missing",
            Self::InvalidGptHeader => "invalid GPT header",
            Self::HeaderCrcMismatch => "GPT header CRC32 mismatch",
            Self::EntriesCrcMismatch => "GPT entries CRC32 mismatch",
            Self::EntriesOutOfBounds => "GPT entries are outside the supplied buffer",
            Self::InvalidEntryGeometry => "invalid GPT entry geometry",
            Self::InvalidPartitionRange => "invalid partition LBA range",
            Self::OverlappingPartitions => "overlapping partitions",
            Self::NoFreeEntry => "no free GPT entry slot",
            Self::InvalidGuid => "invalid partition GUID",
            Self::InvalidBackupLocation => "invalid backup GPT location",
            Self::ArithmeticOverflow => "partition-table arithmetic overflow",
            Self::InvalidPartitionName => "invalid partition name",
        };

        formatter.write_str(message)
    }
}

impl Error for PartError {}

/// An MBR partition entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbrEntry {
    /// Boot indicator: `0x80` for bootable, `0x00` otherwise.
    pub boot_flag: u8,
    /// MBR partition type.
    pub partition_type: u8,
    /// Starting LBA.
    pub start_lba: u32,
    /// Number of sectors.
    pub sector_count: u32,
}

/// A GPT partition entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptEntry {
    /// Partition type GUID.
    pub type_guid: [u8; 16],
    /// Unique partition GUID.
    pub unique_guid: [u8; 16],
    /// First partition LBA.
    pub first_lba: u64,
    /// Last partition LBA, inclusive.
    pub last_lba: u64,
    /// Partition attribute flags.
    pub attributes: u64,
    /// Partition name decoded from UTF-16LE.
    pub name: String,
}

/// Parsed GPT disk information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptInfo {
    /// GPT revision.
    pub revision: u32,
    /// Header size in bytes.
    pub header_size: u32,
    /// Current header LBA.
    pub my_lba: u64,
    /// Alternate GPT header LBA.
    pub alternate_lba: u64,
    /// First usable partition LBA.
    pub first_usable_lba: u64,
    /// Last usable partition LBA.
    pub last_usable_lba: u64,
    /// Disk GUID.
    pub disk_guid: [u8; 16],
    /// Partition-entry array starting LBA.
    pub entries_lba: u64,
    /// Number of partition-entry slots.
    pub entry_count: u32,
    /// Size of each partition entry.
    pub entry_size: u32,
    /// Parsed non-empty partition entries.
    pub entries: Vec<GptEntry>,
}

/// Detected partition-table format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Table {
    /// Legacy Master Boot Record.
    Mbr(Vec<MbrEntry>),
    /// GUID Partition Table.
    Gpt(GptInfo),
}

/// CRC32 IEEE lookup table, initialized at compile time.
const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut index = 0_usize;

    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;

        while bit < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xedb8_8320;
            } else {
                crc >>= 1;
            }

            bit += 1;
        }

        table[index] = crc;
        index += 1;
    }

    table
}

const CRC32_TABLE: [u32; 256] = build_crc32_table();

/// Calculates the IEEE reflected CRC32 checksum.
///
/// Uses polynomial `0xEDB88320`, initial value `0xFFFFFFFF`,
/// and final XOR `0xFFFFFFFF`.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;

    for byte in data {
        let index = ((crc ^ u32::from(*byte)) & 0xff) as usize;
        crc = CRC32_TABLE[index] ^ (crc >> 8);
    }

    crc ^ 0xffff_ffff
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, PartError> {
    let end = offset.checked_add(4).ok_or(PartError::ArithmeticOverflow)?;

    let bytes = data.get(offset..end).ok_or(PartError::BufferTooSmall)?;
    let array: [u8; 4] = bytes.try_into().map_err(|_| PartError::BufferTooSmall)?;

    Ok(u32::from_le_bytes(array))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64, PartError> {
    let end = offset.checked_add(8).ok_or(PartError::ArithmeticOverflow)?;

    let bytes = data.get(offset..end).ok_or(PartError::BufferTooSmall)?;
    let array: [u8; 8] = bytes.try_into().map_err(|_| PartError::BufferTooSmall)?;

    Ok(u64::from_le_bytes(array))
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) -> Result<(), PartError> {
    let end = offset.checked_add(4).ok_or(PartError::ArithmeticOverflow)?;

    let destination = data.get_mut(offset..end).ok_or(PartError::BufferTooSmall)?;

    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u64(data: &mut [u8], offset: usize, value: u64) -> Result<(), PartError> {
    let end = offset.checked_add(8).ok_or(PartError::ArithmeticOverflow)?;

    let destination = data.get_mut(offset..end).ok_or(PartError::BufferTooSmall)?;

    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_guid(data: &[u8], offset: usize) -> Result<[u8; 16], PartError> {
    let end = offset
        .checked_add(16)
        .ok_or(PartError::ArithmeticOverflow)?;

    let bytes = data.get(offset..end).ok_or(PartError::BufferTooSmall)?;
    let mut guid = [0_u8; 16];
    guid.copy_from_slice(bytes);

    Ok(guid)
}

fn write_guid(data: &mut [u8], offset: usize, guid: &[u8; 16]) -> Result<(), PartError> {
    let end = offset
        .checked_add(16)
        .ok_or(PartError::ArithmeticOverflow)?;

    let destination = data.get_mut(offset..end).ok_or(PartError::BufferTooSmall)?;

    destination.copy_from_slice(guid);
    Ok(())
}

fn checked_byte_offset(lba: u64) -> Result<usize, PartError> {
    let bytes = lba
        .checked_mul(u64::try_from(SECTOR_SIZE).map_err(|_| PartError::ArithmeticOverflow)?)
        .ok_or(PartError::ArithmeticOverflow)?;

    usize::try_from(bytes).map_err(|_| PartError::ArithmeticOverflow)
}

fn checked_entry_bytes(count: u32, size: u32) -> Result<usize, PartError> {
    let bytes = count
        .checked_mul(size)
        .ok_or(PartError::ArithmeticOverflow)?;

    usize::try_from(bytes).map_err(|_| PartError::ArithmeticOverflow)
}

fn parse_mbr(data: &[u8]) -> Result<Vec<MbrEntry>, PartError> {
    if data.len() < SECTOR_SIZE {
        return Err(PartError::BufferTooSmall);
    }

    if data.get(510..512) != Some(MBR_SIGNATURE.as_slice()) {
        return Err(PartError::InvalidMbrSignature);
    }

    let mut entries = Vec::new();

    for index in 0..4_usize {
        let offset = MBR_ENTRY_OFFSET
            .checked_add(
                index
                    .checked_mul(MBR_ENTRY_SIZE)
                    .ok_or(PartError::ArithmeticOverflow)?,
            )
            .ok_or(PartError::ArithmeticOverflow)?;

        let entry = data
            .get(offset..offset + MBR_ENTRY_SIZE)
            .ok_or(PartError::BufferTooSmall)?;

        let boot_flag = *entry.first().ok_or(PartError::BufferTooSmall)?;
        let partition_type = *entry.get(4).ok_or(PartError::BufferTooSmall)?;

        if partition_type == 0 {
            continue;
        }

        if boot_flag != 0 && boot_flag != 0x80 {
            return Err(PartError::InvalidPartitionRange);
        }

        let start_lba = u32::from_le_bytes(
            entry
                .get(8..12)
                .ok_or(PartError::BufferTooSmall)?
                .try_into()
                .map_err(|_| PartError::BufferTooSmall)?,
        );

        let sector_count = u32::from_le_bytes(
            entry
                .get(12..16)
                .ok_or(PartError::BufferTooSmall)?
                .try_into()
                .map_err(|_| PartError::BufferTooSmall)?,
        );

        if sector_count == 0 {
            return Err(PartError::InvalidPartitionRange);
        }

        start_lba
            .checked_add(sector_count)
            .ok_or(PartError::ArithmeticOverflow)?;

        entries.push(MbrEntry {
            boot_flag,
            partition_type,
            start_lba,
            sector_count,
        });
    }

    Ok(entries)
}

fn decode_gpt_name(bytes: &[u8]) -> String {
    let mut units = Vec::new();

    for chunk in bytes.as_chunks::<2>().0 {
        let unit = u16::from_le_bytes(*chunk);

        if unit == 0 {
            break;
        }

        units.push(unit);
    }

    String::from_utf16_lossy(&units)
}

fn parse_gpt(data: &[u8]) -> Result<GptInfo, PartError> {
    let minimum_size = GPT_PRIMARY_SECTORS
        .checked_mul(SECTOR_SIZE)
        .ok_or(PartError::ArithmeticOverflow)?;

    if data.len() < minimum_size {
        return Err(PartError::BufferTooSmall);
    }

    if data.get(SECTOR_SIZE..SECTOR_SIZE + 8) != Some(GPT_SIGNATURE.as_slice()) {
        return Err(PartError::InvalidGptSignature);
    }

    if data.get(510..512) != Some(MBR_SIGNATURE.as_slice()) {
        return Err(PartError::InvalidMbrSignature);
    }

    let protective_type = data.get(MBR_ENTRY_OFFSET + 4).copied();

    if protective_type != Some(0xee) {
        return Err(PartError::MissingProtectiveMbr);
    }

    let header = data
        .get(SECTOR_SIZE..SECTOR_SIZE * 2)
        .ok_or(PartError::BufferTooSmall)?;

    let revision = read_u32(header, 8)?;
    let header_size = read_u32(header, 12)?;
    let expected_header_crc = read_u32(header, 16)?;

    let header_size_usize =
        usize::try_from(header_size).map_err(|_| PartError::InvalidGptHeader)?;

    if !(GPT_HEADER_MIN_SIZE..=SECTOR_SIZE).contains(&header_size_usize) {
        return Err(PartError::InvalidGptHeader);
    }

    let mut header_copy = header
        .get(..header_size_usize)
        .ok_or(PartError::InvalidGptHeader)?
        .to_vec();

    let crc_field = header_copy
        .get_mut(16..20)
        .ok_or(PartError::InvalidGptHeader)?;

    crc_field.fill(0);

    if crc32(&header_copy) != expected_header_crc {
        return Err(PartError::HeaderCrcMismatch);
    }

    let my_lba = read_u64(header, 24)?;
    let alternate_lba = read_u64(header, 32)?;
    let first_usable_lba = read_u64(header, 40)?;
    let last_usable_lba = read_u64(header, 48)?;
    let disk_guid = read_guid(header, 56)?;
    let entries_lba = read_u64(header, 72)?;
    let entry_count = read_u32(header, 80)?;
    let entry_size = read_u32(header, 84)?;
    let expected_entries_crc = read_u32(header, 88)?;

    if revision < 0x0001_0000
        || my_lba != 1
        || alternate_lba <= my_lba
        || first_usable_lba > last_usable_lba
        || entries_lba < 2
        || entry_count == 0
        || entry_count > MAX_GPT_ENTRIES
        || !(128..=MAX_GPT_ENTRY_SIZE).contains(&entry_size)
        || !entry_size.is_multiple_of(8)
    {
        return Err(PartError::InvalidGptHeader);
    }

    let entries_length = checked_entry_bytes(entry_count, entry_size)?;
    let entries_offset = checked_byte_offset(entries_lba)?;
    let entries_end = entries_offset
        .checked_add(entries_length)
        .ok_or(PartError::ArithmeticOverflow)?;

    if entries_end > data.len() {
        return Err(PartError::EntriesOutOfBounds);
    }

    let entries_data = data
        .get(entries_offset..entries_end)
        .ok_or(PartError::EntriesOutOfBounds)?;

    if crc32(entries_data) != expected_entries_crc {
        return Err(PartError::EntriesCrcMismatch);
    }

    let entry_size_usize =
        usize::try_from(entry_size).map_err(|_| PartError::InvalidEntryGeometry)?;

    let mut entries = Vec::new();

    for index in 0..entry_count {
        let offset = usize::try_from(index)
            .map_err(|_| PartError::ArithmeticOverflow)?
            .checked_mul(entry_size_usize)
            .ok_or(PartError::ArithmeticOverflow)?;

        let entry_end = offset
            .checked_add(entry_size_usize)
            .ok_or(PartError::ArithmeticOverflow)?;

        let raw = entries_data
            .get(offset..entry_end)
            .ok_or(PartError::EntriesOutOfBounds)?;

        let type_guid = read_guid(raw, 0)?;

        if type_guid.iter().all(|byte| *byte == 0) {
            continue;
        }

        let unique_guid = read_guid(raw, 16)?;
        let first_lba = read_u64(raw, 32)?;
        let last_lba = read_u64(raw, 40)?;
        let attributes = read_u64(raw, 48)?;

        if first_lba > last_lba || first_lba < first_usable_lba || last_lba > last_usable_lba {
            return Err(PartError::InvalidPartitionRange);
        }

        let name_bytes = raw.get(56..128).ok_or(PartError::InvalidEntryGeometry)?;

        entries.push(GptEntry {
            type_guid,
            unique_guid,
            first_lba,
            last_lba,
            attributes,
            name: decode_gpt_name(name_bytes),
        });
    }

    let mut ranges: Vec<(u64, u64)> = entries
        .iter()
        .map(|entry| (entry.first_lba, entry.last_lba))
        .collect();

    ranges.sort_unstable();

    for pair in ranges.windows(2) {
        let first = pair.first().ok_or(PartError::InvalidPartitionRange)?;
        let second = pair.get(1).ok_or(PartError::InvalidPartitionRange)?;

        if second.0 <= first.1 {
            return Err(PartError::OverlappingPartitions);
        }
    }

    Ok(GptInfo {
        revision,
        header_size,
        my_lba,
        alternate_lba,
        first_usable_lba,
        last_usable_lba,
        disk_guid,
        entries_lba,
        entry_count,
        entry_size,
        entries,
    })
}

/// Detects and parses an MBR or primary GPT partition table.
///
/// GPT detection requires the primary 34-sector region. The protective
/// MBR and `EFI PART` header signature must both be present.
/// Otherwise, a valid MBR is parsed.
pub fn detect(first_sectors: &[u8]) -> Result<Table, PartError> {
    if first_sectors.len() >= SECTOR_SIZE * 2
        && first_sectors.get(SECTOR_SIZE..SECTOR_SIZE + 8) == Some(GPT_SIGNATURE.as_slice())
    {
        return parse_gpt(first_sectors).map(Table::Gpt);
    }

    parse_mbr(first_sectors).map(Table::Mbr)
}

/// Aligns an LBA upward to the next 1 MiB boundary.
fn align_up(lba: u64) -> Option<u64> {
    let remainder = lba % PARTITION_ALIGNMENT;

    if remainder == 0 {
        Some(lba)
    } else {
        lba.checked_add(PARTITION_ALIGNMENT - remainder)
    }
}

fn find_largest_gap(mut ranges: Vec<(u64, u64)>, start: u64, end: u64) -> Option<(u64, u64)> {
    if start > end {
        return None;
    }

    ranges.sort_unstable();

    let mut cursor = start;
    let mut best: Option<(u64, u64)> = None;

    for (partition_start, partition_end) in ranges {
        if partition_end < cursor {
            continue;
        }

        if partition_start > end {
            break;
        }

        if partition_start > cursor {
            let gap_start = align_up(cursor)?;
            let gap_end = partition_start.min(end.saturating_add(1));

            if gap_start < gap_end {
                let length = gap_end.checked_sub(gap_start)?;

                if length >= PARTITION_ALIGNMENT
                    && best.is_none_or(|(_, best_length)| length > best_length)
                {
                    best = Some((gap_start, length));
                }
            }
        }

        cursor = cursor.max(partition_end.checked_add(1)?);

        if cursor > end {
            break;
        }
    }

    if cursor <= end {
        let gap_start = align_up(cursor)?;
        let gap_end = end.checked_add(1)?;

        if gap_start < gap_end {
            let length = gap_end.checked_sub(gap_start)?;

            if length >= PARTITION_ALIGNMENT
                && best.is_none_or(|(_, best_length)| length > best_length)
            {
                best = Some((gap_start, length));
            }
        }
    }

    best
}

/// Finds the largest unused, 1 MiB-aligned partition region.
///
/// Returns `(start_lba, sector_count)`. Gaps shorter than 2048 sectors
/// are ignored.
///
/// For GPT, searches only the usable LBA range. For MBR, searches from
/// LBA 2048 to the final disk sector.
#[must_use]
pub fn largest_free_gap(table: &Table, disk_sectors: u64) -> Option<(u64, u64)> {
    if disk_sectors == 0 {
        return None;
    }

    match table {
        Table::Mbr(entries) => {
            let ranges = entries
                .iter()
                .map(|entry| {
                    let end = u64::from(entry.start_lba)
                        .checked_add(u64::from(entry.sector_count))?
                        .checked_sub(1)?;

                    Some((u64::from(entry.start_lba), end))
                })
                .collect::<Option<Vec<_>>>()?;

            find_largest_gap(ranges, PARTITION_ALIGNMENT, disk_sectors.checked_sub(1)?)
        }
        Table::Gpt(info) => {
            let ranges = info
                .entries
                .iter()
                .map(|entry| (entry.first_lba, entry.last_lba))
                .collect();

            find_largest_gap(ranges, info.first_usable_lba, info.last_usable_lba)
        }
    }
}

/// Recomputes both CRC fields in a primary GPT region.
fn update_gpt_crcs(data: &mut [u8]) -> Result<(), PartError> {
    let header = data
        .get(SECTOR_SIZE..SECTOR_SIZE * 2)
        .ok_or(PartError::BufferTooSmall)?;

    let entry_lba = read_u64(header, 72)?;
    let entry_count = read_u32(header, 80)?;
    let entry_size = read_u32(header, 84)?;

    let entries_length = checked_entry_bytes(entry_count, entry_size)?;
    let entries_offset = checked_byte_offset(entry_lba)?;
    let entries_end = entries_offset
        .checked_add(entries_length)
        .ok_or(PartError::ArithmeticOverflow)?;

    let entries = data
        .get(entries_offset..entries_end)
        .ok_or(PartError::EntriesOutOfBounds)?;

    let entries_crc = crc32(entries);

    let header = data
        .get_mut(SECTOR_SIZE..SECTOR_SIZE * 2)
        .ok_or(PartError::BufferTooSmall)?;

    write_u32(header, 88, entries_crc)?;
    write_u32(header, 16, 0)?;

    let header_size =
        usize::try_from(read_u32(header, 12)?).map_err(|_| PartError::InvalidGptHeader)?;

    if !(GPT_HEADER_MIN_SIZE..=SECTOR_SIZE).contains(&header_size) {
        return Err(PartError::InvalidGptHeader);
    }

    let checksum_data = header
        .get(..header_size)
        .ok_or(PartError::InvalidGptHeader)?;

    let header_crc = crc32(checksum_data);
    write_u32(header, 16, header_crc)?;

    Ok(())
}

/// Adds a GPT partition to the primary GPT region.
///
/// `sectors` must contain the primary 34-sector region, beginning at LBA 0.
/// The first empty entry slot is selected. Both GPT CRC fields are
/// recomputed after the modification.
///
/// The supplied partition must fit within the usable LBA range and must
/// not overlap an existing partition. The backup GPT is not modified;
/// use [`backup_gpt`] to generate its contents.
pub fn add_gpt_entry(
    sectors: &mut [u8],
    disk_sectors: u64,
    first_lba: u64,
    last_lba: u64,
    type_guid: [u8; 16],
    unique_guid: [u8; 16],
    name: &str,
) -> Result<(), PartError> {
    if sectors.len() < GPT_PRIMARY_SECTORS * SECTOR_SIZE {
        return Err(PartError::BufferTooSmall);
    }

    if type_guid.iter().all(|byte| *byte == 0) || unique_guid.iter().all(|byte| *byte == 0) {
        return Err(PartError::InvalidGuid);
    }

    if first_lba > last_lba {
        return Err(PartError::InvalidPartitionRange);
    }

    let info = match detect(sectors)? {
        Table::Gpt(info) => info,
        Table::Mbr(_) => return Err(PartError::InvalidGptSignature),
    };

    if info.alternate_lba
        != disk_sectors
            .checked_sub(1)
            .ok_or(PartError::InvalidBackupLocation)?
        || first_lba < info.first_usable_lba
        || last_lba > info.last_usable_lba
    {
        return Err(PartError::InvalidPartitionRange);
    }

    if info
        .entries
        .iter()
        .any(|entry| entry.first_lba <= last_lba && first_lba <= entry.last_lba)
    {
        return Err(PartError::OverlappingPartitions);
    }

    if info
        .entries
        .iter()
        .any(|entry| entry.unique_guid == unique_guid)
    {
        return Err(PartError::InvalidGuid);
    }

    let encoded_name: Vec<u16> = name.encode_utf16().collect();

    let name_capacity = usize::try_from(info.entry_size)
        .map_err(|_| PartError::InvalidEntryGeometry)?
        .checked_sub(56)
        .ok_or(PartError::InvalidEntryGeometry)?
        / 2;

    if encoded_name.len() > name_capacity {
        return Err(PartError::InvalidPartitionName);
    }

    let entries_offset = checked_byte_offset(info.entries_lba)?;
    let entry_size =
        usize::try_from(info.entry_size).map_err(|_| PartError::InvalidEntryGeometry)?;

    let entries_length = checked_entry_bytes(info.entry_count, info.entry_size)?;
    let entries_end = entries_offset
        .checked_add(entries_length)
        .ok_or(PartError::ArithmeticOverflow)?;

    let entries = sectors
        .get(entries_offset..entries_end)
        .ok_or(PartError::EntriesOutOfBounds)?;

    let mut empty_slot = None;

    for index in 0..info.entry_count {
        let offset = usize::try_from(index)
            .map_err(|_| PartError::ArithmeticOverflow)?
            .checked_mul(entry_size)
            .ok_or(PartError::ArithmeticOverflow)?;

        let slot_end = offset
            .checked_add(entry_size)
            .ok_or(PartError::ArithmeticOverflow)?;

        let slot = entries
            .get(offset..slot_end)
            .ok_or(PartError::EntriesOutOfBounds)?;

        let slot_type = read_guid(slot, 0)?;

        if slot_type.iter().all(|byte| *byte == 0) {
            empty_slot = Some(offset);
            break;
        }
    }

    let slot_offset = empty_slot.ok_or(PartError::NoFreeEntry)?;

    let absolute_offset = entries_offset
        .checked_add(slot_offset)
        .ok_or(PartError::ArithmeticOverflow)?;

    let absolute_end = absolute_offset
        .checked_add(entry_size)
        .ok_or(PartError::ArithmeticOverflow)?;

    let slot = sectors
        .get_mut(absolute_offset..absolute_end)
        .ok_or(PartError::EntriesOutOfBounds)?;

    slot.fill(0);

    write_guid(slot, 0, &type_guid)?;
    write_guid(slot, 16, &unique_guid)?;
    write_u64(slot, 32, first_lba)?;
    write_u64(slot, 40, last_lba)?;

    for (index, unit) in encoded_name.iter().enumerate() {
        let offset = 56_usize
            .checked_add(index.checked_mul(2).ok_or(PartError::ArithmeticOverflow)?)
            .ok_or(PartError::ArithmeticOverflow)?;

        let end = offset.checked_add(2).ok_or(PartError::ArithmeticOverflow)?;

        let destination = slot
            .get_mut(offset..end)
            .ok_or(PartError::InvalidPartitionName)?;

        destination.copy_from_slice(&unit.to_le_bytes());
    }

    update_gpt_crcs(sectors)
}

/// Generates backup GPT partition entries and a backup GPT header.
///
/// `primary` must contain the complete primary 34-sector region.
/// The returned tuple contains:
///
/// 1. The backup partition-entry array, with the same size and contents
///    as the primary array.
/// 2. A 512-byte backup header sector.
///
/// The backup header has its current and alternate LBAs swapped, and its
/// partition-entry array begins at `disk_sectors - 33`.
pub fn backup_gpt(primary: &[u8], disk_sectors: u64) -> Result<(Vec<u8>, Vec<u8>), PartError> {
    let info = match detect(primary)? {
        Table::Gpt(info) => info,
        Table::Mbr(_) => return Err(PartError::InvalidGptSignature),
    };

    if disk_sectors < 34
        || info.alternate_lba
            != disk_sectors
                .checked_sub(1)
                .ok_or(PartError::InvalidBackupLocation)?
    {
        return Err(PartError::InvalidBackupLocation);
    }

    let backup_entries_lba = disk_sectors
        .checked_sub(33)
        .ok_or(PartError::InvalidBackupLocation)?;

    if info.last_usable_lba >= backup_entries_lba || info.first_usable_lba <= 1 || info.my_lba != 1
    {
        return Err(PartError::InvalidBackupLocation);
    }

    let entries_length = checked_entry_bytes(info.entry_count, info.entry_size)?;
    let entries_offset = checked_byte_offset(info.entries_lba)?;
    let entries_end = entries_offset
        .checked_add(entries_length)
        .ok_or(PartError::ArithmeticOverflow)?;

    let entries = primary
        .get(entries_offset..entries_end)
        .ok_or(PartError::EntriesOutOfBounds)?
        .to_vec();

    let mut header = primary
        .get(SECTOR_SIZE..SECTOR_SIZE * 2)
        .ok_or(PartError::BufferTooSmall)?
        .to_vec();

    write_u64(&mut header, 24, info.alternate_lba)?;
    write_u64(&mut header, 32, info.my_lba)?;
    write_u64(&mut header, 72, backup_entries_lba)?;
    write_u32(&mut header, 88, crc32(&entries))?;
    write_u32(&mut header, 16, 0)?;

    let header_size =
        usize::try_from(read_u32(&header, 12)?).map_err(|_| PartError::InvalidGptHeader)?;

    if !(GPT_HEADER_MIN_SIZE..=SECTOR_SIZE).contains(&header_size) {
        return Err(PartError::InvalidGptHeader);
    }

    let header_crc = crc32(
        header
            .get(..header_size)
            .ok_or(PartError::InvalidGptHeader)?,
    );

    write_u32(&mut header, 16, header_crc)?;

    Ok((entries, header))
}

/// Parses a canonical GUID string into GPT mixed-endian bytes.
///
/// The accepted format is `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
#[must_use]
pub fn guid_from_str(value: &str) -> Option<[u8; 16]> {
    let compact: String = value.chars().filter(|ch| *ch != '-').collect();

    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    let mut canonical = [0_u8; 16];

    for (index, byte) in canonical.iter_mut().enumerate() {
        let offset = index.checked_mul(2)?;
        let end = offset.checked_add(2)?;
        *byte = u8::from_str_radix(compact.get(offset..end)?, 16).ok()?;
    }

    let mut guid = canonical;
    guid.get_mut(0..4)?.reverse();
    guid.get_mut(4..6)?.reverse();
    guid.get_mut(6..8)?.reverse();

    Some(guid)
}

/// Formats GPT mixed-endian GUID bytes as a canonical GUID string.
#[must_use]
pub fn guid_to_string(guid: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid[3],
        guid[2],
        guid[1],
        guid[0],
        guid[5],
        guid[4],
        guid[7],
        guid[6],
        guid[8],
        guid[9],
        guid[10],
        guid[11],
        guid[12],
        guid[13],
        guid[14],
        guid[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK_SECTORS: u64 = 100_000;
    const ENTRY_COUNT: u32 = 128;
    const ENTRY_SIZE: u32 = 128;

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        if let Some(destination) = data.get_mut(offset..offset + 4) {
            destination.copy_from_slice(&value.to_le_bytes());
        }
    }

    fn put_u64(data: &mut [u8], offset: usize, value: u64) {
        if let Some(destination) = data.get_mut(offset..offset + 8) {
            destination.copy_from_slice(&value.to_le_bytes());
        }
    }

    fn put_guid(data: &mut [u8], offset: usize, value: &[u8; 16]) {
        if let Some(destination) = data.get_mut(offset..offset + 16) {
            destination.copy_from_slice(value);
        }
    }

    /// Recomputes the header CRC (and optionally the entries CRC) after a test edit.
    fn refresh_crcs(data: &mut [u8], entries_too: bool) {
        if entries_too {
            let entries_crc = crc32(
                data.get(2 * SECTOR_SIZE..2 * SECTOR_SIZE + 128 * 128)
                    .unwrap_or_default(),
            );
            put_u32(data, SECTOR_SIZE + 88, entries_crc);
        }

        let header_size =
            usize::try_from(read_u32(data, SECTOR_SIZE + 12).unwrap_or(92)).unwrap_or(92);
        let header_size = header_size.clamp(GPT_HEADER_MIN_SIZE, SECTOR_SIZE);

        let mut header = data
            .get(SECTOR_SIZE..SECTOR_SIZE + header_size)
            .unwrap_or_default()
            .to_vec();

        if let Some(field) = header.get_mut(16..20) {
            field.fill(0);
        }

        let checksum = crc32(&header);
        put_u32(data, SECTOR_SIZE + 16, checksum);
    }

    fn make_gpt() -> Vec<u8> {
        let mut data = vec![0_u8; GPT_PRIMARY_SECTORS * SECTOR_SIZE];

        data[510] = 0x55;
        data[511] = 0xaa;

        data[MBR_ENTRY_OFFSET + 4] = 0xee;
        data[MBR_ENTRY_OFFSET + 8..MBR_ENTRY_OFFSET + 12].copy_from_slice(&1_u32.to_le_bytes());
        data[MBR_ENTRY_OFFSET + 12..MBR_ENTRY_OFFSET + 16]
            .copy_from_slice(&99_999_u32.to_le_bytes());

        let header_offset = SECTOR_SIZE;

        data[header_offset..header_offset + 8].copy_from_slice(GPT_SIGNATURE);

        put_u32(&mut data, header_offset + 8, 0x0001_0000);
        put_u32(&mut data, header_offset + 12, 92);
        put_u64(&mut data, header_offset + 24, 1);
        put_u64(&mut data, header_offset + 32, DISK_SECTORS - 1);
        put_u64(&mut data, header_offset + 40, 2048);
        put_u64(&mut data, header_offset + 48, DISK_SECTORS - 34);

        put_guid(
            &mut data,
            header_offset + 56,
            &guid_from_str("12345678-1234-5678-9abc-def012345678").unwrap_or([0; 16]),
        );

        put_u64(&mut data, header_offset + 72, 2);
        put_u32(&mut data, header_offset + 80, ENTRY_COUNT);
        put_u32(&mut data, header_offset + 84, ENTRY_SIZE);

        let entries_offset = 2 * SECTOR_SIZE;

        for (index, (first, last, unique)) in [
            (2048_u64, 4095_u64, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
            (4096_u64, 8191_u64, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
        ]
        .iter()
        .enumerate()
        {
            let offset = entries_offset + index * 128;

            put_guid(&mut data, offset, &LINUX_FILESYSTEM_GUID);

            let guid = guid_from_str(unique).unwrap_or([0; 16]);
            put_guid(&mut data, offset + 16, &guid);

            put_u64(&mut data, offset + 32, *first);
            put_u64(&mut data, offset + 40, *last);

            let name = format!("Linux{}", index + 1);

            for (unit_index, unit) in name.encode_utf16().enumerate() {
                let name_offset = offset + 56 + unit_index * 2;
                data[name_offset..name_offset + 2].copy_from_slice(&unit.to_le_bytes());
            }
        }

        refresh_crcs(&mut data, true);

        data
    }

    fn valid_mbr() -> Vec<u8> {
        let mut data = vec![0_u8; SECTOR_SIZE];

        data[510] = 0x55;
        data[511] = 0xaa;

        for index in 0..4_usize {
            let offset = MBR_ENTRY_OFFSET + index * MBR_ENTRY_SIZE;

            data[offset] = if index == 0 { 0x80 } else { 0 };
            data[offset + 4] = u8::try_from(index + 1).unwrap_or(0);

            data[offset + 8..offset + 12]
                .copy_from_slice(&(2048_u32 * u32::try_from(index + 1).unwrap_or(0)).to_le_bytes());

            data[offset + 12..offset + 16].copy_from_slice(&2048_u32.to_le_bytes());
        }

        data
    }

    #[test]
    fn crc32_known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414f_a339
        );
    }

    #[test]
    fn detect_valid_gpt() {
        let data = make_gpt();

        let table = detect(&data);

        assert!(table.is_ok());

        let Ok(Table::Gpt(info)) = table else {
            return;
        };

        assert_eq!(info.my_lba, 1);
        assert_eq!(info.alternate_lba, DISK_SECTORS - 1);
        assert_eq!(info.first_usable_lba, 2048);
        assert_eq!(info.entries.len(), 2);
        assert_eq!(info.entries[0].name, "Linux1");
        assert_eq!(info.entries[1].name, "Linux2");
    }

    #[test]
    fn detect_valid_mbr_with_four_entries() {
        let data = valid_mbr();

        let table = detect(&data);

        assert!(matches!(table, Ok(Table::Mbr(entries)) if entries.len() == 4));
    }

    #[test]
    fn detect_rejects_bad_mbr_signature() {
        let mut data = valid_mbr();

        data[510] = 0;
        data[511] = 0;

        assert_eq!(detect(&data), Err(PartError::InvalidMbrSignature));
    }

    #[test]
    fn detect_rejects_truncated_buffers() {
        assert_eq!(detect(&[]), Err(PartError::BufferTooSmall));

        assert_eq!(
            detect(&vec![0; SECTOR_SIZE - 1]),
            Err(PartError::BufferTooSmall)
        );

        let data = make_gpt();

        assert_eq!(
            detect(data.get(..SECTOR_SIZE * 33).unwrap_or_default()),
            Err(PartError::BufferTooSmall)
        );
    }

    #[test]
    fn detect_rejects_corrupted_header_crc() {
        let mut data = make_gpt();

        data[SECTOR_SIZE + 16] ^= 0x01;

        assert_eq!(detect(&data), Err(PartError::HeaderCrcMismatch));
    }

    #[test]
    fn detect_rejects_corrupted_entries_crc() {
        let mut data = make_gpt();

        data[SECTOR_SIZE + 88] ^= 0x01;
        refresh_crcs(&mut data, false);

        assert_eq!(detect(&data), Err(PartError::EntriesCrcMismatch));
    }

    #[test]
    fn detect_rejects_corrupted_entry_data() {
        let mut data = make_gpt();

        data[2 * SECTOR_SIZE + 32] ^= 0x01;

        assert_eq!(detect(&data), Err(PartError::EntriesCrcMismatch));
    }

    #[test]
    fn detect_rejects_invalid_header_fields() {
        let cases = [
            (12_usize, 0_u32),
            (12, 513),
            (80, 0),
            (80, 257),
            (84, 127),
            (84, 513),
            (84, 130),
        ];

        for (offset, value) in cases {
            let mut data = make_gpt();

            put_u32(&mut data, SECTOR_SIZE + offset, value);
            refresh_crcs(&mut data, false);

            assert!(detect(&data).is_err(), "offset {offset}, value {value}");
        }
    }

    #[test]
    fn detect_rejects_invalid_lba_fields() {
        let cases = [
            (24_usize, 2_u64),
            (32, 1),
            (40, 9000),
            (48, 1000),
            (72, 1000),
        ];

        for (offset, value) in cases {
            let mut data = make_gpt();

            put_u64(&mut data, SECTOR_SIZE + offset, value);
            refresh_crcs(&mut data, false);

            assert!(detect(&data).is_err(), "offset {offset}");
        }
    }

    #[test]
    fn detect_rejects_overlapping_entries() {
        let mut data = make_gpt();

        put_u64(&mut data, 2 * SECTOR_SIZE + 128 + 32, 4000);
        refresh_crcs(&mut data, true);

        assert_eq!(detect(&data), Err(PartError::OverlappingPartitions));
    }

    #[test]
    fn detect_rejects_invalid_partition_range() {
        let mut data = make_gpt();

        put_u64(&mut data, 2 * SECTOR_SIZE + 32, 8192);
        refresh_crcs(&mut data, true);

        assert_eq!(detect(&data), Err(PartError::InvalidPartitionRange));
    }

    #[test]
    fn add_entry_and_reparse() {
        let mut data = make_gpt();

        let result = add_gpt_entry(
            &mut data,
            DISK_SECTORS,
            8192,
            12_287,
            EFI_SYSTEM_GUID,
            guid_from_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap_or([0; 16]),
            "EFI System",
        );

        assert!(result.is_ok());

        let parsed = detect(&data);

        assert!(matches!(
            parsed,
            Ok(Table::Gpt(info))
                if info.entries.len() == 3
                    && info.entries.iter().any(|entry| entry.name == "EFI System")
        ));
    }

    #[test]
    fn add_entry_rejects_overlapping_partition() {
        let mut data = make_gpt();

        assert_eq!(
            add_gpt_entry(
                &mut data,
                DISK_SECTORS,
                3000,
                5000,
                EFI_SYSTEM_GUID,
                guid_from_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap_or([0; 16]),
                "Overlap",
            ),
            Err(PartError::OverlappingPartitions)
        );
    }

    #[test]
    fn add_entry_rejects_invalid_guids_and_names() {
        let mut data = make_gpt();

        assert_eq!(
            add_gpt_entry(
                &mut data,
                DISK_SECTORS,
                8192,
                12_287,
                [0; 16],
                [1; 16],
                "Invalid",
            ),
            Err(PartError::InvalidGuid)
        );

        assert_eq!(
            add_gpt_entry(
                &mut data,
                DISK_SECTORS,
                8192,
                12_287,
                EFI_SYSTEM_GUID,
                [0xcc; 16],
                &"A".repeat(37),
            ),
            Err(PartError::InvalidPartitionName)
        );
    }

    #[test]
    fn add_entry_rejects_duplicate_unique_guid() {
        let mut data = make_gpt();

        let duplicate = guid_from_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap_or([0; 16]);

        assert_eq!(
            add_gpt_entry(
                &mut data,
                DISK_SECTORS,
                8192,
                12_287,
                EFI_SYSTEM_GUID,
                duplicate,
                "Duplicate",
            ),
            Err(PartError::InvalidGuid)
        );
    }

    #[test]
    fn backup_header_consistency() {
        let primary = make_gpt();

        let result = backup_gpt(&primary, DISK_SECTORS);

        assert!(result.is_ok());

        let Ok((entries, header)) = result else {
            return;
        };

        assert_eq!(entries.len(), 128 * 128);
        assert_eq!(header.len(), SECTOR_SIZE);

        assert_eq!(read_u64(&header, 24), Ok(DISK_SECTORS - 1));
        assert_eq!(read_u64(&header, 32), Ok(1));
        assert_eq!(read_u64(&header, 72), Ok(DISK_SECTORS - 33));

        assert_eq!(read_u32(&header, 88), Ok(crc32(&entries)));

        let expected_crc = read_u32(&header, 16).unwrap_or(0);

        let mut copy = header.clone();
        put_u32(&mut copy, 16, 0);

        assert_eq!(crc32(copy.get(..92).unwrap_or_default()), expected_crc);
    }

    #[test]
    fn backup_rejects_invalid_disk_size() {
        let primary = make_gpt();

        assert_eq!(
            backup_gpt(&primary, 33),
            Err(PartError::InvalidBackupLocation)
        );
    }

    #[test]
    fn largest_gpt_free_gap() {
        let data = make_gpt();

        let Ok(Table::Gpt(info)) = detect(&data) else {
            return;
        };

        let gap = largest_free_gap(&Table::Gpt(info), DISK_SECTORS);

        // Free space runs from LBA 8192 to the last usable LBA (DISK_SECTORS - 34).
        assert_eq!(gap, Some((8192, DISK_SECTORS - 33 - 8192)));
    }

    #[test]
    fn largest_mbr_free_gap() {
        let data = valid_mbr();

        let Ok(Table::Mbr(entries)) = detect(&data) else {
            return;
        };

        let gap = largest_free_gap(&Table::Mbr(entries), 100_000);

        assert_eq!(gap, Some((10_240, 89_760)));
    }

    #[test]
    fn guid_linux_filesystem_round_trip() {
        let value = "0fc63daf-8483-4772-8e79-3d69d8477de4";

        let guid = guid_from_str(value);

        assert_eq!(guid, Some(LINUX_FILESYSTEM_GUID));

        if let Some(guid) = guid {
            assert_eq!(guid_to_string(&guid), value);
        }
    }

    #[test]
    fn guid_efi_system_round_trip() {
        let value = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";

        let guid = guid_from_str(value);

        assert_eq!(guid, Some(EFI_SYSTEM_GUID));

        if let Some(guid) = guid {
            assert_eq!(guid_to_string(&guid), value);
        }
    }

    #[test]
    fn guid_rejects_malformed_values() {
        assert_eq!(guid_from_str(""), None);
        assert_eq!(guid_from_str("invalid"), None);
        assert_eq!(guid_from_str("0fc63daf"), None);
        assert_eq!(guid_from_str("0fc63daf-8483-4772"), None);
        assert_eq!(guid_from_str("zzzzzzzz-8483-4772-8e79-3d69d8477de4"), None);
    }

    #[test]
    fn malformed_buffers_never_panic() {
        for length in 0..GPT_PRIMARY_SECTORS * SECTOR_SIZE {
            let data = vec![0_u8; length];

            assert!(detect(&data).is_err());
            assert!(backup_gpt(&data, DISK_SECTORS).is_err());
        }

        let mut data = vec![0_u8; GPT_PRIMARY_SECTORS * SECTOR_SIZE];

        assert!(add_gpt_entry(
            &mut data,
            DISK_SECTORS,
            2048,
            4095,
            EFI_SYSTEM_GUID,
            [1; 16],
            "Test",
        )
        .is_err());
    }

    #[test]
    fn free_gap_rejects_too_small_disk() {
        assert_eq!(largest_free_gap(&Table::Mbr(Vec::new()), 1000), None);
    }
}
