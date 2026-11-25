//! Versioned, bounded binary snapshot codec for agent memory records.
//!
//! The codec is deterministic and dependency-free. Its FNV-1a checksum detects
//! accidental corruption only; it is not cryptographic authentication.
//! Encryption, signatures, storage, and atomic restore are caller responsibilities.

const MAGIC: &[u8; 8] = b"AGMEM001";
const VERSION: u16 = 1;
const CHECKSUM_BYTES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRecord {
    pub id: u128,
    pub agent_id: u64,
    pub created_at_ns: u64,
    pub text: String,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub tenant_id: u64,
    pub generation: u64,
    pub records: Vec<SnapshotRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    InputTooLarge,
    Truncated,
    BadMagic,
    UnsupportedVersion,
    ChecksumMismatch,
    TooManyRecords,
    TooManyTags,
    FieldTooLarge,
    InvalidUtf8,
    DuplicateRecordId,
    TrailingBytes,
    TenantMismatch,
}

#[derive(Clone, Debug)]
pub struct CodecLimits {
    pub max_snapshot_bytes: usize,
    pub max_records: usize,
    pub max_text_bytes: usize,
    pub max_tags_per_record: usize,
    pub max_tag_bytes: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_snapshot_bytes: 64 * 1024 * 1024,
            max_records: 1_000_000,
            max_text_bytes: 1024 * 1024,
            max_tags_per_record: 128,
            max_tag_bytes: 1024,
        }
    }
}

pub fn encode(snapshot: &MemorySnapshot, limits: &CodecLimits) -> Result<Vec<u8>, CodecError> {
    validate_snapshot(snapshot, limits)?;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, VERSION);
    put_u64(&mut out, snapshot.tenant_id);
    put_u64(&mut out, snapshot.generation);
    put_u32(
        &mut out,
        u32::try_from(snapshot.records.len()).map_err(|_| CodecError::TooManyRecords)?,
    );

    for record in &snapshot.records {
        put_u128(&mut out, record.id);
        put_u64(&mut out, record.agent_id);
        put_u64(&mut out, record.created_at_ns);
        put_bytes(&mut out, record.text.as_bytes())?;
        put_u16(
            &mut out,
            u16::try_from(record.tags.len()).map_err(|_| CodecError::TooManyTags)?,
        );
        for tag in &record.tags {
            put_bytes(&mut out, tag.as_bytes())?;
        }
        if out.len().saturating_add(CHECKSUM_BYTES) > limits.max_snapshot_bytes {
            return Err(CodecError::InputTooLarge);
        }
    }

    let checksum = fnv1a64(&out);
    put_u64(&mut out, checksum);
    if out.len() > limits.max_snapshot_bytes {
        return Err(CodecError::InputTooLarge);
    }
    Ok(out)
}

pub fn decode(
    bytes: &[u8],
    expected_tenant: Option<u64>,
    limits: &CodecLimits,
) -> Result<MemorySnapshot, CodecError> {
    if bytes.len() > limits.max_snapshot_bytes {
        return Err(CodecError::InputTooLarge);
    }
    if bytes.len() < MAGIC.len() + 2 + 8 + 8 + 4 + CHECKSUM_BYTES {
        return Err(CodecError::Truncated);
    }

    let data_end = bytes
        .len()
        .checked_sub(CHECKSUM_BYTES)
        .ok_or(CodecError::Truncated)?;
    let expected_checksum = read_u64_at(bytes, data_end)?;
    if fnv1a64(&bytes[..data_end]) != expected_checksum {
        return Err(CodecError::ChecksumMismatch);
    }

    let mut reader = Reader::new(&bytes[..data_end]);
    if reader.take(MAGIC.len())? != MAGIC {
        return Err(CodecError::BadMagic);
    }
    if reader.u16()? != VERSION {
        return Err(CodecError::UnsupportedVersion);
    }
    let tenant_id = reader.u64()?;
    if expected_tenant.is_some_and(|tenant| tenant != tenant_id) {
        return Err(CodecError::TenantMismatch);
    }
    let generation = reader.u64()?;
    let count = usize::try_from(reader.u32()?).map_err(|_| CodecError::TooManyRecords)?;
    if count > limits.max_records {
        return Err(CodecError::TooManyRecords);
    }

    let mut records = Vec::with_capacity(count);
    let mut ids = std::collections::BTreeSet::new();
    for _ in 0..count {
        let id = reader.u128()?;
        if !ids.insert(id) {
            return Err(CodecError::DuplicateRecordId);
        }
        let agent_id = reader.u64()?;
        let created_at_ns = reader.u64()?;
        let text = reader.string(limits.max_text_bytes)?;
        let tag_count = usize::from(reader.u16()?);
        if tag_count > limits.max_tags_per_record {
            return Err(CodecError::TooManyTags);
        }
        let mut tags = Vec::with_capacity(tag_count);
        for _ in 0..tag_count {
            tags.push(reader.string(limits.max_tag_bytes)?);
        }
        records.push(SnapshotRecord {
            id,
            agent_id,
            created_at_ns,
            text,
            tags,
        });
    }
    if !reader.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    Ok(MemorySnapshot {
        tenant_id,
        generation,
        records,
    })
}

