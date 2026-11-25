//! Deterministic, bounded codec for non-secret checkpoint-keyring audit events.
//!
//! The format records lifecycle metadata only. Secret key bytes are never accepted,
//! encoded, decoded, or exposed by this crate. CRC-32 detects accidental corruption;
//! it is not an authenticity mechanism.

use std::fmt;

const MAGIC: &[u8; 4] = b"KRAU";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 4 + 2 + 4;
const CHECKSUM_LEN: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyStatus {
    Active,
    VerifyOnly,
    Revoked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditAction {
    Initialized,
    Rotated,
    Revoked,
    Purged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyMetadata {
    pub key_id: String,
    pub status: KeyStatus,
    pub activated_at_ms: u64,
    pub verify_until_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyringAuditEvent {
    pub sequence: u64,
    pub at_ms: u64,
    pub action: AuditAction,
    /// For rotation, this is the former active key; otherwise the affected key.
    pub key: KeyMetadata,
    /// Present only for rotation, and identifies the newly active key.
    pub related: Option<KeyMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodecLimits {
    pub max_events: usize,
    pub max_key_id_bytes: usize,
    pub max_total_bytes: usize,
}
impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_events: 4096,
            max_key_id_bytes: 128,
            max_total_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecError {
    InvalidLimits,
    TooManyEvents,
    TooLarge,
    Truncated,
    BadMagic,
    UnsupportedVersion(u16),
    LengthMismatch,
    ChecksumMismatch,
    InvalidUtf8,
    InvalidKeyId,
    InvalidStatus(u8),
    InvalidAction(u8),
    InvalidOption(u8),
    InvalidEvent,
    NonIncreasingSequence,
    TrailingBytes,
    IntegerOverflow,
}
impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CodecError {}

pub fn encode(events: &[KeyringAuditEvent], limits: &CodecLimits) -> Result<Vec<u8>, CodecError> {
    validate_limits(limits)?;
    if events.len() > limits.max_events {
        return Err(CodecError::TooManyEvents);
    }
    let mut body = Vec::new();
    put_u32(
        &mut body,
        u32::try_from(events.len()).map_err(|_| CodecError::IntegerOverflow)?,
    );
    let mut previous = None;
    for event in events {
        validate_event(event, limits)?;
        if previous.map(|p| event.sequence <= p).unwrap_or(false) {
            return Err(CodecError::NonIncreasingSequence);
        }
        previous = Some(event.sequence);
        put_u64(&mut body, event.sequence);
        put_u64(&mut body, event.at_ms);
        body.push(action_byte(event.action));
        put_metadata(&mut body, &event.key, limits)?;
        match &event.related {
            Some(metadata) => {
                body.push(1);
                put_metadata(&mut body, metadata, limits)?;
            }
            None => body.push(0),
        }
        if HEADER_LEN
            .checked_add(body.len())
            .and_then(|n| n.checked_add(CHECKSUM_LEN))
            .ok_or(CodecError::IntegerOverflow)?
            > limits.max_total_bytes
        {
            return Err(CodecError::TooLarge);
        }
    }
    let body_len = u32::try_from(body.len()).map_err(|_| CodecError::IntegerOverflow)?;
    let mut out = Vec::with_capacity(HEADER_LEN + body.len() + CHECKSUM_LEN);
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, VERSION);
    put_u32(&mut out, body_len);
    out.extend_from_slice(&body);
    let checksum = crc32(&out);
    put_u32(&mut out, checksum);
    if out.len() > limits.max_total_bytes {
        return Err(CodecError::TooLarge);
    }
    Ok(out)
}

pub fn decode(bytes: &[u8], limits: &CodecLimits) -> Result<Vec<KeyringAuditEvent>, CodecError> {
    validate_limits(limits)?;
    if bytes.len() > limits.max_total_bytes {
        return Err(CodecError::TooLarge);
    }
    if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err(CodecError::Truncated);
    }
    if &bytes[..4] != MAGIC {
        return Err(CodecError::BadMagic);
    }
    let version = read_u16_at(bytes, 4)?;
    if version != VERSION {
        return Err(CodecError::UnsupportedVersion(version));
    }
    let body_len = read_u32_at(bytes, 6)? as usize;
    let expected = HEADER_LEN
        .checked_add(body_len)
        .and_then(|n| n.checked_add(CHECKSUM_LEN))
        .ok_or(CodecError::IntegerOverflow)?;
    if expected != bytes.len() {
        return Err(CodecError::LengthMismatch);
    }
    let stored_crc = read_u32_at(bytes, bytes.len() - CHECKSUM_LEN)?;
    if crc32(&bytes[..bytes.len() - CHECKSUM_LEN]) != stored_crc {
        return Err(CodecError::ChecksumMismatch);
    }
    let mut r = Reader {
        bytes: &bytes[HEADER_LEN..bytes.len() - CHECKSUM_LEN],
        pos: 0,
    };
    let count = r.u32()? as usize;
    if count > limits.max_events {
        return Err(CodecError::TooManyEvents);
    }
    let mut events = Vec::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let sequence = r.u64()?;
        let at_ms = r.u64()?;
        let action = parse_action(r.byte()?)?;
        let key = r.metadata(limits)?;
        let related = match r.byte()? {
            0 => None,
            1 => Some(r.metadata(limits)?),
            n => return Err(CodecError::InvalidOption(n)),
        };
        let event = KeyringAuditEvent {
            sequence,
            at_ms,
            action,
            key,
            related,
        };
        validate_event(&event, limits)?;
        if previous.map(|p| sequence <= p).unwrap_or(false) {
            return Err(CodecError::NonIncreasingSequence);
        }
        previous = Some(sequence);
        events.push(event);
    }
    if r.pos != r.bytes.len() {
        return Err(CodecError::TrailingBytes);
    }
    Ok(events)
}

fn validate_limits(l: &CodecLimits) -> Result<(), CodecError> {
    if l.max_events == 0
        || l.max_key_id_bytes == 0
        || l.max_total_bytes < HEADER_LEN + CHECKSUM_LEN + 4
    {
        return Err(CodecError::InvalidLimits);
    }
    Ok(())
}
fn validate_event(e: &KeyringAuditEvent, l: &CodecLimits) -> Result<(), CodecError> {
    if e.sequence == 0 {
        return Err(CodecError::InvalidEvent);
    }
    validate_metadata(&e.key, l)?;
    if let Some(other) = &e.related {
        validate_metadata(other, l)?;
        if other.key_id == e.key.key_id {
            return Err(CodecError::InvalidEvent);
        }
    }
    let valid = match e.action {
        AuditAction::Initialized => {
            e.key.status == KeyStatus::Active
                && e.key.verify_until_ms.is_none()
                && e.related.is_none()
        }
        AuditAction::Rotated => {
            e.key.status == KeyStatus::VerifyOnly
                && e.key.verify_until_ms.is_some()
                && e.related
                    .as_ref()
                    .map(|m| {
                        m.status == KeyStatus::Active
                            && m.verify_until_ms.is_none()
                            && m.activated_at_ms == e.at_ms
                    })
                    .unwrap_or(false)
        }
        AuditAction::Revoked => {
            e.key.status == KeyStatus::Revoked
                && e.key.verify_until_ms == Some(e.at_ms)
                && e.related.is_none()
        }
        AuditAction::Purged => {
            matches!(e.key.status, KeyStatus::Revoked | KeyStatus::VerifyOnly)
                && e.related.is_none()
        }
    };
    if !valid {
        return Err(CodecError::InvalidEvent);
    }
    Ok(())
}
fn validate_metadata(m: &KeyMetadata, l: &CodecLimits) -> Result<(), CodecError> {
    if m.key_id.trim().is_empty()
        || m.key_id.len() > l.max_key_id_bytes
        || m.key_id.as_bytes().contains(&0)
    {
        return Err(CodecError::InvalidKeyId);
    }
    if let Some(until) = m.verify_until_ms {
        if until < m.activated_at_ms {
            return Err(CodecError::InvalidEvent);
        }
    }
    if m.status == KeyStatus::Active && m.verify_until_ms.is_some() {
        return Err(CodecError::InvalidEvent);
    }
    if m.status == KeyStatus::Revoked && m.verify_until_ms.is_none() {
        return Err(CodecError::InvalidEvent);
    }
    Ok(())
}
fn put_metadata(out: &mut Vec<u8>, m: &KeyMetadata, l: &CodecLimits) -> Result<(), CodecError> {
    validate_metadata(m, l)?;
    let n = u16::try_from(m.key_id.len()).map_err(|_| CodecError::IntegerOverflow)?;
    put_u16(out, n);
    out.extend_from_slice(m.key_id.as_bytes());
    out.push(match m.status {
        KeyStatus::Active => 1,
        KeyStatus::VerifyOnly => 2,
        KeyStatus::Revoked => 3,
    });
    put_u64(out, m.activated_at_ms);
    match m.verify_until_ms {
        Some(v) => {
            out.push(1);
            put_u64(out, v)
        }
        None => out.push(0),
    }
    Ok(())
}
fn action_byte(a: AuditAction) -> u8 {
    match a {
        AuditAction::Initialized => 1,
        AuditAction::Rotated => 2,
        AuditAction::Revoked => 3,
        AuditAction::Purged => 4,
    }
}
fn parse_action(n: u8) -> Result<AuditAction, CodecError> {
    match n {
        1 => Ok(AuditAction::Initialized),
        2 => Ok(AuditAction::Rotated),
        3 => Ok(AuditAction::Revoked),
        4 => Ok(AuditAction::Purged),
        _ => Err(CodecError::InvalidAction(n)),
    }
}
fn put_u16(v: &mut Vec<u8>, n: u16) {
    v.extend_from_slice(&n.to_be_bytes())
}
fn put_u32(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_be_bytes())
}
fn put_u64(v: &mut Vec<u8>, n: u64) {
    v.extend_from_slice(&n.to_be_bytes())
}
fn read_u16_at(b: &[u8], p: usize) -> Result<u16, CodecError> {
    let s = b.get(p..p + 2).ok_or(CodecError::Truncated)?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}
