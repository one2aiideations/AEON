//! Bounded binary wire protocol for AgentOS agent-memory replication.
//!
//! The codec is deterministic and self-contained. It performs no I/O, authentication,
//! encryption, compression, or transport negotiation.

use std::convert::TryFrom;

const MAGIC: &[u8; 4] = b"AMRP";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 10;
const CHECKSUM_LEN: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_string_bytes: usize,
    pub max_records: usize,
    pub max_record_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 8 * 1024 * 1024,
            max_string_bytes: 4096,
            max_records: 4096,
            max_record_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryRecord {
    pub memory_id: String,
    pub version: u64,
    pub updated_at_ms: u64,
    pub deleted: bool,
    pub digest: [u8; 32],
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Hello {
        tenant_id: String,
        replica_id: String,
        protocol_min: u8,
        protocol_max: u8,
    },
    Inventory {
        tenant_id: String,
        replica_id: String,
        high_watermark: u64,
        inventory_digest: [u8; 32],
    },
    Fetch {
        tenant_id: String,
        request_id: u64,
        after_sequence: u64,
        max_records: u32,
        max_bytes: u64,
    },
    Records {
        tenant_id: String,
        request_id: u64,
        first_sequence: u64,
        records: Vec<MemoryRecord>,
    },
    Acknowledge {
        tenant_id: String,
        request_id: u64,
        through_sequence: u64,
    },
    Reject {
        tenant_id: String,
        request_id: u64,
        code: u16,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecError {
    FrameTooLarge,
    FrameTooShort,
    BadMagic,
    UnsupportedVersion(u8),
    UnknownMessageType(u8),
    LengthOverflow,
    Truncated,
    TrailingBytes,
    ChecksumMismatch,
    InvalidUtf8,
    EmptyIdentity,
    StringTooLarge,
    TooManyRecords,
    RecordTooLarge,
    InvalidBoolean,
    InvalidRange,
    InvalidLimits,
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
        let part = self
            .bytes
            .get(self.offset..end)
            .ok_or(CodecError::Truncated)?;
        self.offset = end;
        Ok(part)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        self.take(1)?.first().copied().ok_or(CodecError::Truncated)
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        let b = self.take(2)?;
        let a0 = *b.first().ok_or(CodecError::Truncated)?;
        let a1 = *b.get(1).ok_or(CodecError::Truncated)?;
        Ok(u16::from_be_bytes([a0, a1]))
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        let b = self.take(4)?;
        let a0 = *b.first().ok_or(CodecError::Truncated)?;
        let a1 = *b.get(1).ok_or(CodecError::Truncated)?;
        let a2 = *b.get(2).ok_or(CodecError::Truncated)?;
        let a3 = *b.get(3).ok_or(CodecError::Truncated)?;
        Ok(u32::from_be_bytes([a0, a1, a2, a3]))
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        let b = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(b);
        Ok(u64::from_be_bytes(array))
    }

    fn string(&mut self, limits: &Limits) -> Result<String, CodecError> {
        let len = usize::try_from(self.u32()?).map_err(|_| CodecError::LengthOverflow)?;
        if len > limits.max_string_bytes {
            return Err(CodecError::StringTooLarge);
        }
        let bytes = self.take(len)?;
        let value = std::str::from_utf8(bytes).map_err(|_| CodecError::InvalidUtf8)?;
        if value.trim().is_empty() {
            return Err(CodecError::EmptyIdentity);
        }
        Ok(value.to_owned())
    }

    fn done(&self) -> bool {
        self.offset == self.bytes.len()
    }
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

fn put_string(out: &mut Vec<u8>, value: &str, limits: &Limits) -> Result<(), CodecError> {
    if value.trim().is_empty() {
        return Err(CodecError::EmptyIdentity);
    }
    if value.len() > limits.max_string_bytes {
        return Err(CodecError::StringTooLarge);
    }
    let len = u32::try_from(value.len()).map_err(|_| CodecError::LengthOverflow)?;
    put_u32(out, len);
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

fn validate_limits(limits: &Limits) -> Result<(), CodecError> {
    if limits.max_frame_bytes < HEADER_LEN + CHECKSUM_LEN
        || limits.max_string_bytes == 0
        || limits.max_record_bytes == 0
    {
        return Err(CodecError::InvalidLimits);
    }
    Ok(())
}

pub fn encode(message: &Message, limits: &Limits) -> Result<Vec<u8>, CodecError> {
    validate_limits(limits)?;
    let mut body = Vec::new();
    let message_type = match message {
        Message::Hello {
            tenant_id,
            replica_id,
            protocol_min,
            protocol_max,
        } => {
            if protocol_min > protocol_max {
                return Err(CodecError::InvalidRange);
            }
            put_string(&mut body, tenant_id, limits)?;
            put_string(&mut body, replica_id, limits)?;
            body.push(*protocol_min);
            body.push(*protocol_max);
            1_u8
        }
        Message::Inventory {
            tenant_id,
            replica_id,
            high_watermark,
            inventory_digest,
        } => {
            put_string(&mut body, tenant_id, limits)?;
            put_string(&mut body, replica_id, limits)?;
            put_u64(&mut body, *high_watermark);
            body.extend_from_slice(inventory_digest);
            2_u8
        }
        Message::Fetch {
            tenant_id,
            request_id,
            after_sequence,
            max_records,
            max_bytes,
        } => {
            if *max_records == 0 || *max_bytes == 0 {
                return Err(CodecError::InvalidRange);
            }
            put_string(&mut body, tenant_id, limits)?;
            put_u64(&mut body, *request_id);
            put_u64(&mut body, *after_sequence);
            put_u32(&mut body, *max_records);
            put_u64(&mut body, *max_bytes);
            3_u8
        }
        Message::Records {
            tenant_id,
            request_id,
            first_sequence,
            records,
        } => {
            if records.len() > limits.max_records {
                return Err(CodecError::TooManyRecords);
            }
            put_string(&mut body, tenant_id, limits)?;
            put_u64(&mut body, *request_id);
            put_u64(&mut body, *first_sequence);
            put_u32(
                &mut body,
                u32::try_from(records.len()).map_err(|_| CodecError::LengthOverflow)?,
            );
            for record in records {
                put_string(&mut body, &record.memory_id, limits)?;
                put_u64(&mut body, record.version);
                put_u64(&mut body, record.updated_at_ms);
                body.push(u8::from(record.deleted));
                body.extend_from_slice(&record.digest);
                if record.payload.len() > limits.max_record_bytes {
                    return Err(CodecError::RecordTooLarge);
                }
                put_u32(
                    &mut body,
                    u32::try_from(record.payload.len()).map_err(|_| CodecError::LengthOverflow)?,
                );
                body.extend_from_slice(&record.payload);
            }
            4_u8
        }
        Message::Acknowledge {
            tenant_id,
            request_id,
            through_sequence,
        } => {
            put_string(&mut body, tenant_id, limits)?;
            put_u64(&mut body, *request_id);
            put_u64(&mut body, *through_sequence);
            5_u8
        }
        Message::Reject {
            tenant_id,
            request_id,
            code,
            message,
        } => {
            put_string(&mut body, tenant_id, limits)?;
            put_u64(&mut body, *request_id);
            put_u16(&mut body, *code);
            put_string(&mut body, message, limits)?;
            6_u8
        }
    };

    let body_len = u32::try_from(body.len()).map_err(|_| CodecError::LengthOverflow)?;
    let total_len = HEADER_LEN
        .checked_add(body.len())
        .and_then(|n| n.checked_add(CHECKSUM_LEN))
        .ok_or(CodecError::LengthOverflow)?;
    if total_len > limits.max_frame_bytes {
        return Err(CodecError::FrameTooLarge);
    }

    let mut frame = Vec::with_capacity(total_len);
    frame.extend_from_slice(MAGIC);
    frame.push(VERSION);
    frame.push(message_type);
    put_u32(&mut frame, body_len);
    frame.extend_from_slice(&body);
    let checksum = crc32(&frame);
    put_u32(&mut frame, checksum);
    Ok(frame)
}

pub fn decode(frame: &[u8], limits: &Limits) -> Result<Message, CodecError> {
    validate_limits(limits)?;
    if frame.len() > limits.max_frame_bytes {
        return Err(CodecError::FrameTooLarge);
    }
    if frame.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err(CodecError::FrameTooShort);
    }

    let mut header = Reader::new(frame);
    if header.take(4)? != MAGIC {
        return Err(CodecError::BadMagic);
    }
    let version = header.u8()?;
    if version != VERSION {
        return Err(CodecError::UnsupportedVersion(version));
    }
    let message_type = header.u8()?;
    let body_len = usize::try_from(header.u32()?).map_err(|_| CodecError::LengthOverflow)?;
    let expected_len = HEADER_LEN
        .checked_add(body_len)
        .and_then(|n| n.checked_add(CHECKSUM_LEN))
        .ok_or(CodecError::LengthOverflow)?;
    if expected_len != frame.len() {
        return Err(CodecError::TrailingBytes);
    }

    let checksum_offset = frame
        .len()
        .checked_sub(CHECKSUM_LEN)
        .ok_or(CodecError::FrameTooShort)?;
    let checksum_bytes = frame.get(checksum_offset..).ok_or(CodecError::Truncated)?;
    let mut checksum_reader = Reader::new(checksum_bytes);
    let expected_checksum = checksum_reader.u32()?;
    if crc32(frame.get(..checksum_offset).ok_or(CodecError::Truncated)?) != expected_checksum {
        return Err(CodecError::ChecksumMismatch);
    }

    let body_start = HEADER_LEN;
    let body_end = body_start
        .checked_add(body_len)
        .ok_or(CodecError::LengthOverflow)?;
    let body = frame
        .get(body_start..body_end)
        .ok_or(CodecError::Truncated)?;
    let mut reader = Reader::new(body);

    let message = match message_type {
        1 => {
            let tenant_id = reader.string(limits)?;
            let replica_id = reader.string(limits)?;
            let protocol_min = reader.u8()?;
            let protocol_max = reader.u8()?;
            if protocol_min > protocol_max {
                return Err(CodecError::InvalidRange);
            }
            Message::Hello {
                tenant_id,
                replica_id,
                protocol_min,
                protocol_max,
            }
        }
        2 => {
            let tenant_id = reader.string(limits)?;
            let replica_id = reader.string(limits)?;
            let high_watermark = reader.u64()?;
            let mut digest = [0_u8; 32];
            digest.copy_from_slice(reader.take(32)?);
            Message::Inventory {
                tenant_id,
                replica_id,
                high_watermark,
                inventory_digest: digest,
            }
        }
        3 => {
            let tenant_id = reader.string(limits)?;
            let request_id = reader.u64()?;
            let after_sequence = reader.u64()?;
            let max_records = reader.u32()?;
            let max_bytes = reader.u64()?;
            if max_records == 0 || max_bytes == 0 {
                return Err(CodecError::InvalidRange);
            }
            Message::Fetch {
                tenant_id,
                request_id,
                after_sequence,
                max_records,
                max_bytes,
            }
        }
        4 => {
            let tenant_id = reader.string(limits)?;
            let request_id = reader.u64()?;
            let first_sequence = reader.u64()?;
            let count = usize::try_from(reader.u32()?).map_err(|_| CodecError::LengthOverflow)?;
            if count > limits.max_records {
                return Err(CodecError::TooManyRecords);
            }
            let mut records = Vec::with_capacity(count);
            for _ in 0..count {
                let memory_id = reader.string(limits)?;
                let version = reader.u64()?;
                let updated_at_ms = reader.u64()?;
                let deleted = match reader.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(CodecError::InvalidBoolean),
                };
                let mut digest = [0_u8; 32];
                digest.copy_from_slice(reader.take(32)?);
                let payload_len =
                    usize::try_from(reader.u32()?).map_err(|_| CodecError::LengthOverflow)?;
                if payload_len > limits.max_record_bytes {
                    return Err(CodecError::RecordTooLarge);
                }
                let payload = reader.take(payload_len)?.to_vec();
                records.push(MemoryRecord {
                    memory_id,
                    version,
                    updated_at_ms,
                    deleted,
                    digest,
                    payload,
                });
            }
            Message::Records {
                tenant_id,
                request_id,
                first_sequence,
                records,
            }
        }
        5 => {
            let tenant_id = reader.string(limits)?;
            let request_id = reader.u64()?;
            let through_sequence = reader.u64()?;
            Message::Acknowledge {
                tenant_id,
                request_id,
                through_sequence,
            }
        }
        6 => {
            let tenant_id = reader.string(limits)?;
            let request_id = reader.u64()?;
            let code = reader.u16()?;
            let message = reader.string(limits)?;
            Message::Reject {
                tenant_id,
                request_id,
                code,
                message,
            }
        }
        other => return Err(CodecError::UnknownMessageType(other)),
    };

    if !reader.done() {
        return Err(CodecError::TrailingBytes);
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: Message) {
        let limits = Limits::default();
        let encoded = encode(&message, &limits);
        assert!(encoded.is_ok());
        let encoded = match encoded {
            Ok(value) => value,
            Err(_) => return,
        };
        assert_eq!(decode(&encoded, &limits), Ok(message));
    }

    #[test]
    fn round_trips_every_message_variant() {
        round_trip(Message::Hello {
            tenant_id: "tenant-a".to_owned(),
            replica_id: "replica-1".to_owned(),
            protocol_min: 1,
            protocol_max: 1,
        });
        round_trip(Message::Inventory {
            tenant_id: "tenant-a".to_owned(),
            replica_id: "replica-1".to_owned(),
            high_watermark: 99,
            inventory_digest: [7_u8; 32],
        });
        round_trip(Message::Fetch {
            tenant_id: "tenant-a".to_owned(),
            request_id: 4,
            after_sequence: 8,
            max_records: 32,
            max_bytes: 65536,
        });
        round_trip(Message::Records {
            tenant_id: "tenant-a".to_owned(),
            request_id: 4,
            first_sequence: 9,
            records: vec![MemoryRecord {
                memory_id: "mem-1".to_owned(),
                version: 2,
                updated_at_ms: 100,
                deleted: false,
                digest: [3_u8; 32],
                payload: vec![1, 2, 3],
            }],
        });
        round_trip(Message::Acknowledge {
            tenant_id: "tenant-a".to_owned(),
            request_id: 4,
            through_sequence: 10,
        });
        round_trip(Message::Reject {
            tenant_id: "tenant-a".to_owned(),
            request_id: 4,
            code: 409,
            message: "conflict".to_owned(),
        });
    }

    #[test]
    fn rejects_corruption_and_truncation() {
        let limits = Limits::default();
        let message = Message::Acknowledge {
            tenant_id: "tenant".to_owned(),
            request_id: 1,
            through_sequence: 2,
        };
        let encoded = match encode(&message, &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        let mut corrupt = encoded.clone();
        if let Some(byte) = corrupt.get_mut(HEADER_LEN) {
            *byte ^= 0x40;
        }
        assert_eq!(decode(&corrupt, &limits), Err(CodecError::ChecksumMismatch));
        assert_eq!(
            decode(&encoded[..encoded.len().saturating_sub(1)], &limits),
            Err(CodecError::TrailingBytes)
        );
    }

    #[test]
    fn enforces_record_and_frame_limits() {
        let mut limits = Limits::default();
        limits.max_record_bytes = 2;
        let message = Message::Records {
            tenant_id: "tenant".to_owned(),
            request_id: 1,
            first_sequence: 1,
            records: vec![MemoryRecord {
                memory_id: "m".to_owned(),
                version: 1,
                updated_at_ms: 1,
                deleted: false,
                digest: [0; 32],
                payload: vec![1, 2, 3],
            }],
        };
        assert_eq!(encode(&message, &limits), Err(CodecError::RecordTooLarge));
        limits.max_record_bytes = 10;
        limits.max_frame_bytes = HEADER_LEN + CHECKSUM_LEN; // smallest valid limit
        assert_eq!(
            encode(
                &Message::Hello {
                    tenant_id: "t".to_owned(),
                    replica_id: "r".to_owned(),
                    protocol_min: 1,
                    protocol_max: 1,
                },
                &limits
            ),
            Err(CodecError::FrameTooLarge)
        );
    }
}
