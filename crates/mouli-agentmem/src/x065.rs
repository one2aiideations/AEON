//! Deterministic bounded binary codec for M062 delivery-retention plans.
//!
//! The codec provides integrity against accidental corruption using CRC-32.
//! It does not authenticate the producer, encrypt content, or prevent deliberate
//! modification by an attacker.

use crate::x060::PeerKey;
use crate::x062::RetentionPlan;
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"ARPL";
const VERSION: u8 = 1;
const CHECKSUM_LEN: usize = 4;
const FIXED_HEADER_LEN: usize = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodecLimits {
    pub max_encoded_bytes: usize,
    pub max_entries_per_list: usize,
    pub max_peers: usize,
    pub max_identity_bytes: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 8 * 1024 * 1024,
            max_entries_per_list: 100_000,
            max_peers: 16_384,
            max_identity_bytes: 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecError {
    InvalidLimits,
    TooLarge,
    TooManyEntries,
    TooManyPeers,
    InvalidIdentity,
    InvalidMagic,
    UnsupportedVersion(u8),
    Truncated,
    TrailingBytes,
    ChecksumMismatch,
    LengthOverflow,
    NonCanonicalOrder,
    DuplicateEntry,
    DuplicatePeer,
}

pub fn encode(plan: &RetentionPlan, limits: &CodecLimits) -> Result<Vec<u8>, CodecError> {
    validate_limits(limits)?;
    validate_plan(plan, limits)?;

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    put_u64(&mut out, plan.generated_at_ms);
    put_u32(&mut out, count32(plan.prune_delivery_ids.len())?);
    for id in &plan.prune_delivery_ids {
        put_u64(&mut out, *id);
    }

    put_u32(&mut out, count32(plan.forget_inbound.len())?);
    for (peer, frame_id) in &plan.forget_inbound {
        put_peer(&mut out, peer, limits)?;
        put_u64(&mut out, *frame_id);
    }

    put_u64(
        &mut out,
        u64::try_from(plan.retained_terminal_count).map_err(|_| CodecError::LengthOverflow)?,
    );
    put_u32(&mut out, count32(plan.retained_inbound_counts.len())?);
    for (peer, count) in &plan.retained_inbound_counts {
        put_peer(&mut out, peer, limits)?;
        put_u64(
            &mut out,
            u64::try_from(*count).map_err(|_| CodecError::LengthOverflow)?,
        );
    }

    put_u32(&mut out, count32(plan.missing_terminal_timestamps.len())?);
    for id in &plan.missing_terminal_timestamps {
        put_u64(&mut out, *id);
    }

    put_u32(&mut out, count32(plan.missing_inbound_timestamps.len())?);
    for (peer, frame_id) in &plan.missing_inbound_timestamps {
        put_peer(&mut out, peer, limits)?;
        put_u64(&mut out, *frame_id);
    }

    let total = out
        .len()
        .checked_add(CHECKSUM_LEN)
        .ok_or(CodecError::LengthOverflow)?;
    if total > limits.max_encoded_bytes {
        return Err(CodecError::TooLarge);
    }
    let checksum = crc32(&out);
    put_u32(&mut out, checksum);
    Ok(out)
}

pub fn decode(bytes: &[u8], limits: &CodecLimits) -> Result<RetentionPlan, CodecError> {
    validate_limits(limits)?;
    if bytes.len() > limits.max_encoded_bytes {
        return Err(CodecError::TooLarge);
    }
    if bytes.len() < FIXED_HEADER_LEN + 8 + CHECKSUM_LEN {
        return Err(CodecError::Truncated);
    }

    let content_len = bytes
        .len()
        .checked_sub(CHECKSUM_LEN)
        .ok_or(CodecError::Truncated)?;
    let (content, checksum_bytes) = bytes.split_at(content_len);
    let expected = read_u32_exact(checksum_bytes)?;
    if crc32(content) != expected {
        return Err(CodecError::ChecksumMismatch);
    }

    let mut reader = Reader::new(content);
    if reader.take(4)? != MAGIC {
        return Err(CodecError::InvalidMagic);
    }
    let version = reader.u8()?;
    if version != VERSION {
        return Err(CodecError::UnsupportedVersion(version));
    }
    let generated_at_ms = reader.u64()?;

    let prune_count = reader.count(limits.max_entries_per_list)?;
    let mut prune_delivery_ids = Vec::with_capacity(prune_count);
    for _ in 0..prune_count {
        prune_delivery_ids.push(reader.u64()?);
    }

    let forget_count = reader.count(limits.max_entries_per_list)?;
    let mut forget_inbound = Vec::with_capacity(forget_count);
    for _ in 0..forget_count {
        forget_inbound.push((reader.peer(limits)?, reader.u64()?));
    }

    let retained_terminal_count =
        usize::try_from(reader.u64()?).map_err(|_| CodecError::LengthOverflow)?;
    let peer_count = reader.count(limits.max_peers)?;
    let mut retained_inbound_counts = BTreeMap::new();
    for _ in 0..peer_count {
        let peer = reader.peer(limits)?;
        let count = usize::try_from(reader.u64()?).map_err(|_| CodecError::LengthOverflow)?;
        if retained_inbound_counts.insert(peer, count).is_some() {
            return Err(CodecError::DuplicatePeer);
        }
    }

    let missing_terminal_count = reader.count(limits.max_entries_per_list)?;
    let mut missing_terminal_timestamps = Vec::with_capacity(missing_terminal_count);
    for _ in 0..missing_terminal_count {
        missing_terminal_timestamps.push(reader.u64()?);
    }

    let missing_inbound_count = reader.count(limits.max_entries_per_list)?;
    let mut missing_inbound_timestamps = Vec::with_capacity(missing_inbound_count);
    for _ in 0..missing_inbound_count {
        missing_inbound_timestamps.push((reader.peer(limits)?, reader.u64()?));
    }

    if !reader.done() {
        return Err(CodecError::TrailingBytes);
    }
    let plan = RetentionPlan {
        generated_at_ms,
        prune_delivery_ids,
        forget_inbound,
        retained_terminal_count,
        retained_inbound_counts,
        missing_terminal_timestamps,
        missing_inbound_timestamps,
    };
    validate_plan(&plan, limits)?;
    Ok(plan)
}

fn validate_plan(plan: &RetentionPlan, limits: &CodecLimits) -> Result<(), CodecError> {
    if plan.prune_delivery_ids.len() > limits.max_entries_per_list
        || plan.forget_inbound.len() > limits.max_entries_per_list
        || plan.missing_terminal_timestamps.len() > limits.max_entries_per_list
        || plan.missing_inbound_timestamps.len() > limits.max_entries_per_list
    {
        return Err(CodecError::TooManyEntries);
    }
    if plan.retained_inbound_counts.len() > limits.max_peers {
        return Err(CodecError::TooManyPeers);
    }
    if !strictly_increasing(&plan.prune_delivery_ids)
        || !strictly_increasing(&plan.forget_inbound)
        || !strictly_increasing(&plan.missing_terminal_timestamps)
        || !strictly_increasing(&plan.missing_inbound_timestamps)
    {
        return Err(CodecError::NonCanonicalOrder);
    }
    for peer in plan.retained_inbound_counts.keys() {
        validate_peer(peer, limits)?;
    }
    for (peer, _) in &plan.forget_inbound {
        validate_peer(peer, limits)?;
    }
    for (peer, _) in &plan.missing_inbound_timestamps {
        validate_peer(peer, limits)?;
    }
    Ok(())
}

fn validate_peer(peer: &PeerKey, limits: &CodecLimits) -> Result<(), CodecError> {
    for value in [&peer.tenant_id, &peer.peer_id] {
        if value.trim().is_empty() || value.len() > limits.max_identity_bytes {
            return Err(CodecError::InvalidIdentity);
        }
    }
    Ok(())
}

fn validate_limits(limits: &CodecLimits) -> Result<(), CodecError> {
    if limits.max_encoded_bytes < FIXED_HEADER_LEN + 8 + CHECKSUM_LEN
        || limits.max_entries_per_list == 0
        || limits.max_peers == 0
        || limits.max_identity_bytes == 0
    {
        return Err(CodecError::InvalidLimits);
    }
    Ok(())
}

fn strictly_increasing<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn count32(value: usize) -> Result<u32, CodecError> {
    u32::try_from(value).map_err(|_| CodecError::LengthOverflow)
}
fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_peer(out: &mut Vec<u8>, peer: &PeerKey, limits: &CodecLimits) -> Result<(), CodecError> {
    validate_peer(peer, limits)?;
    let tenant = u16::try_from(peer.tenant_id.len()).map_err(|_| CodecError::LengthOverflow)?;
    let id = u16::try_from(peer.peer_id.len()).map_err(|_| CodecError::LengthOverflow)?;
    put_u16(out, tenant);
    out.extend_from_slice(peer.tenant_id.as_bytes());
    put_u16(out, id);
    out.extend_from_slice(peer.peer_id.as_bytes());
    Ok(())
}

fn read_u32_exact(bytes: &[u8]) -> Result<u32, CodecError> {
    let slice = bytes.get(0..4).ok_or(CodecError::Truncated)?;
    let mut value = [0_u8; 4];
    value.copy_from_slice(slice);
    Ok(u32::from_be_bytes(value))
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(CodecError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CodecError::Truncated)?;
        self.offset = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        self.take(1)?.first().copied().ok_or(CodecError::Truncated)
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        let bytes = self.take(2)?;
        let mut value = [0_u8; 2];
        value.copy_from_slice(bytes);
        Ok(u16::from_be_bytes(value))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let bytes = self.take(4)?;
        let mut value = [0_u8; 4];
        value.copy_from_slice(bytes);
        Ok(u32::from_be_bytes(value))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        let bytes = self.take(8)?;
        let mut value = [0_u8; 8];
        value.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(value))
    }
    fn count(&mut self, max: usize) -> Result<usize, CodecError> {
        let count = usize::try_from(self.u32()?).map_err(|_| CodecError::LengthOverflow)?;
        if count > max {
            return Err(CodecError::TooManyEntries);
        }
        Ok(count)
    }
    fn peer(&mut self, limits: &CodecLimits) -> Result<PeerKey, CodecError> {
        let tenant_len = usize::from(self.u16()?);
        if tenant_len == 0 || tenant_len > limits.max_identity_bytes {
            return Err(CodecError::InvalidIdentity);
        }
        let tenant = std::str::from_utf8(self.take(tenant_len)?)
            .map_err(|_| CodecError::InvalidIdentity)?
            .to_owned();
        let peer_len = usize::from(self.u16()?);
        if peer_len == 0 || peer_len > limits.max_identity_bytes {
            return Err(CodecError::InvalidIdentity);
        }
        let peer = std::str::from_utf8(self.take(peer_len)?)
            .map_err(|_| CodecError::InvalidIdentity)?
            .to_owned();
        let value = PeerKey {
            tenant_id: tenant,
            peer_id: peer,
        };
        validate_peer(&value, limits)?;
        Ok(value)
    }
    fn done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> RetentionPlan {
        let peer = PeerKey {
            tenant_id: "tenant".into(),
            peer_id: "peer".into(),
        };
        RetentionPlan {
            generated_at_ms: 1234,
            prune_delivery_ids: vec![2, 5],
            forget_inbound: vec![(peer.clone(), 7), (peer.clone(), 9)],
            retained_terminal_count: 3,
            retained_inbound_counts: BTreeMap::from([(peer.clone(), 4)]),
            missing_terminal_timestamps: vec![8, 11],
            missing_inbound_timestamps: vec![(peer, 12)],
        }
    }

    #[test]
    fn round_trip_is_deterministic() {
        let value = plan();
        let first = encode(&value, &CodecLimits::default());
        let second = encode(&value, &CodecLimits::default());
        assert_eq!(first, second);
        assert_eq!(
            first.and_then(|bytes| decode(&bytes, &CodecLimits::default())),
            Ok(value)
        );
    }

    #[test]
    fn rejects_corruption_and_trailing_data() {
        let encoded = encode(&plan(), &CodecLimits::default());
        assert!(encoded.is_ok());
        if let Ok(mut bytes) = encoded {
            let last = bytes.len().saturating_sub(1);
            if let Some(byte) = bytes.get_mut(last) {
                *byte ^= 0x40;
            }
            assert_eq!(
                decode(&bytes, &CodecLimits::default()),
                Err(CodecError::ChecksumMismatch)
            );
        }
    }

    #[test]
    fn rejects_noncanonical_plan() {
        let mut value = plan();
        value.prune_delivery_ids = vec![5, 2];
        assert_eq!(
            encode(&value, &CodecLimits::default()),
            Err(CodecError::NonCanonicalOrder)
        );
    }

    #[test]
    fn enforces_size_and_entry_bounds() {
        let value = plan();
        let limits = CodecLimits {
            max_entries_per_list: 1,
            ..CodecLimits::default()
        };
        assert_eq!(encode(&value, &limits), Err(CodecError::TooManyEntries));
    }
}
