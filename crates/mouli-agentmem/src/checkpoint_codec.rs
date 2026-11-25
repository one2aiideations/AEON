//! Deterministic, bounded snapshot codec for replication replay-window checkpoints.
//!
//! The codec provides complete encode/decode and validation logic. It does not write
//! files, encrypt snapshots, authenticate their origin, or atomically install state.

use std::collections::BTreeSet;
use std::convert::TryFrom;

const MAGIC: &[u8; 4] = b"ARCP";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 10;
const CHECKSUM_LEN: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StreamCheckpoint {
    pub tenant_id: String,
    pub peer_id: String,
    pub highest_sequence: u64,
    pub accepted_sequences: BTreeSet<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayCheckpoint {
    pub window_size: u64,
    pub streams: Vec<StreamCheckpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodecLimits {
    pub max_snapshot_bytes: usize,
    pub max_streams: usize,
    pub max_identity_bytes: usize,
    pub max_sequences_per_stream: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_snapshot_bytes: 16 * 1024 * 1024,
            max_streams: 65_536,
            max_identity_bytes: 4096,
            max_sequences_per_stream: 4096,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointError {
    InvalidLimits,
    SnapshotTooLarge,
    SnapshotTooShort,
    BadMagic,
    UnsupportedVersion(u8),
    LengthOverflow,
    Truncated,
    TrailingBytes,
    ChecksumMismatch,
    InvalidUtf8,
    EmptyIdentity,
    IdentityTooLarge,
    TooManyStreams,
    TooManySequences,
    InvalidWindow,
    InvalidSequence,
    DuplicateStream,
    DuplicateSequence,
    SequenceOutsideWindow,
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], CheckpointError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(CheckpointError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CheckpointError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, CheckpointError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(CheckpointError::Truncated)
    }

    fn u32(&mut self) -> Result<u32, CheckpointError> {
        let b = self.take(4)?;
        let mut a = [0_u8; 4];
        a.copy_from_slice(b);
        Ok(u32::from_be_bytes(a))
    }

    fn u64(&mut self) -> Result<u64, CheckpointError> {
        let b = self.take(8)?;
        let mut a = [0_u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }

    fn string(&mut self, limits: &CodecLimits) -> Result<String, CheckpointError> {
        let len = usize::try_from(self.u32()?).map_err(|_| CheckpointError::LengthOverflow)?;
        if len > limits.max_identity_bytes {
            return Err(CheckpointError::IdentityTooLarge);
        }
        let bytes = self.take(len)?;
        let value = std::str::from_utf8(bytes).map_err(|_| CheckpointError::InvalidUtf8)?;
        if value.trim().is_empty() {
            return Err(CheckpointError::EmptyIdentity);
        }
        Ok(value.to_owned())
    }

    fn is_done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, value: &str, limits: &CodecLimits) -> Result<(), CheckpointError> {
    if value.trim().is_empty() {
        return Err(CheckpointError::EmptyIdentity);
    }
    if value.len() > limits.max_identity_bytes {
        return Err(CheckpointError::IdentityTooLarge);
    }
    put_u32(
        out,
        u32::try_from(value.len()).map_err(|_| CheckpointError::LengthOverflow)?,
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

fn validate_limits(limits: &CodecLimits) -> Result<(), CheckpointError> {
    if limits.max_snapshot_bytes < HEADER_LEN + CHECKSUM_LEN
        || limits.max_identity_bytes == 0
        || limits.max_streams == 0
        || limits.max_sequences_per_stream == 0
    {
        return Err(CheckpointError::InvalidLimits);
    }
    Ok(())
}

fn validate_checkpoint(
    checkpoint: &ReplayCheckpoint,
    limits: &CodecLimits,
) -> Result<(), CheckpointError> {
    if checkpoint.window_size == 0 {
        return Err(CheckpointError::InvalidWindow);
    }
    if checkpoint.streams.len() > limits.max_streams {
        return Err(CheckpointError::TooManyStreams);
    }

    let mut stream_keys = BTreeSet::new();
    for stream in &checkpoint.streams {
        if stream.tenant_id.trim().is_empty() || stream.peer_id.trim().is_empty() {
            return Err(CheckpointError::EmptyIdentity);
        }
        if stream.tenant_id.len() > limits.max_identity_bytes
            || stream.peer_id.len() > limits.max_identity_bytes
        {
            return Err(CheckpointError::IdentityTooLarge);
        }
        if !stream_keys.insert((stream.tenant_id.as_str(), stream.peer_id.as_str())) {
            return Err(CheckpointError::DuplicateStream);
        }
        if stream.accepted_sequences.len() > limits.max_sequences_per_stream {
            return Err(CheckpointError::TooManySequences);
        }
        if stream
            .accepted_sequences
            .iter()
            .any(|sequence| *sequence == 0)
        {
            return Err(CheckpointError::InvalidSequence);
        }
        if stream
            .accepted_sequences
            .iter()
            .any(|sequence| *sequence > stream.highest_sequence)
        {
            return Err(CheckpointError::InvalidSequence);
        }
        let oldest = stream
            .highest_sequence
            .saturating_sub(checkpoint.window_size.saturating_sub(1));
        if stream
            .accepted_sequences
            .iter()
            .any(|sequence| *sequence < oldest)
        {
            return Err(CheckpointError::SequenceOutsideWindow);
        }
    }
    Ok(())
}

/// Encodes a canonical snapshot. Streams and accepted sequence numbers are sorted
/// deterministically regardless of their original input order.
pub fn encode(
    checkpoint: &ReplayCheckpoint,
    limits: &CodecLimits,
) -> Result<Vec<u8>, CheckpointError> {
    validate_limits(limits)?;
    validate_checkpoint(checkpoint, limits)?;

    let mut streams = checkpoint.streams.clone();
    streams.sort_by(|a, b| {
        a.tenant_id
            .cmp(&b.tenant_id)
            .then_with(|| a.peer_id.cmp(&b.peer_id))
    });

    let mut body = Vec::new();
    put_u64(&mut body, checkpoint.window_size);
    put_u32(
        &mut body,
        u32::try_from(streams.len()).map_err(|_| CheckpointError::LengthOverflow)?,
    );

    for stream in streams {
        put_string(&mut body, &stream.tenant_id, limits)?;
        put_string(&mut body, &stream.peer_id, limits)?;
        put_u64(&mut body, stream.highest_sequence);
        put_u32(
            &mut body,
            u32::try_from(stream.accepted_sequences.len())
                .map_err(|_| CheckpointError::LengthOverflow)?,
        );
        for sequence in stream.accepted_sequences {
            put_u64(&mut body, sequence);
        }
    }

    let total = HEADER_LEN
        .checked_add(body.len())
        .and_then(|size| size.checked_add(CHECKSUM_LEN))
        .ok_or(CheckpointError::LengthOverflow)?;
    if total > limits.max_snapshot_bytes {
        return Err(CheckpointError::SnapshotTooLarge);
    }

    let mut output = Vec::with_capacity(total);
    output.extend_from_slice(MAGIC);
    output.push(VERSION);
    output.push(0);
    put_u32(
        &mut output,
        u32::try_from(body.len()).map_err(|_| CheckpointError::LengthOverflow)?,
    );
    output.extend_from_slice(&body);
    let checksum = crc32(&output);
    put_u32(&mut output, checksum);
    Ok(output)
}

/// Decodes and fully validates a snapshot before returning any checkpoint state.
pub fn decode(bytes: &[u8], limits: &CodecLimits) -> Result<ReplayCheckpoint, CheckpointError> {
    validate_limits(limits)?;
    if bytes.len() > limits.max_snapshot_bytes {
        return Err(CheckpointError::SnapshotTooLarge);
    }
    if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err(CheckpointError::SnapshotTooShort);
    }

    let mut header = Reader::new(bytes);
    if header.take(4)? != MAGIC {
        return Err(CheckpointError::BadMagic);
    }
    let version = header.u8()?;
    if version != VERSION {
        return Err(CheckpointError::UnsupportedVersion(version));
    }
    let flags = header.u8()?;
    if flags != 0 {
        return Err(CheckpointError::TrailingBytes);
    }
    let body_len = usize::try_from(header.u32()?).map_err(|_| CheckpointError::LengthOverflow)?;
    let expected_total = HEADER_LEN
        .checked_add(body_len)
        .and_then(|size| size.checked_add(CHECKSUM_LEN))
        .ok_or(CheckpointError::LengthOverflow)?;
    if expected_total != bytes.len() {
        return Err(CheckpointError::TrailingBytes);
    }

    let checksum_offset = bytes
        .len()
        .checked_sub(CHECKSUM_LEN)
        .ok_or(CheckpointError::SnapshotTooShort)?;
    let checksum_bytes = bytes
        .get(checksum_offset..)
        .ok_or(CheckpointError::Truncated)?;
    let mut checksum_reader = Reader::new(checksum_bytes);
    let expected_checksum = checksum_reader.u32()?;
    if crc32(
        bytes
            .get(..checksum_offset)
            .ok_or(CheckpointError::Truncated)?,
    ) != expected_checksum
    {
        return Err(CheckpointError::ChecksumMismatch);
    }

    let body_start = HEADER_LEN;
    let body_end = body_start
        .checked_add(body_len)
        .ok_or(CheckpointError::LengthOverflow)?;
    let body = bytes
        .get(body_start..body_end)
        .ok_or(CheckpointError::Truncated)?;
    let mut reader = Reader::new(body);

    let window_size = reader.u64()?;
    if window_size == 0 {
        return Err(CheckpointError::InvalidWindow);
    }
    let stream_count =
        usize::try_from(reader.u32()?).map_err(|_| CheckpointError::LengthOverflow)?;
    if stream_count > limits.max_streams {
        return Err(CheckpointError::TooManyStreams);
    }

    let mut streams = Vec::with_capacity(stream_count);
    let mut keys = BTreeSet::new();
    for _ in 0..stream_count {
        let tenant_id = reader.string(limits)?;
        let peer_id = reader.string(limits)?;
        if !keys.insert((tenant_id.clone(), peer_id.clone())) {
            return Err(CheckpointError::DuplicateStream);
        }
        let highest_sequence = reader.u64()?;
        let sequence_count =
            usize::try_from(reader.u32()?).map_err(|_| CheckpointError::LengthOverflow)?;
        if sequence_count > limits.max_sequences_per_stream {
            return Err(CheckpointError::TooManySequences);
        }
        let oldest = highest_sequence.saturating_sub(window_size.saturating_sub(1));
        let mut accepted_sequences = BTreeSet::new();
        for _ in 0..sequence_count {
            let sequence = reader.u64()?;
            if sequence == 0 || sequence > highest_sequence {
                return Err(CheckpointError::InvalidSequence);
            }
            if sequence < oldest {
                return Err(CheckpointError::SequenceOutsideWindow);
            }
            if !accepted_sequences.insert(sequence) {
                return Err(CheckpointError::DuplicateSequence);
            }
        }
        streams.push(StreamCheckpoint {
            tenant_id,
            peer_id,
            highest_sequence,
            accepted_sequences,
        });
    }

    if !reader.is_done() {
        return Err(CheckpointError::TrailingBytes);
    }
    let checkpoint = ReplayCheckpoint {
        window_size,
        streams,
    };
    validate_checkpoint(&checkpoint, limits)?;
    Ok(checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ReplayCheckpoint {
        let mut sequences = BTreeSet::new();
        sequences.insert(7);
        sequences.insert(9);
        ReplayCheckpoint {
            window_size: 4,
            streams: vec![
                StreamCheckpoint {
                    tenant_id: "tenant-a".to_owned(),
                    peer_id: "peer-2".to_owned(),
                    highest_sequence: 9,
                    accepted_sequences: sequences,
                },
                StreamCheckpoint {
                    tenant_id: "tenant-b".to_owned(),
                    peer_id: "peer-1".to_owned(),
                    highest_sequence: 3,
                    accepted_sequences: [1, 3].into_iter().collect(),
                },
            ],
        }
    }

    #[test]
    fn round_trip_preserves_checkpoint() {
        let limits = CodecLimits::default();
        let original = sample();
        let encoded = encode(&original, &limits);
        assert!(encoded.is_ok());
        let decoded = match encoded {
            Ok(bytes) => decode(&bytes, &limits),
            Err(error) => Err(error),
        };
        assert_eq!(decoded, Ok(original));
    }

    #[test]
    fn encoding_is_canonical() {
        let limits = CodecLimits::default();
        let mut a = sample();
        let mut b = sample();
        a.streams.reverse();
        b.streams.reverse();
        assert_eq!(encode(&a, &limits), encode(&b, &limits));
    }

    #[test]
    fn detects_corruption_and_truncation() {
        let limits = CodecLimits::default();
        let mut bytes = match encode(&sample(), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        let last_body_byte = bytes.len().saturating_sub(CHECKSUM_LEN + 1);
        if let Some(byte) = bytes.get_mut(last_body_byte) {
            *byte ^= 0x01;
        }
        assert_eq!(
            decode(&bytes, &limits),
            Err(CheckpointError::ChecksumMismatch)
        );
        assert_eq!(
            decode(&bytes[..bytes.len().saturating_sub(1)], &limits),
            Err(CheckpointError::TrailingBytes)
        );
    }

    #[test]
    fn rejects_invalid_window_and_out_of_window_sequence() {
        let limits = CodecLimits::default();
        let invalid = ReplayCheckpoint {
            window_size: 0,
            streams: Vec::new(),
        };
        assert_eq!(
            encode(&invalid, &limits),
            Err(CheckpointError::InvalidWindow)
        );

        let mut sequences = BTreeSet::new();
        sequences.insert(1);
        sequences.insert(9);
        let invalid = ReplayCheckpoint {
            window_size: 4,
            streams: vec![StreamCheckpoint {
                tenant_id: "t".to_owned(),
                peer_id: "p".to_owned(),
                highest_sequence: 9,
                accepted_sequences: sequences,
            }],
        };
        assert_eq!(
            encode(&invalid, &limits),
            Err(CheckpointError::SequenceOutsideWindow)
        );
    }

    #[test]
    fn enforces_stream_and_payload_limits() {
        let mut limits = CodecLimits::default();
        limits.max_streams = 1;
        assert_eq!(
            encode(&sample(), &limits),
            Err(CheckpointError::TooManyStreams)
        );
        limits.max_streams = 10;
        limits.max_snapshot_bytes = HEADER_LEN + CHECKSUM_LEN; // smallest valid limit
        assert_eq!(
            encode(&sample(), &limits),
            Err(CheckpointError::SnapshotTooLarge)
        );
    }
}
