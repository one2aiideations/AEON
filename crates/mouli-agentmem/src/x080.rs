//! Deterministic bounded checkpoint codec for M079 audit-report replay watermarks.
//!
//! The checkpoint preserves the highest accepted sequence for each
//! `(consumer_id, key_id)` principal. Canonical ordering and CRC-32 provide
//! deterministic encoding and accidental-corruption detection; they do not
//! authenticate the checkpoint.
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"IARC";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 4 + 1 + 4;
const ENTRY_FIXED_BYTES: usize = 2 + 2 + 8;
const CHECKSUM_BYTES: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ReplayPrincipal {
    pub consumer_id: String,
    pub key_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayCheckpoint {
    pub generation: u64,
    pub watermarks: BTreeMap<ReplayPrincipal, u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayCheckpointLimits {
    pub max_encoded_bytes: usize,
    pub max_principals: usize,
    pub max_consumer_id_bytes: usize,
    pub max_key_id_bytes: usize,
    pub require_nonzero_generation: bool,
}
impl Default for ReplayCheckpointLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 4 * 1024 * 1024,
            max_principals: 16_384,
            max_consumer_id_bytes: 256,
            max_key_id_bytes: 128,
            require_nonzero_generation: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayCheckpointError {
    InvalidLimits,
    TooLarge,
    Truncated,
    InvalidMagic,
    UnsupportedVersion(u8),
    ChecksumMismatch,
    InvalidCheckpoint,
    InvalidUtf8,
    NonCanonicalOrder,
    DuplicatePrincipal,
    ZeroSequence,
    GenerationMismatch,
}

impl ReplayCheckpoint {
    pub fn new(generation: u64, watermarks: BTreeMap<ReplayPrincipal, u64>) -> Self {
        Self {
            generation,
            watermarks,
        }
    }

    pub fn principal_count(&self) -> usize {
        self.watermarks.len()
    }

    pub fn maximum_sequence(&self) -> u64 {
        self.watermarks.values().copied().max().unwrap_or(0)
    }
}

pub fn encode(
    checkpoint: &ReplayCheckpoint,
    limits: &ReplayCheckpointLimits,
) -> Result<Vec<u8>, ReplayCheckpointError> {
    validate_limits(limits)?;
    validate_checkpoint(checkpoint, limits)?;
    let count =
        u32::try_from(checkpoint.watermarks.len()).map_err(|_| ReplayCheckpointError::TooLarge)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&checkpoint.generation.to_be_bytes());
    bytes.extend_from_slice(&count.to_be_bytes());
    for (principal, sequence) in &checkpoint.watermarks {
        let consumer_len = u16::try_from(principal.consumer_id.len())
            .map_err(|_| ReplayCheckpointError::TooLarge)?;
        let key_len =
            u16::try_from(principal.key_id.len()).map_err(|_| ReplayCheckpointError::TooLarge)?;
        bytes.extend_from_slice(&consumer_len.to_be_bytes());
        bytes.extend_from_slice(&key_len.to_be_bytes());
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(principal.consumer_id.as_bytes());
        bytes.extend_from_slice(principal.key_id.as_bytes());
        if bytes
            .len()
            .checked_add(CHECKSUM_BYTES)
            .ok_or(ReplayCheckpointError::TooLarge)?
            > limits.max_encoded_bytes
        {
            return Err(ReplayCheckpointError::TooLarge);
        }
    }
    let checksum = crc32(&bytes);
    bytes.extend_from_slice(&checksum.to_be_bytes());
    if bytes.len() > limits.max_encoded_bytes {
        return Err(ReplayCheckpointError::TooLarge);
    }
    Ok(bytes)
}

pub fn decode(
    bytes: &[u8],
    expected_generation: Option<u64>,
    limits: &ReplayCheckpointLimits,
) -> Result<ReplayCheckpoint, ReplayCheckpointError> {
    validate_limits(limits)?;
    if bytes.len() > limits.max_encoded_bytes {
        return Err(ReplayCheckpointError::TooLarge);
    }
    if bytes.len() < HEADER_BYTES + 8 + CHECKSUM_BYTES {
        return Err(ReplayCheckpointError::Truncated);
    }
    if bytes.get(0..4) != Some(MAGIC.as_slice()) {
        return Err(ReplayCheckpointError::InvalidMagic);
    }
    let version = bytes
        .get(4)
        .copied()
        .ok_or(ReplayCheckpointError::Truncated)?;
    if version != VERSION {
        return Err(ReplayCheckpointError::UnsupportedVersion(version));
    }
    let checksum_at = bytes
        .len()
        .checked_sub(CHECKSUM_BYTES)
        .ok_or(ReplayCheckpointError::Truncated)?;
    let supplied = read_u32(
        bytes
            .get(checksum_at..)
            .ok_or(ReplayCheckpointError::Truncated)?,
    )?;
    if crc32(
        bytes
            .get(..checksum_at)
            .ok_or(ReplayCheckpointError::Truncated)?,
    ) != supplied
    {
        return Err(ReplayCheckpointError::ChecksumMismatch);
    }
    let generation = read_u64_at(bytes, 5)?;
    if expected_generation.is_some_and(|expected| expected != generation) {
        return Err(ReplayCheckpointError::GenerationMismatch);
    }
    let count =
        usize::try_from(read_u32_at(bytes, 13)?).map_err(|_| ReplayCheckpointError::TooLarge)?;
    if count > limits.max_principals {
        return Err(ReplayCheckpointError::InvalidCheckpoint);
    }
    let mut offset = 17usize;
    let mut watermarks = BTreeMap::new();
    let mut previous: Option<ReplayPrincipal> = None;
    for _ in 0..count {
        let fixed_end = offset
            .checked_add(ENTRY_FIXED_BYTES)
            .ok_or(ReplayCheckpointError::Truncated)?;
        let fixed = bytes
            .get(offset..fixed_end)
            .filter(|_| fixed_end <= checksum_at)
            .ok_or(ReplayCheckpointError::Truncated)?;
        let consumer_len = usize::from(read_u16(
            fixed.get(0..2).ok_or(ReplayCheckpointError::Truncated)?,
        )?);
        let key_len = usize::from(read_u16(
            fixed.get(2..4).ok_or(ReplayCheckpointError::Truncated)?,
        )?);
        let sequence = read_u64(fixed.get(4..12).ok_or(ReplayCheckpointError::Truncated)?)?;
        if consumer_len == 0 || key_len == 0 || sequence == 0 {
            return Err(ReplayCheckpointError::ZeroSequence);
        }
        if consumer_len > limits.max_consumer_id_bytes || key_len > limits.max_key_id_bytes {
            return Err(ReplayCheckpointError::InvalidCheckpoint);
        }
        let consumer_start = fixed_end;
        let consumer_end = consumer_start
            .checked_add(consumer_len)
            .ok_or(ReplayCheckpointError::Truncated)?;
        let key_end = consumer_end
            .checked_add(key_len)
            .ok_or(ReplayCheckpointError::Truncated)?;
        if key_end > checksum_at {
            return Err(ReplayCheckpointError::Truncated);
        }
        let consumer_id = std::str::from_utf8(
            bytes
                .get(consumer_start..consumer_end)
                .ok_or(ReplayCheckpointError::Truncated)?,
        )
        .map_err(|_| ReplayCheckpointError::InvalidUtf8)?
        .to_owned();
        let key_id = std::str::from_utf8(
            bytes
                .get(consumer_end..key_end)
                .ok_or(ReplayCheckpointError::Truncated)?,
        )
        .map_err(|_| ReplayCheckpointError::InvalidUtf8)?
        .to_owned();
        if consumer_id.trim().is_empty() || key_id.trim().is_empty() {
            return Err(ReplayCheckpointError::InvalidCheckpoint);
        }
        let principal = ReplayPrincipal {
            consumer_id,
            key_id,
        };
        if let Some(prior) = previous.as_ref() {
            if &principal == prior {
                return Err(ReplayCheckpointError::DuplicatePrincipal);
            }
            if &principal < prior {
                return Err(ReplayCheckpointError::NonCanonicalOrder);
            }
        }
        previous = Some(principal.clone());
        if watermarks.insert(principal, sequence).is_some() {
            return Err(ReplayCheckpointError::DuplicatePrincipal);
        }
        offset = key_end;
    }
    if offset != checksum_at {
        return Err(ReplayCheckpointError::InvalidCheckpoint);
    }
    let checkpoint = ReplayCheckpoint {
        generation,
        watermarks,
    };
    validate_checkpoint(&checkpoint, limits)?;
    Ok(checkpoint)
}

fn validate_limits(limits: &ReplayCheckpointLimits) -> Result<(), ReplayCheckpointError> {
    if limits.max_encoded_bytes < HEADER_BYTES + 8 + CHECKSUM_BYTES
        || limits.max_encoded_bytes > 64 * 1024 * 1024
        || limits.max_principals == 0
        || limits.max_principals > u32::MAX as usize
        || limits.max_consumer_id_bytes == 0
        || limits.max_key_id_bytes == 0
        || limits.max_consumer_id_bytes > usize::from(u16::MAX)
        || limits.max_key_id_bytes > usize::from(u16::MAX)
    {
        return Err(ReplayCheckpointError::InvalidLimits);
    }
    Ok(())
}

fn validate_checkpoint(
    checkpoint: &ReplayCheckpoint,
    limits: &ReplayCheckpointLimits,
) -> Result<(), ReplayCheckpointError> {
    if (limits.require_nonzero_generation && checkpoint.generation == 0)
        || checkpoint.watermarks.len() > limits.max_principals
    {
        return Err(ReplayCheckpointError::InvalidCheckpoint);
    }
    for (principal, sequence) in &checkpoint.watermarks {
        if principal.consumer_id.trim().is_empty()
            || principal.key_id.trim().is_empty()
            || principal.consumer_id.len() > limits.max_consumer_id_bytes
            || principal.key_id.len() > limits.max_key_id_bytes
        {
            return Err(ReplayCheckpointError::InvalidCheckpoint);
        }
        if *sequence == 0 {
            return Err(ReplayCheckpointError::ZeroSequence);
        }
    }
    Ok(())
}
fn read_u16(bytes: &[u8]) -> Result<u16, ReplayCheckpointError> {
    let part = bytes.get(0..2).ok_or(ReplayCheckpointError::Truncated)?;
    let mut out = [0u8; 2];
    out.copy_from_slice(part);
    Ok(u16::from_be_bytes(out))
}
fn read_u32(bytes: &[u8]) -> Result<u32, ReplayCheckpointError> {
    let part = bytes.get(0..4).ok_or(ReplayCheckpointError::Truncated)?;
    let mut out = [0u8; 4];
    out.copy_from_slice(part);
    Ok(u32::from_be_bytes(out))
}
fn read_u64(bytes: &[u8]) -> Result<u64, ReplayCheckpointError> {
    let part = bytes.get(0..8).ok_or(ReplayCheckpointError::Truncated)?;
    let mut out = [0u8; 8];
    out.copy_from_slice(part);
    Ok(u64::from_be_bytes(out))
}
fn read_u32_at(bytes: &[u8], offset: usize) -> Result<u32, ReplayCheckpointError> {
    let end = offset
        .checked_add(4)
        .ok_or(ReplayCheckpointError::Truncated)?;
    read_u32(
        bytes
            .get(offset..end)
            .ok_or(ReplayCheckpointError::Truncated)?,
    )
}
fn read_u64_at(bytes: &[u8], offset: usize) -> Result<u64, ReplayCheckpointError> {
    let end = offset
        .checked_add(8)
        .ok_or(ReplayCheckpointError::Truncated)?;
    read_u64(
        bytes
            .get(offset..end)
            .ok_or(ReplayCheckpointError::Truncated)?,
    )
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> ReplayCheckpoint {
        let mut map = BTreeMap::new();
        map.insert(
            ReplayPrincipal {
                consumer_id: "consumer-a".into(),
                key_id: "key-1".into(),
            },
            8,
        );
        map.insert(
            ReplayPrincipal {
                consumer_id: "consumer-b".into(),
                key_id: "key-2".into(),
            },
            19,
        );
        ReplayCheckpoint::new(4, map)
    }
    #[test]
    fn deterministic_round_trip_and_generation_guard() {
        let checkpoint = fixture();
        let a = encode(&checkpoint, &ReplayCheckpointLimits::default());
        let b = encode(&checkpoint, &ReplayCheckpointLimits::default());
        assert_eq!(a, b);
        assert_eq!(
            a.and_then(|v| decode(&v, Some(4), &ReplayCheckpointLimits::default())),
            Ok(checkpoint)
        );
    }
    #[test]
    fn rejects_wrong_generation() {
        let bytes = encode(&fixture(), &ReplayCheckpointLimits::default()).unwrap_or_default();
        assert_eq!(
            decode(&bytes, Some(5), &ReplayCheckpointLimits::default()),
            Err(ReplayCheckpointError::GenerationMismatch)
        );
    }
    #[test]
    fn rejects_checksum_corruption() {
        let mut bytes = encode(&fixture(), &ReplayCheckpointLimits::default()).unwrap_or_default();
        if let Some(value) = bytes.get_mut(20) {
            *value ^= 1;
        }
        assert_eq!(
            decode(&bytes, None, &ReplayCheckpointLimits::default()),
            Err(ReplayCheckpointError::ChecksumMismatch)
        );
    }
    #[test]
    fn rejects_zero_watermark() {
        let mut checkpoint = fixture();
        if let Some(value) = checkpoint.watermarks.values_mut().next() {
            *value = 0;
        }
        assert_eq!(
            encode(&checkpoint, &ReplayCheckpointLimits::default()),
            Err(ReplayCheckpointError::ZeroSequence)
        );
    }
    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = encode(&fixture(), &ReplayCheckpointLimits::default()).unwrap_or_default();
        bytes.push(0);
        assert_eq!(
            decode(&bytes, None, &ReplayCheckpointLimits::default()),
            Err(ReplayCheckpointError::ChecksumMismatch)
        );
    }
}
