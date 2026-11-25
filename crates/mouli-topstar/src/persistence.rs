use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;

const MAGIC: &[u8; 8] = b"TOPJNL01";
const HEADER_LEN: usize = 8 + 4;
const MAX_RECORD_SIZE: u32 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRecord<T> {
    pub sequence: u64,
    pub payload: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalPosition {
    pub sequence: u64,
    pub offset: u64,
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid journal header")]
    InvalidHeader,
    #[error("record size {0} exceeds maximum")]
    RecordTooLarge(u32),
    #[error("truncated journal record at byte offset {offset}")]
    TruncatedRecord { offset: u64 },
    #[error("invalid journal record at byte offset {offset}: {source}")]
    InvalidRecord {
        offset: u64,
        source: serde_json::Error,
    },
    #[error("non-monotonic sequence: expected {expected}, found {found}")]
    NonMonotonicSequence { expected: u64, found: u64 },
}

#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: File,
    next_sequence: u64,
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;

        let metadata = file.metadata()?;
        if metadata.len() == 0 {
            file.write_all(MAGIC)?;
            file.sync_data()?;
        } else {
            if metadata.len() < MAGIC.len() as u64 {
                return Err(PersistenceError::InvalidHeader);
            }
            let mut header = [0_u8; MAGIC.len()];
            let mut reader = BufReader::new(file.try_clone()?);
            reader.read_exact(&mut header)?;
            if &header != MAGIC {
                return Err(PersistenceError::InvalidHeader);
            }
        }

        let next_sequence = Self::recover_next_sequence(&mut file)?;
        Ok(Self {
            path,
            file,
            next_sequence,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn append<T: Serialize>(
        &mut self,
        payload: &T,
    ) -> Result<JournalPosition, PersistenceError> {
        let record = JournalRecord {
            sequence: self.next_sequence,
            payload,
        };
        let bytes = serde_json::to_vec(&record)?;
        let length =
            u32::try_from(bytes.len()).map_err(|_| PersistenceError::RecordTooLarge(u32::MAX))?;
        if length > MAX_RECORD_SIZE {
            return Err(PersistenceError::RecordTooLarge(length));
        }

        let offset = self.file.metadata()?.len();
        self.file.write_all(&length.to_le_bytes())?;
        self.file.write_all(&bytes)?;
        self.file.sync_data()?;

        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("journal sequence exhausted");

        Ok(JournalPosition {
            sequence: record.sequence,
            offset,
        })
    }

    pub fn read_all<T: DeserializeOwned>(&self) -> Result<Vec<JournalRecord<T>>, PersistenceError> {
        let mut file = File::open(&self.path)?;
        let mut magic = [0_u8; MAGIC.len()];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(PersistenceError::InvalidHeader);
        }

        let mut records: Vec<JournalRecord<T>> = Vec::new();
        let mut offset = HEADER_LEN as u64;

        loop {
            let mut length_bytes = [0_u8; 4];
            match file.read_exact(&mut length_bytes) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error.into()),
            }

            let length = u32::from_le_bytes(length_bytes);
            if length > MAX_RECORD_SIZE {
                return Err(PersistenceError::RecordTooLarge(length));
            }

            let mut bytes = vec![0_u8; length as usize];
            if let Err(error) = file.read_exact(&mut bytes) {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    return Err(PersistenceError::TruncatedRecord { offset });
                }
                return Err(error.into());
            }

            let record: JournalRecord<T> = serde_json::from_slice(&bytes)
                .map_err(|source| PersistenceError::InvalidRecord { offset, source })?;

            if let Some(previous) = records.last() {
                let expected = previous.sequence.checked_add(1).ok_or(
                    PersistenceError::NonMonotonicSequence {
                        expected: u64::MAX,
                        found: record.sequence,
                    },
                )?;
                if record.sequence != expected {
                    return Err(PersistenceError::NonMonotonicSequence {
                        expected,
                        found: record.sequence,
                    });
                }
            } else if record.sequence != 0 {
                return Err(PersistenceError::NonMonotonicSequence {
                    expected: 0,
                    found: record.sequence,
                });
            }

            records.push(record);
            offset = offset
                .checked_add(4 + u64::from(length))
                .ok_or(PersistenceError::TruncatedRecord { offset })?;
        }

        Ok(records)
    }

    fn recover_next_sequence(file: &mut File) -> Result<u64, PersistenceError> {
        let records = Self::read_records_from(file)?;
        match records.last() {
            Some(record) => {
                record
                    .sequence
                    .checked_add(1)
                    .ok_or(PersistenceError::NonMonotonicSequence {
                        expected: u64::MAX,
                        found: u64::MAX,
                    })
            }
            None => Ok(0),
        }
    }

    fn read_records_from(
        file: &mut File,
    ) -> Result<Vec<JournalRecord<serde_json::Value>>, PersistenceError> {
        file.rewind()?;

        let mut magic = [0_u8; MAGIC.len()];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(PersistenceError::InvalidHeader);
        }

        let mut records: Vec<JournalRecord<serde_json::Value>> = Vec::new();
        let mut offset = HEADER_LEN as u64;

        loop {
            let mut length_bytes = [0_u8; 4];
            match file.read_exact(&mut length_bytes) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error.into()),
            }

            let length = u32::from_le_bytes(length_bytes);
            if length > MAX_RECORD_SIZE {
                return Err(PersistenceError::RecordTooLarge(length));
            }

            let mut bytes = vec![0_u8; length as usize];
            if let Err(error) = file.read_exact(&mut bytes) {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    return Err(PersistenceError::TruncatedRecord { offset });
                }
                return Err(error.into());
            }

            let record: JournalRecord<serde_json::Value> = serde_json::from_slice(&bytes)
                .map_err(|source| PersistenceError::InvalidRecord { offset, source })?;

            if let Some(previous) = records.last() {
                let expected = previous.sequence.checked_add(1).ok_or(
                    PersistenceError::NonMonotonicSequence {
                        expected: u64::MAX,
                        found: u64::MAX,
                    },
                )?;
                if record.sequence != expected {
                    return Err(PersistenceError::NonMonotonicSequence {
                        expected,
                        found: record.sequence,
                    });
                }
            } else if record.sequence != 0 {
                return Err(PersistenceError::NonMonotonicSequence {
                    expected: 0,
                    found: record.sequence,
                });
            }

            records.push(record);
            offset = offset
                .checked_add(4 + u64::from(length))
                .ok_or(PersistenceError::TruncatedRecord { offset })?;
        }

        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("topstar-journal-{nonce}.log"))
    }

    #[test]
    fn appends_and_recovers_records() {
        let path = temporary_path();

        {
            let mut journal = Journal::open(&path).expect("journal should open");
            assert_eq!(journal.next_sequence(), 0);
            assert_eq!(
                journal
                    .append(&serde_json::json!({"event": "created"}))
                    .expect("append should succeed")
                    .sequence,
                0
            );
            assert_eq!(journal.next_sequence(), 1);
            journal
                .append(&serde_json::json!({"event": "authorized"}))
                .expect("append should succeed");
        }

        let journal = Journal::open(&path).expect("journal should recover");
        let records: Vec<JournalRecord<serde_json::Value>> =
            journal.read_all().expect("records should decode");

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 0);
        assert_eq!(records[1].sequence, 1);
        assert_eq!(records[1].payload["event"], "authorized");

        std::fs::remove_file(path).expect("temporary journal should be removed");
    }

    #[test]
    fn rejects_invalid_header() {
        let path = temporary_path();
        std::fs::write(&path, b"invalid").expect("fixture should write");

        assert!(matches!(
            Journal::open(&path),
            Err(PersistenceError::InvalidHeader)
        ));

        std::fs::remove_file(path).expect("temporary fixture should be removed");
    }

    #[test]
    fn rejects_non_monotonic_sequences() {
        let path = temporary_path();

        let first = JournalRecord {
            sequence: 0_u64,
            payload: serde_json::json!({"ok": true}),
        };
        let second = JournalRecord {
            sequence: 2_u64,
            payload: serde_json::json!({"ok": true}),
        };

        let first_bytes = serde_json::to_vec(&first).expect("serialization should succeed");
        let second_bytes = serde_json::to_vec(&second).expect("serialization should succeed");
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&(first_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&first_bytes);
        bytes.extend_from_slice(&(second_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&second_bytes);
        std::fs::write(&path, bytes).expect("fixture should write");

        assert!(matches!(
            Journal::open(&path),
            Err(PersistenceError::NonMonotonicSequence {
                expected: 1,
                found: 2
            })
        ));

        std::fs::remove_file(path).expect("temporary fixture should be removed");
    }
}
