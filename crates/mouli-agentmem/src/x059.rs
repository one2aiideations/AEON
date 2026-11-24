//! Durable append-only journal for replay delta delivery state transitions.
//!
//! Each event is independently framed and checksummed. Recovery replays complete
//! events in order and truncates only an incomplete final frame (a normal torn-write
//! case). A complete frame with invalid magic, version, length, or checksum is an
//! error; it is never silently discarded. Callers own semantic state reconstruction.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 4] = b"ADJ1";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 10;
const CHECKSUM_LEN: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalLimits {
    pub max_frame_bytes: usize,
    pub max_identity_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_events: usize,
}

impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 4 * 1024 * 1024,
            max_identity_bytes: 1024,
            max_payload_bytes: 4 * 1024 * 1024 - 128,
            max_events: 1_000_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JournalEvent {
    Enqueued {
        delivery_id: u64,
        tenant_id: String,
        peer_id: String,
        frame: Vec<u8>,
    },
    Leased {
        delivery_id: u64,
        attempt: u32,
        lease_until_ms: u64,
    },
    Acknowledged {
        delivery_id: u64,
    },
    RetryScheduled {
        delivery_id: u64,
        available_at_ms: u64,
    },
    Failed {
        delivery_id: u64,
    },
    InboundAccepted {
        tenant_id: String,
        peer_id: String,
        frame_id: u64,
    },
    InboundForgotten {
        tenant_id: String,
        peer_id: String,
        frame_id: u64,
    },
    TerminalPruned {
        delivery_id: u64,
    },
    /// Preserves IDs already allocated before a journal compaction.
    /// Must be the first event in a compacted journal.
    DeliveryIdHighWatermark {
        last_delivery_id: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JournalError {
    Io(String),
    InvalidLimits,
    FrameTooLarge,
    InvalidIdentity,
    InvalidPayload,
    BadMagic,
    UnsupportedVersion(u8),
    InvalidFlags,
    Truncated,
    LengthMismatch,
    ChecksumMismatch,
    UnknownEvent(u8),
    LengthOverflow,
}

impl From<io::Error> for JournalError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recovery {
    pub events: Vec<JournalEvent>,
    pub valid_bytes: u64,
    pub truncated_bytes: u64,
}

pub struct DeliveryJournal {
    file: File,
    limits: JournalLimits,
    end: u64,
}

impl DeliveryJournal {
    /// Opens or creates a journal, validates its complete frames, and truncates
    /// an incomplete final frame before accepting new appends.
    pub fn open(
        path: impl AsRef<Path>,
        limits: JournalLimits,
    ) -> Result<(Self, Recovery), JournalError> {
        validate_limits(&limits)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false) // keep existing records: they are recovered below
            .open(path)?;
        let recovery = recover_file(&mut file, &limits)?;
        let end = recovery.valid_bytes;
        file.seek(SeekFrom::Start(end))?;
        Ok((Self { file, limits, end }, recovery))
    }

    /// Appends one event and calls sync_data before reporting success.
    pub fn append(&mut self, event: &JournalEvent) -> Result<u64, JournalError> {
        let frame = encode_event(event, &self.limits)?;
        let next_end = self
            .end
            .checked_add(u64::try_from(frame.len()).map_err(|_| JournalError::LengthOverflow)?)
            .ok_or(JournalError::LengthOverflow)?;
        self.file.seek(SeekFrom::Start(self.end))?;
        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        self.end = next_end;
        Ok(self.end)
    }

    pub fn len_bytes(&self) -> u64 {
        self.end
    }

    /// Forces journal data and metadata to stable storage where supported.
    pub fn sync(&mut self) -> Result<(), JournalError> {
        self.file.sync_all()?;
        Ok(())
    }
}

fn validate_limits(limits: &JournalLimits) -> Result<(), JournalError> {
    if limits.max_frame_bytes < HEADER_LEN + CHECKSUM_LEN + 1
        || limits.max_identity_bytes == 0
        || limits.max_payload_bytes == 0
        || limits.max_events == 0
    {
        return Err(JournalError::InvalidLimits);
    }
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

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], JournalError> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or(JournalError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(JournalError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, JournalError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(JournalError::Truncated)
    }

    fn u32(&mut self) -> Result<u32, JournalError> {
        let bytes = self.take(4)?;
        let mut value = [0_u8; 4];
        value.copy_from_slice(bytes);
        Ok(u32::from_be_bytes(value))
    }

    fn u64(&mut self) -> Result<u64, JournalError> {
        let bytes = self.take(8)?;
        let mut value = [0_u8; 8];
        value.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(value))
    }

    fn string(&mut self, limits: &JournalLimits) -> Result<String, JournalError> {
        let len = usize::try_from(self.u32()?).map_err(|_| JournalError::LengthOverflow)?;
        if len == 0 || len > limits.max_identity_bytes {
            return Err(JournalError::InvalidIdentity);
        }
        let bytes = self.take(len)?;
        let value = std::str::from_utf8(bytes).map_err(|_| JournalError::InvalidIdentity)?;
        if value.trim().is_empty() {
            return Err(JournalError::InvalidIdentity);
        }
        Ok(value.to_owned())
    }

    fn bytes(&mut self, limits: &JournalLimits) -> Result<Vec<u8>, JournalError> {
        let len = usize::try_from(self.u32()?).map_err(|_| JournalError::LengthOverflow)?;
        if len == 0 || len > limits.max_payload_bytes {
            return Err(JournalError::InvalidPayload);
        }
        Ok(self.take(len)?.to_vec())
    }

    fn done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn put_string(out: &mut Vec<u8>, value: &str, limits: &JournalLimits) -> Result<(), JournalError> {
    if value.trim().is_empty() || value.len() > limits.max_identity_bytes {
        return Err(JournalError::InvalidIdentity);
    }
    put_u32(
        out,
        u32::try_from(value.len()).map_err(|_| JournalError::LengthOverflow)?,
    );
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn encode_event(event: &JournalEvent, limits: &JournalLimits) -> Result<Vec<u8>, JournalError> {
    let mut body = Vec::new();
    match event {
        JournalEvent::Enqueued {
            delivery_id,
            tenant_id,
            peer_id,
            frame,
        } => {
            if frame.is_empty() || frame.len() > limits.max_payload_bytes {
                return Err(JournalError::InvalidPayload);
            }
            body.push(1);
            put_u64(&mut body, *delivery_id);
            put_string(&mut body, tenant_id, limits)?;
            put_string(&mut body, peer_id, limits)?;
            put_u32(
                &mut body,
                u32::try_from(frame.len()).map_err(|_| JournalError::LengthOverflow)?,
            );
            body.extend_from_slice(frame);
        }
        JournalEvent::Leased {
            delivery_id,
            attempt,
            lease_until_ms,
        } => {
            body.push(2);
            put_u64(&mut body, *delivery_id);
            put_u32(&mut body, *attempt);
            put_u64(&mut body, *lease_until_ms);
        }
        JournalEvent::Acknowledged { delivery_id } => {
            body.push(3);
            put_u64(&mut body, *delivery_id);
        }
        JournalEvent::RetryScheduled {
            delivery_id,
            available_at_ms,
        } => {
            body.push(4);
            put_u64(&mut body, *delivery_id);
            put_u64(&mut body, *available_at_ms);
        }
        JournalEvent::Failed { delivery_id } => {
            body.push(5);
            put_u64(&mut body, *delivery_id);
        }
        JournalEvent::InboundAccepted {
            tenant_id,
            peer_id,
            frame_id,
        } => {
            body.push(6);
            put_string(&mut body, tenant_id, limits)?;
            put_string(&mut body, peer_id, limits)?;
            put_u64(&mut body, *frame_id);
        }
        JournalEvent::InboundForgotten {
            tenant_id,
            peer_id,
            frame_id,
        } => {
            body.push(7);
            put_string(&mut body, tenant_id, limits)?;
            put_string(&mut body, peer_id, limits)?;
            put_u64(&mut body, *frame_id);
        }
        JournalEvent::TerminalPruned { delivery_id } => {
            body.push(8);
            put_u64(&mut body, *delivery_id);
        }
        JournalEvent::DeliveryIdHighWatermark { last_delivery_id } => {
            body.push(9);
            put_u64(&mut body, *last_delivery_id);
        }
    }

    let total = HEADER_LEN
        .checked_add(body.len())
        .and_then(|n| n.checked_add(CHECKSUM_LEN))
        .ok_or(JournalError::LengthOverflow)?;
    if total > limits.max_frame_bytes {
        return Err(JournalError::FrameTooLarge);
    }

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(0);
    put_u32(
        &mut out,
        u32::try_from(body.len()).map_err(|_| JournalError::LengthOverflow)?,
    );
    out.extend_from_slice(&body);
    let checksum = crc32(&out);
    put_u32(&mut out, checksum);
    Ok(out)
}

fn decode_event(body: &[u8], limits: &JournalLimits) -> Result<JournalEvent, JournalError> {
    let mut reader = Reader::new(body);
    let event = match reader.u8()? {
        1 => {
            let delivery_id = reader.u64()?;
            let tenant_id = reader.string(limits)?;
            let peer_id = reader.string(limits)?;
            let frame = reader.bytes(limits)?;
            JournalEvent::Enqueued {
                delivery_id,
                tenant_id,
                peer_id,
                frame,
            }
        }
        2 => JournalEvent::Leased {
            delivery_id: reader.u64()?,
            attempt: reader.u32()?,
            lease_until_ms: reader.u64()?,
        },
        3 => JournalEvent::Acknowledged {
            delivery_id: reader.u64()?,
        },
        4 => JournalEvent::RetryScheduled {
            delivery_id: reader.u64()?,
            available_at_ms: reader.u64()?,
        },
        5 => JournalEvent::Failed {
            delivery_id: reader.u64()?,
        },
        6 => JournalEvent::InboundAccepted {
            tenant_id: reader.string(limits)?,
            peer_id: reader.string(limits)?,
            frame_id: reader.u64()?,
        },
        7 => JournalEvent::InboundForgotten {
            tenant_id: reader.string(limits)?,
            peer_id: reader.string(limits)?,
            frame_id: reader.u64()?,
        },
        8 => JournalEvent::TerminalPruned {
            delivery_id: reader.u64()?,
        },
        9 => JournalEvent::DeliveryIdHighWatermark {
            last_delivery_id: reader.u64()?,
        },
        kind => return Err(JournalError::UnknownEvent(kind)),
    };
    if !reader.done() {
        return Err(JournalError::LengthMismatch);
    }
    Ok(event)
}

/// Recovers all complete events. An incomplete suffix is truncated; corruption
/// in a complete frame is returned as an error and leaves the file untouched.
pub fn recover_file(file: &mut File, limits: &JournalLimits) -> Result<Recovery, JournalError> {
    validate_limits(limits)?;
    let file_len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let mut events = Vec::new();
    let mut offset = 0_u64;

    while offset < file_len {
        let remaining = file_len.saturating_sub(offset);
        if remaining < HEADER_LEN as u64 {
            break;
        }
        let mut header = [0_u8; HEADER_LEN];
        file.read_exact(&mut header)?;
        if header.get(0..4) != Some(MAGIC.as_slice()) {
            return Err(JournalError::BadMagic);
        }
        let version = header.get(4).copied().ok_or(JournalError::Truncated)?;
        if version != VERSION {
            return Err(JournalError::UnsupportedVersion(version));
        }
        if header.get(5).copied().ok_or(JournalError::Truncated)? != 0 {
            return Err(JournalError::InvalidFlags);
        }
        let length_slice = header.get(6..10).ok_or(JournalError::Truncated)?;
        let mut length_bytes = [0_u8; 4];
        length_bytes.copy_from_slice(length_slice);
        let body_len_u32 = u32::from_be_bytes(length_bytes);
        let body_len = usize::try_from(body_len_u32).map_err(|_| JournalError::LengthOverflow)?;
        let total = HEADER_LEN
            .checked_add(body_len)
            .and_then(|n| n.checked_add(CHECKSUM_LEN))
            .ok_or(JournalError::LengthOverflow)?;
        if total > limits.max_frame_bytes {
            return Err(JournalError::FrameTooLarge);
        }
        if remaining < u64::try_from(total).map_err(|_| JournalError::LengthOverflow)? {
            break;
        }
        let mut rest = vec![0_u8; body_len + CHECKSUM_LEN];
        file.read_exact(&mut rest)?;
        let mut complete = header.to_vec();
        complete.extend_from_slice(&rest);
        let checksum_start = complete
            .len()
            .checked_sub(CHECKSUM_LEN)
            .ok_or(JournalError::Truncated)?;
        let checksum_bytes = complete
            .get(checksum_start..)
            .ok_or(JournalError::Truncated)?;
        let mut checksum_array = [0_u8; 4];
        checksum_array.copy_from_slice(checksum_bytes);
        let expected_checksum = u32::from_be_bytes(checksum_array);
        if crc32(&complete[..checksum_start]) != expected_checksum {
            return Err(JournalError::ChecksumMismatch);
        }
        if events.len() >= limits.max_events {
            return Err(JournalError::FrameTooLarge);
        }
        let body = complete
            .get(HEADER_LEN..checksum_start)
            .ok_or(JournalError::Truncated)?;
        let event = decode_event(body, limits)?;
        events.push(event);
        offset = offset
            .checked_add(u64::try_from(total).map_err(|_| JournalError::LengthOverflow)?)
            .ok_or(JournalError::LengthOverflow)?;
    }

    let truncated_bytes = file_len.saturating_sub(offset);
    if truncated_bytes > 0 {
        file.set_len(offset)?;
        file.sync_all()?;
    }
    file.seek(SeekFrom::Start(offset))?;
    Ok(Recovery {
        events,
        valid_bytes: offset,
        truncated_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn path(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("agentos-{label}-{nonce}.journal"))
    }

    fn sample() -> Vec<JournalEvent> {
        vec![
            JournalEvent::Enqueued {
                delivery_id: 7,
                tenant_id: "tenant-a".into(),
                peer_id: "peer-b".into(),
                frame: vec![1, 2, 3, 4],
            },
            JournalEvent::Leased {
                delivery_id: 7,
                attempt: 1,
                lease_until_ms: 500,
            },
            JournalEvent::RetryScheduled {
                delivery_id: 7,
                available_at_ms: 600,
            },
            JournalEvent::Acknowledged { delivery_id: 7 },
            JournalEvent::InboundAccepted {
                tenant_id: "tenant-a".into(),
                peer_id: "peer-b".into(),
                frame_id: 99,
            },
        ]
    }

    #[test]
    fn append_and_reopen_replays_events_in_order() {
        let path = path("roundtrip");
        let expected = sample();
        {
            let opened = DeliveryJournal::open(&path, JournalLimits::default());
            assert!(opened.is_ok());
            let (mut journal, recovery) = match opened {
                Ok(v) => v,
                Err(_) => return,
            };
            assert!(recovery.events.is_empty());
            for event in &expected {
                assert!(journal.append(event).is_ok());
            }
            assert!(journal.sync().is_ok());
        }
        let reopened = DeliveryJournal::open(&path, JournalLimits::default());
        assert!(reopened.is_ok());
        if let Ok((_journal, recovery)) = reopened {
            assert_eq!(recovery.events, expected);
            assert_eq!(recovery.truncated_bytes, 0);
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn incomplete_tail_is_truncated_but_prior_events_survive() {
        let path = path("torn");
        let expected = sample();
        {
            let opened = DeliveryJournal::open(&path, JournalLimits::default());
            if let Ok((mut journal, _)) = opened {
                assert!(journal.append(&expected[0]).is_ok());
            }
        }
        if let Ok(mut file) = OpenOptions::new().append(true).open(&path) {
            let _ = file.write_all(b"ADJ");
        }
        let reopened = DeliveryJournal::open(&path, JournalLimits::default());
        assert!(reopened.is_ok());
        if let Ok((_journal, recovery)) = reopened {
            assert_eq!(recovery.events, vec![expected[0].clone()]);
            assert_eq!(recovery.truncated_bytes, 3);
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn complete_corrupt_frame_is_not_silently_truncated() {
        let path = path("corrupt");
        {
            let opened = DeliveryJournal::open(&path, JournalLimits::default());
            if let Ok((mut journal, _)) = opened {
                assert!(journal.append(&sample()[0]).is_ok());
            }
        }
        if let Ok(mut file) = OpenOptions::new().read(true).write(true).open(&path) {
            if file.seek(SeekFrom::Start(12)).is_ok() {
                let _ = file.write_all(&[0xff]);
            }
        }
        assert_eq!(
            DeliveryJournal::open(&path, JournalLimits::default()).err(),
            Some(JournalError::ChecksumMismatch)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_oversized_payload_and_invalid_identity() {
        let limits = JournalLimits {
            max_payload_bytes: 2,
            ..JournalLimits::default()
        };
        let event = JournalEvent::Enqueued {
            delivery_id: 1,
            tenant_id: "t".into(),
            peer_id: "p".into(),
            frame: vec![1, 2, 3],
        };
        assert_eq!(
            encode_event(&event, &limits),
            Err(JournalError::InvalidPayload)
        );
        let bad = JournalEvent::InboundAccepted {
            tenant_id: " ".into(),
            peer_id: "p".into(),
            frame_id: 1,
        };
        assert_eq!(
            encode_event(&bad, &JournalLimits::default()),
            Err(JournalError::InvalidIdentity)
        );
    }
}
