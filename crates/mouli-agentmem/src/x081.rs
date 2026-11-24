//! Durable atomic file storage for M080 audit-report replay checkpoints.
//!
//! The store validates complete checkpoint bytes before returning them. Saves
//! use optimistic generation checks, a synced sibling staging file, atomic
//! rename, and parent-directory sync. Writers must be externally serialized:
//! the generation precondition is not a cross-process lock or CAS primitive.

use crate::x080::{
    decode, encode, ReplayCheckpoint, ReplayCheckpointError, ReplayCheckpointLimits,
};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayCheckpointStoreLimits {
    pub codec: ReplayCheckpointLimits,
    pub max_file_bytes: usize,
}

impl Default for ReplayCheckpointStoreLimits {
    fn default() -> Self {
        Self {
            codec: ReplayCheckpointLimits::default(),
            max_file_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayCheckpointMetadata {
    pub generation: u64,
    pub principal_count: usize,
    pub maximum_sequence: u64,
    pub encoded_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    InvalidLimits,
    NotFound,
    AlreadyExists,
    GenerationConflict {
        expected: Option<u64>,
        actual: Option<u64>,
    },
    GenerationNotIncreasing {
        current: u64,
        proposed: u64,
    },
    TooLarge,
    InvalidPath,
    TemporaryFileCollision,
    Codec(ReplayCheckpointError),
    Io(String),
}

impl From<ReplayCheckpointError> for StoreError {
    fn from(value: ReplayCheckpointError) -> Self {
        Self::Codec(value)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

/// Loads a checkpoint, rejecting oversized, truncated, or invalid files.
pub fn load(
    path: impl AsRef<Path>,
    limits: &ReplayCheckpointStoreLimits,
) -> Result<(ReplayCheckpoint, ReplayCheckpointMetadata), StoreError> {
    validate_limits(limits)?;
    let path = path.as_ref();
    let metadata = fs::metadata(path).map_err(map_not_found)?;
    if !metadata.is_file() {
        return Err(StoreError::InvalidPath);
    }
    let file_len = usize::try_from(metadata.len()).map_err(|_| StoreError::TooLarge)?;
    if file_len > limits.max_file_bytes {
        return Err(StoreError::TooLarge);
    }

    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(file_len);
    file.take(
        u64::try_from(limits.max_file_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_file_bytes {
        return Err(StoreError::TooLarge);
    }
    let checkpoint = decode(&bytes, None, &limits.codec)?;
    let result = metadata_for(&checkpoint, bytes.len());
    Ok((checkpoint, result))
}

/// Publishes a checkpoint after checking its generation against the current file.
///
/// `expected_generation = None` requires an absent destination. `Some(g)`
/// requires a readable checkpoint at generation `g`. New generations must be
/// strictly increasing. The staging file is synced before atomic rename, then
/// the containing directory is synced to confirm publication durability.
pub fn save(
    path: impl AsRef<Path>,
    checkpoint: &ReplayCheckpoint,
    expected_generation: Option<u64>,
    limits: &ReplayCheckpointStoreLimits,
) -> Result<ReplayCheckpointMetadata, StoreError> {
    validate_limits(limits)?;
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() || path.file_name().is_none() {
        return Err(StoreError::InvalidPath);
    }

    let current = match load(path, limits) {
        Ok((existing, _)) => Some(existing.generation),
        Err(StoreError::NotFound) => None,
        Err(error) => return Err(error),
    };
    if current != expected_generation {
        return Err(StoreError::GenerationConflict {
            expected: expected_generation,
            actual: current,
        });
    }
    if let Some(generation) = current {
        if checkpoint.generation <= generation {
            return Err(StoreError::GenerationNotIncreasing {
                current: generation,
                proposed: checkpoint.generation,
            });
        }
    }

    let bytes = encode(checkpoint, &limits.codec)?;
    if bytes.len() > limits.max_file_bytes {
        return Err(StoreError::TooLarge);
    }
    let temporary = reserve_temp_sibling(path)?;
    if let Err(error) = write_synced(&temporary, &bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(StoreError::Io(error.to_string()));
    }
    File::open(parent)?.sync_all()?;
    Ok(metadata_for(checkpoint, bytes.len()))
}

/// Creates a checkpoint only when the destination does not already exist.
/// Unlike `save`, this operation never replaces a pre-existing destination.
pub fn create_new(
    path: impl AsRef<Path>,
    checkpoint: &ReplayCheckpoint,
    limits: &ReplayCheckpointStoreLimits,
) -> Result<ReplayCheckpointMetadata, StoreError> {
    validate_limits(limits)?;
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() || path.file_name().is_none() {
        return Err(StoreError::InvalidPath);
    }
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(StoreError::AlreadyExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(StoreError::Io(error.to_string())),
    }
    let bytes = encode(checkpoint, &limits.codec)?;
    if bytes.len() > limits.max_file_bytes {
        return Err(StoreError::TooLarge);
    }
    let temporary = reserve_temp_sibling(path)?;
    if let Err(error) = write_synced(&temporary, &bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    // A hard link provides no-overwrite publication; it must remain on the same filesystem.
    if let Err(error) = fs::hard_link(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(StoreError::AlreadyExists);
        }
        return Err(StoreError::Io(error.to_string()));
    }
    fs::remove_file(&temporary)?;
    File::open(parent)?.sync_all()?;
    Ok(metadata_for(checkpoint, bytes.len()))
}

fn metadata_for(checkpoint: &ReplayCheckpoint, encoded_bytes: usize) -> ReplayCheckpointMetadata {
    ReplayCheckpointMetadata {
        generation: checkpoint.generation,
        principal_count: checkpoint.principal_count(),
        maximum_sequence: checkpoint.maximum_sequence(),
        encoded_bytes,
    }
}

fn validate_limits(limits: &ReplayCheckpointStoreLimits) -> Result<(), StoreError> {
    if limits.max_file_bytes == 0 || limits.max_file_bytes > limits.codec.max_encoded_bytes {
        return Err(StoreError::InvalidLimits);
    }
    Ok(())
}

fn map_not_found(error: std::io::Error) -> StoreError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StoreError::NotFound
    } else {
        StoreError::Io(error.to_string())
    }
}

fn reserve_temp_sibling(destination: &Path) -> Result<PathBuf, StoreError> {
    for _ in 0..64 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = destination.as_os_str().to_os_string();
        name.push(format!(
            ".agentos-audit-replay-checkpoint-{}-{sequence}.tmp",
            std::process::id()
        ));
        let candidate = PathBuf::from(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(StoreError::Io(error.to_string())),
        }
    }
    Err(StoreError::TemporaryFileCollision)
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x080::ReplayPrincipal;
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|v| v.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "agentos-m081-{label}-{}-{nonce}.checkpoint",
            std::process::id()
        ))
    }
    fn checkpoint(generation: u64, sequence: u64) -> ReplayCheckpoint {
        ReplayCheckpoint::new(
            generation,
            BTreeMap::from([(
                ReplayPrincipal {
                    consumer_id: "consumer-a".into(),
                    key_id: "key-a".into(),
                },
                sequence,
            )]),
        )
    }

    #[test]
    fn create_load_and_cas_update() {
        let file = path("cas");
        let limits = ReplayCheckpointStoreLimits::default();
        let first = checkpoint(1, 12);
        assert!(create_new(&file, &first, &limits).is_ok());
        let loaded = load(&file, &limits);
        assert_eq!(loaded.map(|v| v.0), Ok(first));
        let second = checkpoint(2, 17);
        assert!(save(&file, &second, Some(1), &limits).is_ok());
        assert_eq!(load(&file, &limits).map(|v| v.0), Ok(second));
        let _ = fs::remove_file(file);
    }

    #[test]
    fn rejects_stale_generation_and_existing_create() {
        let file = path("stale");
        let limits = ReplayCheckpointStoreLimits::default();
        assert!(create_new(&file, &checkpoint(3, 8), &limits).is_ok());
        assert_eq!(
            create_new(&file, &checkpoint(4, 9), &limits),
            Err(StoreError::AlreadyExists)
        );
        assert!(matches!(
            save(&file, &checkpoint(4, 9), Some(2), &limits),
            Err(StoreError::GenerationConflict { .. })
        ));
        assert!(matches!(
            save(&file, &checkpoint(3, 10), Some(3), &limits),
            Err(StoreError::GenerationNotIncreasing { .. })
        ));
        let _ = fs::remove_file(file);
    }

    #[test]
    fn rejects_corruption_without_rewriting_file() {
        let file = path("corrupt");
        let limits = ReplayCheckpointStoreLimits::default();
        assert!(create_new(&file, &checkpoint(1, 2), &limits).is_ok());
        let mut bytes = fs::read(&file).unwrap_or_default();
        if let Some(byte) = bytes.get_mut(6) {
            *byte ^= 0x40;
        }
        assert!(fs::write(&file, &bytes).is_ok());
        let before = fs::read(&file).unwrap_or_default();
        assert!(matches!(load(&file, &limits), Err(StoreError::Codec(_))));
        assert_eq!(fs::read(&file).unwrap_or_default(), before);
        let _ = fs::remove_file(file);
    }

    #[test]
    fn enforces_file_limit() {
        let file = path("limit");
        let limits = ReplayCheckpointStoreLimits {
            max_file_bytes: 20,
            ..ReplayCheckpointStoreLimits::default()
        };
        assert_eq!(
            create_new(&file, &checkpoint(1, 2), &limits),
            Err(StoreError::TooLarge)
        );
        assert!(!file.exists());
    }
}
