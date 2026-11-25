//! Deterministic bounded binary codec for M055 replay checkpoint deltas.
//!
//! The codec serializes base digest, target window, and ordered remove/replace changes.
//! It validates replacement streams against M053 checkpoint invariants before encoding
//! or returning decoded data. CRC-32 detects accidental corruption; it is not a MAC.

use std::collections::BTreeSet;
use std::convert::TryFrom;

use crate::checkpoint_codec::{encode as encode_checkpoint, CodecLimits, ReplayCheckpoint};
use crate::x055::{CheckpointDelta, StreamChange, StreamKey};

const MAGIC: &[u8; 4] = b"ARDL";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 10;
const CHECKSUM_LEN: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeltaCodecLimits {
    pub max_frame_bytes: usize,
    pub max_changes: usize,
    pub max_identity_bytes: usize,
    pub max_sequences_per_stream: usize,
}

impl Default for DeltaCodecLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 16 * 1024 * 1024,
            max_changes: 65_536,
            max_identity_bytes: 4096,
            max_sequences_per_stream: 4096,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeltaCodecError {
    InvalidLimits,
    FrameTooLarge,
    FrameTooShort,
    BadMagic,
    UnsupportedVersion(u8),
    InvalidFlags,
    LengthOverflow,
    Truncated,
    TrailingBytes,
    ChecksumMismatch,
    InvalidUtf8,
    EmptyIdentity,
    IdentityTooLarge,
    TooManyChanges,
    TooManySequences,
    InvalidWindow,
    InvalidSequence,
    DuplicateChange,
    InvalidChangeKind,
    Codec(String),
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], DeltaCodecError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(DeltaCodecError::LengthOverflow)?;
        let part = self
            .bytes
            .get(self.offset..end)
            .ok_or(DeltaCodecError::Truncated)?;
        self.offset = end;
        Ok(part)
    }

    fn u8(&mut self) -> Result<u8, DeltaCodecError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(DeltaCodecError::Truncated)
    }

    fn u32(&mut self) -> Result<u32, DeltaCodecError> {
        let b = self.take(4)?;
        let mut a = [0_u8; 4];
        a.copy_from_slice(b);
        Ok(u32::from_be_bytes(a))
    }

    fn u64(&mut self) -> Result<u64, DeltaCodecError> {
        let b = self.take(8)?;
        let mut a = [0_u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }

    fn string(&mut self, limits: &DeltaCodecLimits) -> Result<String, DeltaCodecError> {
        let len = usize::try_from(self.u32()?).map_err(|_| DeltaCodecError::LengthOverflow)?;
        if len > limits.max_identity_bytes {
            return Err(DeltaCodecError::IdentityTooLarge);
        }
        let bytes = self.take(len)?;
        let value = std::str::from_utf8(bytes).map_err(|_| DeltaCodecError::InvalidUtf8)?;
        if value.trim().is_empty() {
            return Err(DeltaCodecError::EmptyIdentity);
        }
        Ok(value.to_owned())
    }

    fn done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_string(
    out: &mut Vec<u8>,
    value: &str,
    limits: &DeltaCodecLimits,
) -> Result<(), DeltaCodecError> {
    if value.trim().is_empty() {
        return Err(DeltaCodecError::EmptyIdentity);
    }
    if value.len() > limits.max_identity_bytes {
        return Err(DeltaCodecError::IdentityTooLarge);
    }
    put_u32(
        out,
        u32::try_from(value.len()).map_err(|_| DeltaCodecError::LengthOverflow)?,
    );
    out.extend_from_slice(value.as_bytes());
    Ok(())
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

fn validate_limits(limits: &DeltaCodecLimits) -> Result<(), DeltaCodecError> {
    if limits.max_frame_bytes < HEADER_LEN + CHECKSUM_LEN
        || limits.max_identity_bytes == 0
        || limits.max_changes == 0
        || limits.max_sequences_per_stream == 0
    {
        return Err(DeltaCodecError::InvalidLimits);
    }
    Ok(())
}

fn validate_delta(
    delta: &CheckpointDelta,
    limits: &DeltaCodecLimits,
) -> Result<(), DeltaCodecError> {
    if delta.target_window_size == 0 {
        return Err(DeltaCodecError::InvalidWindow);
    }
    if delta.changes.len() > limits.max_changes {
        return Err(DeltaCodecError::TooManyChanges);
    }

    let mut keys = BTreeSet::new();
    for change in &delta.changes {
        let key = match change {
            StreamChange::Remove { key } => key,
            StreamChange::Replace { stream } => {
                if stream.accepted_sequences.len() > limits.max_sequences_per_stream {
                    return Err(DeltaCodecError::TooManySequences);
                }
                let checkpoint = ReplayCheckpoint {
                    window_size: delta.target_window_size,
                    streams: vec![stream.clone()],
                };
                let codec_limits = CodecLimits {
                    max_snapshot_bytes: limits.max_frame_bytes,
                    max_streams: 1,
                    max_identity_bytes: limits.max_identity_bytes,
                    max_sequences_per_stream: limits.max_sequences_per_stream,
                };
                encode_checkpoint(&checkpoint, &codec_limits).map_err(|error| {
                    DeltaCodecError::Codec(format!("invalid replacement stream: {error:?}"))
                })?;
                // The key reference is not needed after the validation call.
                continue;
            }
        };
        if key.tenant_id.trim().is_empty() || key.peer_id.trim().is_empty() {
            return Err(DeltaCodecError::EmptyIdentity);
        }
        if key.tenant_id.len() > limits.max_identity_bytes
            || key.peer_id.len() > limits.max_identity_bytes
        {
            return Err(DeltaCodecError::IdentityTooLarge);
        }
        if !keys.insert((key.tenant_id.as_str(), key.peer_id.as_str())) {
            return Err(DeltaCodecError::DuplicateChange);
        }
    }

    // Validate uniqueness across both Remove and Replace entries.
    let mut all_keys = BTreeSet::new();
    for change in &delta.changes {
        let (tenant, peer) = match change {
            StreamChange::Remove { key } => (key.tenant_id.as_str(), key.peer_id.as_str()),
            StreamChange::Replace { stream } => {
                (stream.tenant_id.as_str(), stream.peer_id.as_str())
            }
        };
        if !all_keys.insert((tenant, peer)) {
            return Err(DeltaCodecError::DuplicateChange);
        }
    }
    Ok(())
}

/// Encodes a deterministic M055 delta frame.
pub fn encode(
    delta: &CheckpointDelta,
    limits: &DeltaCodecLimits,
) -> Result<Vec<u8>, DeltaCodecError> {
    validate_limits(limits)?;
    validate_delta(delta, limits)?;

    let mut changes = delta.changes.clone();
    changes.sort_by_key(change_key);

    let mut body = Vec::new();
    put_u64(&mut body, delta.base_digest);
    put_u64(&mut body, delta.target_window_size);
    put_u32(
        &mut body,
        u32::try_from(changes.len()).map_err(|_| DeltaCodecError::LengthOverflow)?,
    );

    for change in changes {
        match change {
            StreamChange::Remove { key } => {
                body.push(1);
                put_string(&mut body, &key.tenant_id, limits)?;
                put_string(&mut body, &key.peer_id, limits)?;
            }
            StreamChange::Replace { stream } => {
                body.push(2);
                put_string(&mut body, &stream.tenant_id, limits)?;
                put_string(&mut body, &stream.peer_id, limits)?;
                put_u64(&mut body, stream.highest_sequence);
                put_u32(
                    &mut body,
                    u32::try_from(stream.accepted_sequences.len())
                        .map_err(|_| DeltaCodecError::LengthOverflow)?,
                );
                for sequence in stream.accepted_sequences {
                    put_u64(&mut body, sequence);
                }
            }
        }
    }

    let total = HEADER_LEN
        .checked_add(body.len())
        .and_then(|n| n.checked_add(CHECKSUM_LEN))
        .ok_or(DeltaCodecError::LengthOverflow)?;
    if total > limits.max_frame_bytes {
        return Err(DeltaCodecError::FrameTooLarge);
    }

    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(MAGIC);
    frame.push(VERSION);
    frame.push(0);
    put_u32(
        &mut frame,
        u32::try_from(body.len()).map_err(|_| DeltaCodecError::LengthOverflow)?,
    );
    frame.extend_from_slice(&body);
    let checksum = crc32(&frame);
    put_u32(&mut frame, checksum);
    Ok(frame)
}

fn change_key(change: &StreamChange) -> StreamKey {
    match change {
        StreamChange::Remove { key } => key.clone(),
        StreamChange::Replace { stream } => StreamKey {
            tenant_id: stream.tenant_id.clone(),
            peer_id: stream.peer_id.clone(),
        },
    }
}

/// Decodes a frame and validates every replacement stream before returning the delta.
pub fn decode(frame: &[u8], limits: &DeltaCodecLimits) -> Result<CheckpointDelta, DeltaCodecError> {
    validate_limits(limits)?;
    if frame.len() > limits.max_frame_bytes {
        return Err(DeltaCodecError::FrameTooLarge);
    }
    if frame.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err(DeltaCodecError::FrameTooShort);
    }

    let mut header = Reader::new(frame);
    if header.take(4)? != MAGIC {
        return Err(DeltaCodecError::BadMagic);
    }
    let version = header.u8()?;
    if version != VERSION {
        return Err(DeltaCodecError::UnsupportedVersion(version));
    }
    if header.u8()? != 0 {
        return Err(DeltaCodecError::InvalidFlags);
    }
    let body_len = usize::try_from(header.u32()?).map_err(|_| DeltaCodecError::LengthOverflow)?;
    let expected = HEADER_LEN
        .checked_add(body_len)
        .and_then(|n| n.checked_add(CHECKSUM_LEN))
        .ok_or(DeltaCodecError::LengthOverflow)?;
    if expected != frame.len() {
        return Err(DeltaCodecError::TrailingBytes);
    }

    let checksum_offset = frame
        .len()
        .checked_sub(CHECKSUM_LEN)
        .ok_or(DeltaCodecError::FrameTooShort)?;
    let mut checksum_reader = Reader::new(
        frame
            .get(checksum_offset..)
            .ok_or(DeltaCodecError::Truncated)?,
    );
    let expected_checksum = checksum_reader.u32()?;
    if crc32(
        frame
            .get(..checksum_offset)
            .ok_or(DeltaCodecError::Truncated)?,
    ) != expected_checksum
    {
        return Err(DeltaCodecError::ChecksumMismatch);
    }

    let body_start = HEADER_LEN;
    let body_end = body_start
        .checked_add(body_len)
        .ok_or(DeltaCodecError::LengthOverflow)?;
    let mut reader = Reader::new(
        frame
            .get(body_start..body_end)
            .ok_or(DeltaCodecError::Truncated)?,
    );

    let base_digest = reader.u64()?;
    let target_window_size = reader.u64()?;
    if target_window_size == 0 {
        return Err(DeltaCodecError::InvalidWindow);
    }
    let count = usize::try_from(reader.u32()?).map_err(|_| DeltaCodecError::LengthOverflow)?;
    if count > limits.max_changes {
        return Err(DeltaCodecError::TooManyChanges);
    }

    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = reader.u8()?;
        let tenant_id = reader.string(limits)?;
        let peer_id = reader.string(limits)?;
        match kind {
            1 => changes.push(StreamChange::Remove {
                key: StreamKey { tenant_id, peer_id },
            }),
            2 => {
                let highest_sequence = reader.u64()?;
                let sequence_count =
                    usize::try_from(reader.u32()?).map_err(|_| DeltaCodecError::LengthOverflow)?;
                if sequence_count > limits.max_sequences_per_stream {
                    return Err(DeltaCodecError::TooManySequences);
                }
                let mut accepted_sequences = BTreeSet::new();
                for _ in 0..sequence_count {
                    let sequence = reader.u64()?;
                    if !accepted_sequences.insert(sequence) {
                        return Err(DeltaCodecError::InvalidSequence);
                    }
                }
                changes.push(StreamChange::Replace {
                    stream: crate::checkpoint_codec::StreamCheckpoint {
                        tenant_id,
                        peer_id,
                        highest_sequence,
                        accepted_sequences,
                    },
                });
            }
            _ => return Err(DeltaCodecError::InvalidChangeKind),
        }
    }

    if !reader.done() {
        return Err(DeltaCodecError::TrailingBytes);
    }
    let delta = CheckpointDelta {
        base_digest,
        target_window_size,
        changes,
    };
    validate_delta(&delta, limits)?;
    Ok(delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_codec::StreamCheckpoint;
    use std::collections::BTreeSet;

    fn sample() -> CheckpointDelta {
        let mut accepted = BTreeSet::new();
        accepted.insert(5);
        accepted.insert(7);
        CheckpointDelta {
            base_digest: 0x1234_5678,
            target_window_size: 16,
            changes: vec![
                StreamChange::Remove {
                    key: StreamKey {
                        tenant_id: "tenant-a".to_owned(),
                        peer_id: "peer-old".to_owned(),
                    },
                },
                StreamChange::Replace {
                    stream: StreamCheckpoint {
                        tenant_id: "tenant-b".to_owned(),
                        peer_id: "peer-new".to_owned(),
                        highest_sequence: 7,
                        accepted_sequences: accepted,
                    },
                },
            ],
        }
    }

    #[test]
    fn round_trips_remove_and_replace_changes() {
        let limits = DeltaCodecLimits::default();
        let bytes = encode(&sample(), &limits);
        assert!(bytes.is_ok());
        let decoded = match bytes {
            Ok(value) => decode(&value, &limits),
            Err(error) => Err(error),
        };
        assert_eq!(decoded, Ok(sample()));
    }

    #[test]
    fn encoding_is_deterministic_independent_of_change_order() {
        let limits = DeltaCodecLimits::default();
        let a = sample();
        let mut b = sample();
        b.changes.reverse();
        assert_eq!(encode(&a, &limits), encode(&b, &limits));
    }

    #[test]
    fn rejects_corrupt_frames_and_bad_magic() {
        let limits = DeltaCodecLimits::default();
        let mut bytes = match encode(&sample(), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        if let Some(byte) = bytes.get_mut(HEADER_LEN) {
            *byte ^= 1;
        }
        assert_eq!(
            decode(&bytes, &limits),
            Err(DeltaCodecError::ChecksumMismatch)
        );
        // A frame of valid length whose magic is wrong is rejected as BadMagic.
        let mut wrong_magic = bytes.clone();
        wrong_magic[..4].copy_from_slice(b"NOPE");
        assert_eq!(
            decode(&wrong_magic, &limits),
            Err(DeltaCodecError::BadMagic)
        );
        // A frame shorter than header + checksum is rejected as too short.
        assert_eq!(
            decode(b"NOPE000000", &limits),
            Err(DeltaCodecError::FrameTooShort)
        );
    }

    #[test]
    fn rejects_duplicate_changes_and_invalid_replacement_sequences() {
        let limits = DeltaCodecLimits::default();
        let duplicate = CheckpointDelta {
            base_digest: 1,
            target_window_size: 8,
            changes: vec![
                StreamChange::Remove {
                    key: StreamKey {
                        tenant_id: "t".to_owned(),
                        peer_id: "p".to_owned(),
                    },
                },
                StreamChange::Remove {
                    key: StreamKey {
                        tenant_id: "t".to_owned(),
                        peer_id: "p".to_owned(),
                    },
                },
            ],
        };
        assert_eq!(
            encode(&duplicate, &limits),
            Err(DeltaCodecError::DuplicateChange)
        );

        let invalid = CheckpointDelta {
            base_digest: 1,
            target_window_size: 8,
            changes: vec![StreamChange::Replace {
                stream: StreamCheckpoint {
                    tenant_id: "t".to_owned(),
                    peer_id: "p".to_owned(),
                    highest_sequence: 3,
                    accepted_sequences: [3, 4].into_iter().collect(),
                },
            }],
        };
        assert!(matches!(
            encode(&invalid, &limits),
            Err(DeltaCodecError::Codec(_))
        ));
    }

    #[test]
    fn enforces_frame_and_change_limits() {
        let mut limits = DeltaCodecLimits::default();
        limits.max_changes = 1;
        let mut value = sample();
        assert_eq!(
            encode(&value, &limits),
            Err(DeltaCodecError::TooManyChanges)
        );
        limits.max_changes = 10;
        // One byte below the real frame size must be rejected as too large.
        limits.max_frame_bytes = 10_000;
        let full = encode(&value, &limits).expect("fits in 10k").len();
        limits.max_frame_bytes = full - 1;
        assert_eq!(encode(&value, &limits), Err(DeltaCodecError::FrameTooLarge));
        // A limit at the smallest valid size is rejected too (with whichever size error fires first).
        limits.max_frame_bytes = HEADER_LEN + CHECKSUM_LEN;
        assert!(encode(&value, &limits).is_err());
        value.changes.clear();
    }
}
