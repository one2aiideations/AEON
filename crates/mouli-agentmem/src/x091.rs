//! Deterministic bounded codec for M090 keyring-audit verification reports.
//!
//! Reports contain non-secret metadata only. CRC-32 detects accidental
//! corruption; it does not authenticate a report or its source journal.

use crate::x088::KeyStatus;
use crate::x090::{KeySummary, VerificationReport};
use std::fmt;

const MAGIC: &[u8; 4] = b"KVRP";
const VERSION: u16 = 1;
const HEADER: usize = 10;
const CHECKSUM: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportLimits {
    pub max_key_id_bytes: usize,
    pub max_summaries: usize,
    pub max_total_bytes: usize,
}
impl Default for ReportLimits {
    fn default() -> Self {
        Self {
            max_key_id_bytes: 128,
            max_summaries: 4096,
            max_total_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReportCodecError {
    InvalidLimits,
    TooLarge,
    TooManySummaries,
    Truncated,
    BadMagic,
    UnsupportedVersion(u16),
    LengthMismatch,
    ChecksumMismatch,
    InvalidUtf8,
    InvalidKeyId,
    InvalidStatus(u8),
    InvalidOption(u8),
    InvalidReport,
    NonCanonicalOrder,
    IntegerOverflow,
    TrailingBytes,
}
impl fmt::Display for ReportCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ReportCodecError {}

pub fn encode(
    report: &VerificationReport,
    limits: &ReportLimits,
) -> Result<Vec<u8>, ReportCodecError> {
    validate_limits(limits)?;
    validate_report(report, limits)?;
    if report.key_summaries.len() > limits.max_summaries {
        return Err(ReportCodecError::TooManySummaries);
    }
    let mut body = Vec::new();
    put_u64(
        &mut body,
        u64::try_from(report.events_verified).map_err(|_| ReportCodecError::IntegerOverflow)?,
    );
    put_u64(&mut body, report.journal_bytes);
    put_optional_u64(&mut body, report.first_sequence);
    put_optional_u64(&mut body, report.last_sequence);
    put_optional_string(&mut body, report.active_key_id.as_deref(), limits)?;
    put_u32(
        &mut body,
        u32::try_from(report.key_summaries.len()).map_err(|_| ReportCodecError::IntegerOverflow)?,
    );
    for summary in &report.key_summaries {
        put_string(&mut body, &summary.key_id, limits)?;
        body.push(status_byte(summary.final_status));
        put_u64(&mut body, summary.activated_at_ms);
        put_optional_u64(&mut body, summary.verify_until_ms);
        if HEADER
            .checked_add(body.len())
            .and_then(|v| v.checked_add(CHECKSUM))
            .ok_or(ReportCodecError::IntegerOverflow)?
            > limits.max_total_bytes
        {
            return Err(ReportCodecError::TooLarge);
        }
    }
    let body_len = u32::try_from(body.len()).map_err(|_| ReportCodecError::IntegerOverflow)?;
    let mut out = Vec::with_capacity(HEADER + body.len() + CHECKSUM);
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, VERSION);
    put_u32(&mut out, body_len);
    out.extend_from_slice(&body);
    let sum = crc32(&out);
    put_u32(&mut out, sum);
    if out.len() > limits.max_total_bytes {
        return Err(ReportCodecError::TooLarge);
    }
    Ok(out)
}

pub fn decode(bytes: &[u8], limits: &ReportLimits) -> Result<VerificationReport, ReportCodecError> {
    validate_limits(limits)?;
    if bytes.len() > limits.max_total_bytes {
        return Err(ReportCodecError::TooLarge);
    }
    if bytes.len() < HEADER + CHECKSUM {
        return Err(ReportCodecError::Truncated);
    }
    if bytes.get(0..4) != Some(MAGIC.as_slice()) {
        return Err(ReportCodecError::BadMagic);
    }
    let version = read_u16(bytes, 4)?;
    if version != VERSION {
        return Err(ReportCodecError::UnsupportedVersion(version));
    }
    let body_len =
        usize::try_from(read_u32(bytes, 6)?).map_err(|_| ReportCodecError::IntegerOverflow)?;
    let expected = HEADER
        .checked_add(body_len)
        .and_then(|v| v.checked_add(CHECKSUM))
        .ok_or(ReportCodecError::IntegerOverflow)?;
    if expected != bytes.len() {
        return Err(ReportCodecError::LengthMismatch);
    }
    let checksum_offset = bytes
        .len()
        .checked_sub(CHECKSUM)
        .ok_or(ReportCodecError::Truncated)?;
    let stored = read_u32(bytes, checksum_offset)?;
    if crc32(&bytes[..checksum_offset]) != stored {
        return Err(ReportCodecError::ChecksumMismatch);
    }
    let mut r = Reader {
        bytes: &bytes[HEADER..checksum_offset],
        pos: 0,
    };
    let events_verified =
        usize::try_from(r.u64()?).map_err(|_| ReportCodecError::IntegerOverflow)?;
    let journal_bytes = r.u64()?;
    let first_sequence = r.opt_u64()?;
    let last_sequence = r.opt_u64()?;
    let active_key_id = r.opt_string(limits)?;
    let count = usize::try_from(r.u32()?).map_err(|_| ReportCodecError::IntegerOverflow)?;
    if count > limits.max_summaries {
        return Err(ReportCodecError::TooManySummaries);
    }
    let mut key_summaries = Vec::with_capacity(count);
    for _ in 0..count {
        let key_id = r.string(limits)?;
        let raw_status = r.byte()?;
        let final_status = parse_status(raw_status)?;
        let activated_at_ms = r.u64()?;
        let verify_until_ms = r.opt_u64()?;
        key_summaries.push(KeySummary {
            key_id,
            final_status,
            activated_at_ms,
            verify_until_ms,
        });
    }
    if r.pos != r.bytes.len() {
        return Err(ReportCodecError::TrailingBytes);
    }
    let report = VerificationReport {
        events_verified,
        journal_bytes,
        first_sequence,
        last_sequence,
        active_key_id,
        key_summaries,
    };
    validate_report(&report, limits)?;
    Ok(report)
}

fn validate_limits(l: &ReportLimits) -> Result<(), ReportCodecError> {
    if l.max_key_id_bytes == 0
        || l.max_key_id_bytes > u16::MAX as usize
        || l.max_summaries == 0
        || l.max_summaries > u32::MAX as usize
        || l.max_total_bytes < HEADER + CHECKSUM + 8
    {
        return Err(ReportCodecError::InvalidLimits);
    }
    Ok(())
}

fn validate_report(
    report: &VerificationReport,
    limits: &ReportLimits,
) -> Result<(), ReportCodecError> {
    match (
        report.events_verified,
        report.first_sequence,
        report.last_sequence,
    ) {
        (0, None, None) => {}
        (0, _, _) | (_, None, _) | (_, _, None) => return Err(ReportCodecError::InvalidReport),
        (_, Some(first), Some(last)) if first != 0 && last >= first => {}
        _ => return Err(ReportCodecError::InvalidReport),
    }
    if report.key_summaries.len() > limits.max_summaries {
        return Err(ReportCodecError::TooManySummaries);
    }
    let mut previous: Option<&str> = None;
    let mut active_count = 0usize;
    for summary in &report.key_summaries {
        validate_key_id(&summary.key_id, limits)?;
        if let Some(p) = previous {
            if summary.key_id.as_str() <= p {
                return Err(ReportCodecError::NonCanonicalOrder);
            }
        }
        previous = Some(&summary.key_id);
        match summary.final_status {
            KeyStatus::Active => {
                active_count = active_count
                    .checked_add(1)
                    .ok_or(ReportCodecError::IntegerOverflow)?;
                if summary.verify_until_ms.is_some() {
                    return Err(ReportCodecError::InvalidReport);
                }
            }
            KeyStatus::VerifyOnly | KeyStatus::Revoked => {
                if summary.verify_until_ms.is_none() {
                    return Err(ReportCodecError::InvalidReport);
                }
            }
        }
    }
    match report.active_key_id.as_deref() {
        Some(id) => {
            validate_key_id(id, limits)?;
            if active_count != 1
                || !report
                    .key_summaries
                    .iter()
                    .any(|s| s.key_id == id && s.final_status == KeyStatus::Active)
            {
                return Err(ReportCodecError::InvalidReport);
            }
        }
        None if active_count == 0 => {}
        None => return Err(ReportCodecError::InvalidReport),
    }
    Ok(())
}

fn validate_key_id(id: &str, limits: &ReportLimits) -> Result<(), ReportCodecError> {
    if id.is_empty()
        || id.len() > limits.max_key_id_bytes
        || id.trim() != id
        || id.chars().any(char::is_control)
    {
        return Err(ReportCodecError::InvalidKeyId);
    }
    Ok(())
}
fn status_byte(s: KeyStatus) -> u8 {
    match s {
        KeyStatus::Active => 1,
        KeyStatus::VerifyOnly => 2,
        KeyStatus::Revoked => 3,
    }
}
fn parse_status(v: u8) -> Result<KeyStatus, ReportCodecError> {
    match v {
        1 => Ok(KeyStatus::Active),
        2 => Ok(KeyStatus::VerifyOnly),
        3 => Ok(KeyStatus::Revoked),
        n => Err(ReportCodecError::InvalidStatus(n)),
    }
}
fn put_optional_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(v) => {
            out.push(1);
            put_u64(out, v);
        }
        None => out.push(0),
    }
}
fn put_optional_string(
    out: &mut Vec<u8>,
    value: Option<&str>,
    limits: &ReportLimits,
) -> Result<(), ReportCodecError> {
    match value {
        Some(v) => {
            out.push(1);
            put_string(out, v, limits)?;
        }
        None => out.push(0),
    };
    Ok(())
}
fn put_string(
    out: &mut Vec<u8>,
    value: &str,
    limits: &ReportLimits,
) -> Result<(), ReportCodecError> {
    validate_key_id(value, limits)?;
    let n = u16::try_from(value.len()).map_err(|_| ReportCodecError::IntegerOverflow)?;
    put_u16(out, n);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}
fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn read_u16(b: &[u8], p: usize) -> Result<u16, ReportCodecError> {
    let s = b
        .get(p..p.checked_add(2).ok_or(ReportCodecError::IntegerOverflow)?)
        .ok_or(ReportCodecError::Truncated)?;
    let a: [u8; 2] = s.try_into().map_err(|_| ReportCodecError::Truncated)?;
    Ok(u16::from_be_bytes(a))
}
fn read_u32(b: &[u8], p: usize) -> Result<u32, ReportCodecError> {
    let s = b
        .get(p..p.checked_add(4).ok_or(ReportCodecError::IntegerOverflow)?)
        .ok_or(ReportCodecError::Truncated)?;
    let a: [u8; 4] = s.try_into().map_err(|_| ReportCodecError::Truncated)?;
    Ok(u32::from_be_bytes(a))
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ReportCodecError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(ReportCodecError::IntegerOverflow)?;
        let s = self
            .bytes
            .get(self.pos..end)
            .ok_or(ReportCodecError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn byte(&mut self) -> Result<u8, ReportCodecError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(ReportCodecError::Truncated)
    }
    fn u16(&mut self) -> Result<u16, ReportCodecError> {
        let s = self.take(2)?;
        let a: [u8; 2] = s.try_into().map_err(|_| ReportCodecError::Truncated)?;
        Ok(u16::from_be_bytes(a))
    }
    fn u32(&mut self) -> Result<u32, ReportCodecError> {
        let s = self.take(4)?;
        let a: [u8; 4] = s.try_into().map_err(|_| ReportCodecError::Truncated)?;
        Ok(u32::from_be_bytes(a))
    }
    fn u64(&mut self) -> Result<u64, ReportCodecError> {
        let s = self.take(8)?;
        let a: [u8; 8] = s.try_into().map_err(|_| ReportCodecError::Truncated)?;
        Ok(u64::from_be_bytes(a))
    }
    fn opt_u64(&mut self) -> Result<Option<u64>, ReportCodecError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            n => Err(ReportCodecError::InvalidOption(n)),
        }
    }
    fn string(&mut self, l: &ReportLimits) -> Result<String, ReportCodecError> {
        let n = usize::from(self.u16()?);
        if n == 0 || n > l.max_key_id_bytes {
            return Err(ReportCodecError::InvalidKeyId);
        }
        let value = std::str::from_utf8(self.take(n)?)
            .map_err(|_| ReportCodecError::InvalidUtf8)?
            .to_owned();
        validate_key_id(&value, l)?;
        Ok(value)
    }
    fn opt_string(&mut self, l: &ReportLimits) -> Result<Option<String>, ReportCodecError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.string(l)?)),
            n => Err(ReportCodecError::InvalidOption(n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> VerificationReport {
        VerificationReport {
            events_verified: 2,
            journal_bytes: 96,
            first_sequence: Some(1),
            last_sequence: Some(2),
            active_key_id: Some("k2".to_owned()),
            key_summaries: vec![
                KeySummary {
                    key_id: "k1".to_owned(),
                    final_status: KeyStatus::VerifyOnly,
                    activated_at_ms: 10,
                    verify_until_ms: Some(50),
                },
                KeySummary {
                    key_id: "k2".to_owned(),
                    final_status: KeyStatus::Active,
                    activated_at_ms: 20,
                    verify_until_ms: None,
                },
            ],
        }
    }
    #[test]
    fn round_trip_is_exact() {
        let value = sample();
        let bytes = encode(&value, &ReportLimits::default()).unwrap();
        assert_eq!(decode(&bytes, &ReportLimits::default()).unwrap(), value);
    }
    #[test]
    fn rejects_checksum_corruption() {
        let mut bytes = encode(&sample(), &ReportLimits::default()).unwrap();
        let n = bytes.len();
        bytes[n - 5] ^= 1;
        assert_eq!(
            decode(&bytes, &ReportLimits::default()),
            Err(ReportCodecError::ChecksumMismatch)
        );
    }
    #[test]
    fn rejects_noncanonical_summary_order() {
        let mut value = sample();
        value.key_summaries.reverse();
        assert_eq!(
            encode(&value, &ReportLimits::default()),
            Err(ReportCodecError::NonCanonicalOrder)
        );
    }
    #[test]
    fn rejects_active_key_mismatch() {
        let mut value = sample();
        value.active_key_id = Some("k1".to_owned());
        assert_eq!(
            encode(&value, &ReportLimits::default()),
            Err(ReportCodecError::InvalidReport)
        );
    }
}
