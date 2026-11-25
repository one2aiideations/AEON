//! Durable append-only journal for M088 checkpoint-keyring lifecycle audit events.
//!
//! Each record is a length-prefixed, independently checksummed M088 event envelope.
//! Appends are synced before the in-memory sequence watermark advances. Recovery
//! truncates only an incomplete final frame; complete malformed records fail closed.
//! This journal is single-process/single-writer and does not authenticate its history.

use crate::x088 as codec;
use codec::{CodecError, CodecLimits, KeyringAuditEvent};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const FRAME_PREFIX: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalLimits {
    pub max_record_bytes: usize,
    pub max_total_bytes: u64,
    pub codec: CodecLimits,
}
impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_record_bytes: 16_384,
            max_total_bytes: 16 * 1024 * 1024,
            codec: CodecLimits::default(),
        }
    }
}

#[derive(Debug)]
pub enum JournalError {
    InvalidLimits,
    RecordTooLarge,
    JournalFull,
    SequenceNotIncreasing { previous: u64, proposed: u64 },
    CorruptRecord { offset: u64, source: CodecError },
    InvalidRecordCount,
    IntegerOverflow,
    Io(std::io::Error),
}
impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for JournalError {}
impl From<std::io::Error> for JournalError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    pub events: Vec<KeyringAuditEvent>,
    pub truncated_tail_bytes: u64,
    pub journal_bytes: u64,
    pub last_sequence: Option<u64>,
}

pub struct KeyringAuditJournal {
    path: PathBuf,
    file: File,
    limits: JournalLimits,
    events: Vec<KeyringAuditEvent>,
    last_sequence: Option<u64>,
    bytes: u64,
}

impl KeyringAuditJournal {
    /// Opens or creates a journal and performs recovery before returning it.
    /// The caller must ensure exclusive process ownership of `path`.
    pub fn open(
        path: impl AsRef<Path>,
        limits: JournalLimits,
    ) -> Result<(Self, RecoveryReport), JournalError> {
        validate_limits(&limits)?;
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false) // keep existing records: they are recovered below
            .open(&path)?;
        let report = recover_file(&mut file, &limits)?;
        let bytes = report.journal_bytes;
        let last_sequence = report.last_sequence;
        let events = report.events.clone();
        file.seek(SeekFrom::End(0))?;
        let journal = Self {
            path,
            file,
            limits,
            events,
            last_sequence,
            bytes,
        };
        Ok((journal, report))
    }

    /// Durably appends one event. Memory state advances only after `sync_data` succeeds.
    pub fn append(&mut self, event: KeyringAuditEvent) -> Result<(), JournalError> {
        if let Some(previous) = self.last_sequence {
            if event.sequence <= previous {
                return Err(JournalError::SequenceNotIncreasing {
                    previous,
                    proposed: event.sequence,
                });
            }
        }
        let encoded =
            codec::encode(std::slice::from_ref(&event), &self.limits.codec).map_err(|source| {
                JournalError::CorruptRecord {
                    offset: self.bytes,
                    source,
                }
            })?;
        if encoded.len() > self.limits.max_record_bytes {
            return Err(JournalError::RecordTooLarge);
        }
        let frame_len = FRAME_PREFIX
            .checked_add(encoded.len())
            .ok_or(JournalError::IntegerOverflow)?;
        let next_bytes = self
            .bytes
            .checked_add(u64::try_from(frame_len).map_err(|_| JournalError::IntegerOverflow)?)
            .ok_or(JournalError::IntegerOverflow)?;
        if next_bytes > self.limits.max_total_bytes {
            return Err(JournalError::JournalFull);
        }
        let length = u32::try_from(encoded.len()).map_err(|_| JournalError::IntegerOverflow)?;
        let start = self.file.seek(SeekFrom::End(0))?;
        let result = (|| -> Result<(), std::io::Error> {
            self.file.write_all(&length.to_be_bytes())?;
            self.file.write_all(&encoded)?;
            self.file.sync_data()?;
            Ok(())
        })();
        if let Err(error) = result {
            // Best-effort rollback prevents a failed append from leaving bytes that a
            // subsequent append could mistake for a complete record.
            let rollback = self.file.set_len(start).and_then(|_| self.file.sync_data());
            if rollback.is_err() {
                return Err(JournalError::Io(error));
            }
            return Err(JournalError::Io(error));
        }
        self.bytes = next_bytes;
        self.last_sequence = Some(event.sequence);
        self.events.push(event);
        Ok(())
    }

    pub fn events(&self) -> &[KeyringAuditEvent] {
        &self.events
    }
    pub fn last_sequence(&self) -> Option<u64> {
        self.last_sequence
    }
    pub fn journal_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn validate_limits(l: &JournalLimits) -> Result<(), JournalError> {
    if l.max_record_bytes < 18
        || l.max_total_bytes < 4
        || l.max_record_bytes > u32::MAX as usize
        || l.codec.max_total_bytes < 18
        || l.codec.max_events == 0
    {
        return Err(JournalError::InvalidLimits);
    }
    Ok(())
}