fn validate_snapshot(snapshot: &MemorySnapshot, limits: &CodecLimits) -> Result<(), CodecError> {
    if snapshot.records.len() > limits.max_records {
        return Err(CodecError::TooManyRecords);
    }
    let mut ids = std::collections::BTreeSet::new();
    for record in &snapshot.records {
        if record.text.len() > limits.max_text_bytes {
            return Err(CodecError::FieldTooLarge);
        }
        if record.tags.len() > limits.max_tags_per_record {
            return Err(CodecError::TooManyTags);
        }
        if record
            .tags
            .iter()
            .any(|tag| tag.len() > limits.max_tag_bytes)
        {
            return Err(CodecError::FieldTooLarge);
        }
        if !ids.insert(record.id) {
            return Err(CodecError::DuplicateRecordId);
        }
    }
    Ok(())
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), CodecError> {
    let len = u32::try_from(value.len()).map_err(|_| CodecError::FieldTooLarge)?;
    put_u32(out, len);
    out.extend_from_slice(value);
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u128(out: &mut Vec<u8>, value: u128) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u64_at(bytes: &[u8], offset: usize) -> Result<u64, CodecError> {
    let end = offset.checked_add(8).ok_or(CodecError::Truncated)?;
    let slice = bytes.get(offset..end).ok_or(CodecError::Truncated)?;
    let array: [u8; 8] = slice.try_into().map_err(|_| CodecError::Truncated)?;
    Ok(u64::from_le_bytes(array))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        let end = self.offset.checked_add(len).ok_or(CodecError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CodecError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| CodecError::Truncated)?;
        Ok(u16::from_le_bytes(bytes))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| CodecError::Truncated)?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| CodecError::Truncated)?;
        Ok(u64::from_le_bytes(bytes))
    }
    fn u128(&mut self) -> Result<u128, CodecError> {
        let bytes: [u8; 16] = self
            .take(16)?
            .try_into()
            .map_err(|_| CodecError::Truncated)?;
        Ok(u128::from_le_bytes(bytes))
    }
    fn string(&mut self, max_len: usize) -> Result<String, CodecError> {
        let len = usize::try_from(self.u32()?).map_err(|_| CodecError::FieldTooLarge)?;
        if len > max_len {
            return Err(CodecError::FieldTooLarge);
        }
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| CodecError::InvalidUtf8)
    }
    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MemorySnapshot {
        MemorySnapshot {
            tenant_id: 42,
            generation: 7,
            records: vec![SnapshotRecord {
                id: 9,
                agent_id: 3,
                created_at_ns: 100,
                text: "remember this".to_owned(),
                tags: vec!["project".to_owned(), "important".to_owned()],
            }],
        }
    }

    #[test]
    fn round_trip_preserves_snapshot() {
        let limits = CodecLimits::default();
        let encoded = encode(&sample(), &limits);
        assert!(encoded.is_ok());
        let decoded = decode(&encoded.unwrap_or_default(), Some(42), &limits);
        assert_eq!(decoded, Ok(sample()));
    }

    #[test]
    fn rejects_wrong_tenant_and_corruption() {
        let limits = CodecLimits::default();
        let mut encoded = encode(&sample(), &limits).unwrap_or_default();
        assert_eq!(
            decode(&encoded, Some(7), &limits),
            Err(CodecError::TenantMismatch)
        );
        if let Some(byte) = encoded.get_mut(25) {
            *byte ^= 1;
        }
        assert_eq!(
            decode(&encoded, Some(42), &limits),
            Err(CodecError::ChecksumMismatch)
        );
    }

    #[test]
    fn rejects_duplicate_ids_and_oversized_fields() {
        let limits = CodecLimits::default();
        let mut duplicate = sample();
        duplicate.records.push(duplicate.records[0].clone());
        assert_eq!(
            encode(&duplicate, &limits),
            Err(CodecError::DuplicateRecordId)
        );

        let mut tiny = CodecLimits::default();
        tiny.max_text_bytes = 2;
        assert_eq!(encode(&sample(), &tiny), Err(CodecError::FieldTooLarge));
    }
}
