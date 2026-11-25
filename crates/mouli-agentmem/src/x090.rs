//! Read-only structural and semantic verifier for the authenticated-checkpoint
//! keyring audit journal. The verifier never truncates or repairs the source.
//!
//! M088 CRC-32 framing detects accidental corruption only. This crate validates
//! lifecycle consistency, not cryptographic authenticity of the journal history.

use crate::x088 as codec;
use codec::{AuditAction, CodecError, CodecLimits, KeyMetadata, KeyStatus, KeyringAuditEvent};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const PREFIX_LEN: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyLimits {
    pub max_record_bytes: usize,
    pub max_total_bytes: u64,
    pub max_events: usize,
    pub codec: CodecLimits,
}
impl Default for VerifyLimits {
    fn default() -> Self {
        Self {
            max_record_bytes: 16_384,
            max_total_bytes: 16 * 1024 * 1024,
            max_events: 100_000,
            codec: CodecLimits::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeySummary {
    pub key_id: String,
    pub final_status: KeyStatus,
    pub activated_at_ms: u64,
    pub verify_until_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationReport {
    pub events_verified: usize,
    pub journal_bytes: u64,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
    pub active_key_id: Option<String>,
    pub key_summaries: Vec<KeySummary>,
}

#[derive(Debug)]
pub enum VerifyError {
    InvalidLimits,
    JournalTooLarge,
    TruncatedFrame {
        offset: u64,
    },
    InvalidFrameLength {
        offset: u64,
        length: usize,
    },
    CorruptRecord {
        offset: u64,
        source: CodecError,
    },
    RecordEventCount {
        offset: u64,
        count: usize,
    },
    TooManyEvents,
    SequenceNotIncreasing {
        previous: u64,
        proposed: u64,
    },
    InvalidLifecycle {
        sequence: u64,
        reason: LifecycleError,
    },
    IntegerOverflow,
    Io(std::io::Error),
}
impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for VerifyError {}
impl From<std::io::Error> for VerifyError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    InitializationNotFirst,
    DuplicateKeyId,
    NoActiveKey,
    ActiveKeyMismatch,
    UnknownKey,
    KeyAlreadyTerminal,
    MetadataMismatch,
    RotationDeadlineBeforeEvent,
    PurgeBeforeExpiry,
    InvalidActionShape,
}

#[derive(Clone)]
struct KeyState {
    status: KeyStatus,
    activated_at_ms: u64,
    verify_until_ms: Option<u64>,
}

/// Verify an existing journal file without changing its bytes or metadata.
/// The caller must prevent concurrent writers while verification is running.
pub fn verify_file(
    path: impl AsRef<Path>,
    limits: VerifyLimits,
) -> Result<VerificationReport, VerifyError> {
    validate_limits(&limits)?;
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len > limits.max_total_bytes {
        return Err(VerifyError::JournalTooLarge);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut offset = 0u64;
    let mut events = Vec::new();
    let mut previous_sequence = None;
    while offset < file_len {
        let remaining = file_len
            .checked_sub(offset)
            .ok_or(VerifyError::IntegerOverflow)?;
        if remaining < PREFIX_LEN as u64 {
            return Err(VerifyError::TruncatedFrame { offset });
        }
        let mut prefix = [0u8; PREFIX_LEN];
        file.read_exact(&mut prefix)?;
        let record_len = u32::from_be_bytes(prefix) as usize;
        if record_len == 0 || record_len > limits.max_record_bytes {
            return Err(VerifyError::InvalidFrameLength {
                offset,
                length: record_len,
            });
        }
        let frame_len = PREFIX_LEN
            .checked_add(record_len)
            .ok_or(VerifyError::IntegerOverflow)?;
        if remaining < u64::try_from(frame_len).map_err(|_| VerifyError::IntegerOverflow)? {
            return Err(VerifyError::TruncatedFrame { offset });
        }
        let mut encoded = vec![0u8; record_len];
        file.read_exact(&mut encoded)?;
        let decoded = codec::decode(&encoded, &limits.codec)
            .map_err(|source| VerifyError::CorruptRecord { offset, source })?;
        if decoded.len() != 1 {
            return Err(VerifyError::RecordEventCount {
                offset,
                count: decoded.len(),
            });
        }
        let event = decoded
            .into_iter()
            .next()
            .ok_or(VerifyError::RecordEventCount { offset, count: 0 })?;
        if let Some(previous) = previous_sequence {
            if event.sequence <= previous {
                return Err(VerifyError::SequenceNotIncreasing {
                    previous,
                    proposed: event.sequence,
                });
            }
        }
        previous_sequence = Some(event.sequence);
        events.push(event);
        if events.len() > limits.max_events {
            return Err(VerifyError::TooManyEvents);
        }
        offset = offset
            .checked_add(u64::try_from(frame_len).map_err(|_| VerifyError::IntegerOverflow)?)
            .ok_or(VerifyError::IntegerOverflow)?;
    }
    replay_lifecycle(&events, file_len)
}

/// Validate a decoded event sequence when the caller already owns the events.
pub fn verify_events(events: &[KeyringAuditEvent]) -> Result<VerificationReport, VerifyError> {
    replay_lifecycle(events, 0)
}

fn validate_limits(l: &VerifyLimits) -> Result<(), VerifyError> {
    if l.max_record_bytes < 18
        || l.max_record_bytes > u32::MAX as usize
        || l.max_total_bytes < 4
        || l.max_events == 0
        || l.codec.max_events == 0
        || l.codec.max_total_bytes < 18
    {
        return Err(VerifyError::InvalidLimits);
    }
    Ok(())
}

fn replay_lifecycle(
    events: &[KeyringAuditEvent],
    journal_bytes: u64,
) -> Result<VerificationReport, VerifyError> {
    let mut states: BTreeMap<String, KeyState> = BTreeMap::new();
    let mut active: Option<String> = None;
    let mut previous_sequence = None;
    for event in events {
        if let Some(previous) = previous_sequence {
            if event.sequence <= previous {
                return Err(VerifyError::SequenceNotIncreasing {
                    previous,
                    proposed: event.sequence,
                });
            }
        }
        previous_sequence = Some(event.sequence);
        apply_event(event, &mut states, &mut active).map_err(|reason| {
            VerifyError::InvalidLifecycle {
                sequence: event.sequence,
                reason,
            }
        })?;
    }
    let key_summaries = states
        .into_iter()
        .map(|(key_id, state)| KeySummary {
            key_id,
            final_status: state.status,
            activated_at_ms: state.activated_at_ms,
            verify_until_ms: state.verify_until_ms,
        })
        .collect();
    Ok(VerificationReport {
        events_verified: events.len(),
        journal_bytes,
        first_sequence: events.first().map(|e| e.sequence),
        last_sequence: events.last().map(|e| e.sequence),
        active_key_id: active,
        key_summaries,
    })
}

fn apply_event(
    event: &KeyringAuditEvent,
    states: &mut BTreeMap<String, KeyState>,
    active: &mut Option<String>,
) -> Result<(), LifecycleError> {
    match event.action {
        AuditAction::Initialized => {
            if !states.is_empty() {
                return Err(LifecycleError::InitializationNotFirst);
            }
            if event.key.status != KeyStatus::Active || event.related.is_some() {
                return Err(LifecycleError::InvalidActionShape);
            }
            if states.contains_key(&event.key.key_id) {
                return Err(LifecycleError::DuplicateKeyId);
            }
            states.insert(event.key.key_id.clone(), state_from(&event.key));
            *active = Some(event.key.key_id.clone());
        }
        AuditAction::Rotated => {
            let old_id = event.key.key_id.as_str();
            let new = event
                .related
                .as_ref()
                .ok_or(LifecycleError::InvalidActionShape)?;
            if active.as_deref() != Some(old_id) {
                return Err(LifecycleError::ActiveKeyMismatch);
            }
            let old_state = states.get(old_id).ok_or(LifecycleError::UnknownKey)?;
            if old_state.status != KeyStatus::Active {
                return Err(LifecycleError::KeyAlreadyTerminal);
            }
            if old_state.activated_at_ms != event.key.activated_at_ms {
                return Err(LifecycleError::MetadataMismatch);
            }
            if event.key.status != KeyStatus::VerifyOnly
                || event
                    .key
                    .verify_until_ms
                    .map(|v| v < event.at_ms)
                    .unwrap_or(true)
            {
                return Err(LifecycleError::RotationDeadlineBeforeEvent);
            }
            if states.contains_key(&new.key_id) {
                return Err(LifecycleError::DuplicateKeyId);
            }
            if new.status != KeyStatus::Active
                || new.activated_at_ms != event.at_ms
                || new.verify_until_ms.is_some()
            {
                return Err(LifecycleError::InvalidActionShape);
            }
            states.insert(old_id.to_owned(), state_from(&event.key));
            states.insert(new.key_id.clone(), state_from(new));
            *active = Some(new.key_id.clone());
        }
        AuditAction::Revoked => {
            let id = event.key.key_id.as_str();
            let previous = states.get(id).ok_or(LifecycleError::UnknownKey)?;
            if !matches!(previous.status, KeyStatus::Active | KeyStatus::VerifyOnly) {
                return Err(LifecycleError::KeyAlreadyTerminal);
            }
            if previous.activated_at_ms != event.key.activated_at_ms {
                return Err(LifecycleError::MetadataMismatch);
            }
            if event.key.status != KeyStatus::Revoked
                || event.key.verify_until_ms != Some(event.at_ms)
            {
                return Err(LifecycleError::InvalidActionShape);
            }
            states.insert(id.to_owned(), state_from(&event.key));
            if active.as_deref() == Some(id) {
                *active = None;
            }
        }
        AuditAction::Purged => {
            let id = event.key.key_id.as_str();
            let previous = states.get(id).ok_or(LifecycleError::UnknownKey)?;
            if !matches!(previous.status, KeyStatus::Revoked | KeyStatus::VerifyOnly) {
                return Err(LifecycleError::KeyAlreadyTerminal);
            }
            if previous.status != event.key.status
                || previous.activated_at_ms != event.key.activated_at_ms
                || previous.verify_until_ms != event.key.verify_until_ms
            {
                return Err(LifecycleError::MetadataMismatch);
            }
            if previous.status == KeyStatus::VerifyOnly
                && event.at_ms < previous.verify_until_ms.unwrap_or(u64::MAX)
            {
                return Err(LifecycleError::PurgeBeforeExpiry);
            }
            if previous.status == KeyStatus::Revoked
                && event.at_ms < previous.verify_until_ms.unwrap_or(u64::MAX)
            {
                return Err(LifecycleError::PurgeBeforeExpiry);
            }
            states.remove(id);
        }
    }
    Ok(())
}

fn state_from(metadata: &KeyMetadata) -> KeyState {
    KeyState {
        status: metadata.status,
        activated_at_ms: metadata.activated_at_ms,
        verify_until_ms: metadata.verify_until_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec::{AuditAction, KeyMetadata};

    fn meta(id: &str, status: KeyStatus, activated: u64, until: Option<u64>) -> KeyMetadata {
        KeyMetadata {
            key_id: id.to_owned(),
            status,
            activated_at_ms: activated,
            verify_until_ms: until,
        }
    }
    fn initialized() -> KeyringAuditEvent {
        KeyringAuditEvent {
            sequence: 1,
            at_ms: 10,
            action: AuditAction::Initialized,
            key: meta("k1", KeyStatus::Active, 10, None),
            related: None,
        }
    }
    fn rotated() -> KeyringAuditEvent {
        KeyringAuditEvent {
            sequence: 2,
            at_ms: 20,
            action: AuditAction::Rotated,
            key: meta("k1", KeyStatus::VerifyOnly, 10, Some(50)),
            related: Some(meta("k2", KeyStatus::Active, 20, None)),
        }
    }

    #[test]
    fn accepts_valid_initialization_and_rotation() {
        let report = verify_events(&[initialized(), rotated()]).unwrap();
        assert_eq!(report.events_verified, 2);
        assert_eq!(report.active_key_id.as_deref(), Some("k2"));
        assert_eq!(report.key_summaries.len(), 2);
    }

    #[test]
    fn rejects_rotation_of_non_active_key() {
        let mut event = rotated();
        event.key.key_id = "unknown".to_owned();
        assert!(matches!(
            verify_events(&[initialized(), event]),
            Err(VerifyError::InvalidLifecycle {
                reason: LifecycleError::ActiveKeyMismatch,
                ..
            })
        ));
    }

    #[test]
    fn rejects_reused_key_identifier() {
        let mut event = rotated();
        event.related = Some(meta("k1", KeyStatus::Active, 20, None));
        assert!(matches!(
            verify_events(&[initialized(), event]),
            Err(VerifyError::InvalidLifecycle {
                reason: LifecycleError::DuplicateKeyId,
                ..
            })
        ));
    }

    #[test]
    fn rejects_purge_before_verification_deadline() {
        let mut purge = rotated();
        purge.sequence = 3;
        purge.at_ms = 49;
        purge.action = AuditAction::Purged;
        purge.key = meta("k1", KeyStatus::VerifyOnly, 10, Some(50));
        purge.related = None;
        assert!(matches!(
            verify_events(&[initialized(), rotated(), purge]),
            Err(VerifyError::InvalidLifecycle {
                reason: LifecycleError::PurgeBeforeExpiry,
                ..
            })
        ));
    }
}