fn recover_file(file: &mut File, limits: &JournalLimits) -> Result<RecoveryReport, JournalError> {
    let file_len = file.metadata()?.len();
    if file_len > limits.max_total_bytes {
        return Err(JournalError::JournalFull);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut events = Vec::new();
    let mut offset = 0u64;
    let mut truncated = 0u64;
    let mut last_sequence = None;
    loop {
        if offset == file_len {
            break;
        }
        let remaining = file_len
            .checked_sub(offset)
            .ok_or(JournalError::IntegerOverflow)?;
        if remaining < FRAME_PREFIX as u64 {
            truncated = remaining;
            file.set_len(offset)?;
            file.sync_data()?;
            break;
        }
        let mut prefix = [0u8; 4];
        file.read_exact(&mut prefix)?;
        let record_len = u32::from_be_bytes(prefix) as usize;
        if record_len == 0 || record_len > limits.max_record_bytes {
            return Err(JournalError::CorruptRecord {
                offset,
                source: CodecError::TooLarge,
            });
        }
        let frame_size = FRAME_PREFIX
            .checked_add(record_len)
            .ok_or(JournalError::IntegerOverflow)?;
        if remaining < frame_size as u64 {
            truncated = remaining;
            file.set_len(offset)?;
            file.sync_data()?;
            break;
        }
        let mut encoded = vec![0u8; record_len];
        file.read_exact(&mut encoded)?;
        let decoded = codec::decode(&encoded, &limits.codec)
            .map_err(|source| JournalError::CorruptRecord { offset, source })?;
        if decoded.len() != 1 {
            return Err(JournalError::InvalidRecordCount);
        }
        let event = decoded
            .into_iter()
            .next()
            .ok_or(JournalError::InvalidRecordCount)?;
        if let Some(previous) = last_sequence {
            if event.sequence <= previous {
                return Err(JournalError::SequenceNotIncreasing {
                    previous,
                    proposed: event.sequence,
                });
            }
        }
        last_sequence = Some(event.sequence);
        events.push(event);
        offset = offset
            .checked_add(frame_size as u64)
            .ok_or(JournalError::IntegerOverflow)?;
    }
    file.seek(SeekFrom::End(0))?;
    Ok(RecoveryReport {
        events,
        truncated_tail_bytes: truncated,
        journal_bytes: offset,
        last_sequence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec::{AuditAction, KeyMetadata, KeyStatus};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn path() -> PathBuf {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("agentos-m089-{}-{n}.journal", std::process::id()))
    }
    fn event(sequence: u64, key_id: &str) -> KeyringAuditEvent {
        KeyringAuditEvent {
            sequence,
            at_ms: sequence * 10,
            action: AuditAction::Initialized,
            key: KeyMetadata {
                key_id: key_id.to_owned(),
                status: KeyStatus::Active,
                activated_at_ms: sequence * 10,
                verify_until_ms: None,
            },
            related: None,
        }
    }

    #[test]
    fn append_and_reopen_restores_watermark() {
        let p = path();
        {
            let (mut j, _) = KeyringAuditJournal::open(&p, JournalLimits::default()).unwrap();
            j.append(event(1, "key-a")).unwrap();
            j.append(event(2, "key-b")).unwrap();
        }
        let (j, report) = KeyringAuditJournal::open(&p, JournalLimits::default()).unwrap();
        assert_eq!(j.last_sequence(), Some(2));
        assert_eq!(report.events.len(), 2);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn incomplete_final_frame_is_truncated() {
        let p = path();
        {
            let (mut j, _) = KeyringAuditJournal::open(&p, JournalLimits::default()).unwrap();
            j.append(event(1, "key-a")).unwrap();
        }
        let good = std::fs::metadata(&p).unwrap().len();
        {
            let mut f = OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(&[0, 0, 0, 20, 1, 2]).unwrap();
        }
        let (_, report) = KeyringAuditJournal::open(&p, JournalLimits::default()).unwrap();
        assert_eq!(report.truncated_tail_bytes, 6);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), good);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn duplicate_or_regressing_sequence_is_rejected() {
        let p = path();
        let (mut j, _) = KeyringAuditJournal::open(&p, JournalLimits::default()).unwrap();
        j.append(event(4, "key-a")).unwrap();
        assert!(matches!(
            j.append(event(4, "key-b")),
            Err(JournalError::SequenceNotIncreasing { .. })
        ));
        assert!(matches!(
            j.append(event(3, "key-c")),
            Err(JournalError::SequenceNotIncreasing { .. })
        ));
        drop(j);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn complete_corruption_fails_closed() {
        let p = path();
        {
            let (mut j, _) = KeyringAuditJournal::open(&p, JournalLimits::default()).unwrap();
            j.append(event(1, "key-a")).unwrap();
        }
        let mut bytes = std::fs::read(&p).unwrap();
        let last = bytes.len().saturating_sub(1);
        if let Some(b) = bytes.get_mut(last) {
            *b ^= 0x80;
        }
        std::fs::write(&p, bytes).unwrap();
        assert!(matches!(
            KeyringAuditJournal::open(&p, JournalLimits::default()),
            Err(JournalError::CorruptRecord { .. })
        ));
        let _ = std::fs::remove_file(p);
    }
}