fn read_u32_at(b: &[u8], p: usize) -> Result<u32, CodecError> {
    let s = b.get(p..p + 4).ok_or(CodecError::Truncated)?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.pos.checked_add(n).ok_or(CodecError::IntegerOverflow)?;
        let s = self.bytes.get(self.pos..end).ok_or(CodecError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn byte(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        let s = self.take(8)?;
        Ok(u64::from_be_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }
    fn metadata(&mut self, l: &CodecLimits) -> Result<KeyMetadata, CodecError> {
        let n = self.u16()? as usize;
        if n == 0 || n > l.max_key_id_bytes {
            return Err(CodecError::InvalidKeyId);
        }
        let id = std::str::from_utf8(self.take(n)?)
            .map_err(|_| CodecError::InvalidUtf8)?
            .to_owned();
        let status = match self.byte()? {
            1 => KeyStatus::Active,
            2 => KeyStatus::VerifyOnly,
            3 => KeyStatus::Revoked,
            x => return Err(CodecError::InvalidStatus(x)),
        };
        let activated_at_ms = self.u64()?;
        let verify_until_ms = match self.byte()? {
            0 => None,
            1 => Some(self.u64()?),
            x => return Err(CodecError::InvalidOption(x)),
        };
        let m = KeyMetadata {
            key_id: id,
            status,
            activated_at_ms,
            verify_until_ms,
        };
        validate_metadata(&m, l)?;
        Ok(m)
    }
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb88320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> CodecLimits {
        CodecLimits::default()
    }
    fn meta(id: &str, status: KeyStatus, at: u64, until: Option<u64>) -> KeyMetadata {
        KeyMetadata {
            key_id: id.into(),
            status,
            activated_at_ms: at,
            verify_until_ms: until,
        }
    }
    #[test]
    fn round_trip_lifecycle_events() {
        let events = vec![
            KeyringAuditEvent {
                sequence: 1,
                at_ms: 10,
                action: AuditAction::Initialized,
                key: meta("k1", KeyStatus::Active, 10, None),
                related: None,
            },
            KeyringAuditEvent {
                sequence: 2,
                at_ms: 20,
                action: AuditAction::Rotated,
                key: meta("k1", KeyStatus::VerifyOnly, 10, Some(100)),
                related: Some(meta("k2", KeyStatus::Active, 20, None)),
            },
            KeyringAuditEvent {
                sequence: 3,
                at_ms: 30,
                action: AuditAction::Revoked,
                key: meta("k2", KeyStatus::Revoked, 20, Some(30)),
                related: None,
            },
            KeyringAuditEvent {
                sequence: 4,
                at_ms: 31,
                action: AuditAction::Purged,
                key: meta("k1", KeyStatus::VerifyOnly, 10, Some(100)),
                related: None,
            },
        ];
        let bytes = encode(&events, &limits()).unwrap();
        assert_eq!(decode(&bytes, &limits()).unwrap(), events);
    }
    #[test]
    fn detects_corruption() {
        let e = KeyringAuditEvent {
            sequence: 1,
            at_ms: 1,
            action: AuditAction::Initialized,
            key: meta("k", KeyStatus::Active, 1, None),
            related: None,
        };
        let mut b = encode(&[e], &limits()).unwrap();
        b[12] ^= 1;
        assert_eq!(decode(&b, &limits()), Err(CodecError::ChecksumMismatch));
    }
    #[test]
    fn rejects_sequence_regression() {
        let e = |s| KeyringAuditEvent {
            sequence: s,
            at_ms: s,
            action: AuditAction::Initialized,
            key: meta(&format!("k{s}"), KeyStatus::Active, s, None),
            related: None,
        };
        assert_eq!(
            encode(&[e(2), e(1)], &limits()),
            Err(CodecError::NonIncreasingSequence)
        );
    }
    #[test]
    fn rejects_invalid_rotation_shape() {
        let e = KeyringAuditEvent {
            sequence: 1,
            at_ms: 2,
            action: AuditAction::Rotated,
            key: meta("old", KeyStatus::Active, 1, None),
            related: Some(meta("new", KeyStatus::Active, 2, None)),
        };
        assert_eq!(encode(&[e], &limits()), Err(CodecError::InvalidEvent));
    }
}
