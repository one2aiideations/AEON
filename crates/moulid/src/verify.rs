//! Checking a model file against the catalogue before it is loaded.
//!
//! A file whose name is in the catalogue must have that entry's exact size and SHA-256; if not, it
//! may have been damaged in copying or changed on purpose, and the person is told before the model
//! is loaded. A file whose name is not in the catalogue (a model the person copied on themselves)
//! cannot be checked: it is reported as unknown and loads as before.
//!
//! What this does not show: a matching hash shows the file is the one the catalogue lists, not that
//! the model is safe, good, or fit for any purpose. A file given a catalogue name by someone who
//! also changed the catalogue is not caught: the catalogue on the data disk is trusted.

use crate::catalog::Entry;
use crate::fetch::matches_entry;
use std::io;
use std::path::Path;

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Same name, size and SHA-256 as this catalogue entry.
    Verified {
        /// The catalogue id.
        id: String,
    },
    /// The name is not in the catalogue, so nothing can be said.
    Unknown,
    /// The name is in the catalogue but the file is not the same.
    Mismatch {
        /// The catalogue id.
        id: String,
        /// What differs.
        why: &'static str,
    },
}

/// Whether the catalogue knows a file by this name (so a check would be worth the time).
#[must_use]
pub fn known(file_name: &str, entries: &[Entry]) -> bool {
    entries.iter().any(|e| e.file == file_name)
}

/// Checks the file at `path`. The size is compared first, so a file that is plainly the wrong one is
/// refused without reading it; otherwise the whole file is hashed.
///
/// # Errors
/// I/O errors reading the file.
pub fn check(path: &Path, entries: &[Entry]) -> io::Result<Check> {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Ok(Check::Unknown);
    };
    let Some(entry) = entries.iter().find(|e| e.file == name) else {
        return Ok(Check::Unknown);
    };
    let size = std::fs::metadata(path)?.len();
    if size != entry.size {
        return Ok(Check::Mismatch {
            id: entry.id.clone(),
            why: "its size is different",
        });
    }
    Ok(if matches_entry(path, entry)? {
        Check::Verified {
            id: entry.id.clone(),
        }
    } else {
        Check::Mismatch {
            id: entry.id.clone(),
            why: "its checksum is different",
        }
    })
}

/// The warning to show for a mismatch, or `None` when there is nothing to warn about.
#[must_use]
pub fn warning(name: &str, check: &Check) -> Option<String> {
    match check {
        Check::Mismatch { id, why } => Some(format!(
            "{name} is not the catalogue's copy of {id}: {why}. It may have been damaged while copying or changed. A damaged model can fail to load or give wrong answers."
        )),
        Check::Verified { .. } | Check::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;

    fn entry(file: &str, data: &[u8]) -> Entry {
        Entry {
            id: "tiny".into(),
            file: file.into(),
            url: "https://huggingface.co/x".into(),
            size: data.len() as u64,
            sha256: Sha256::digest(data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            licence: "test".into(),
            min_ram_mb: 1,
            note: String::new(),
        }
    }

    /// A folder under the system temp directory that is removed when dropped.
    struct Tmp(std::path::PathBuf);

    impl Tmp {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn dir() -> Tmp {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "mouli-verify-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    #[test]
    fn a_file_that_matches_its_catalogue_entry_is_verified() {
        let d = dir();
        let data = b"GGUF pretend model bytes".repeat(100);
        fs::write(d.path().join("tiny.gguf"), &data).unwrap();
        let e = [entry("tiny.gguf", &data)];
        assert_eq!(
            check(&d.path().join("tiny.gguf"), &e).unwrap(),
            Check::Verified { id: "tiny".into() }
        );
        assert!(warning("tiny.gguf", &Check::Verified { id: "tiny".into() }).is_none());
    }

    #[test]
    fn a_changed_or_truncated_file_is_a_mismatch_with_the_reason() {
        let d = dir();
        let data = b"GGUF pretend model bytes".repeat(100);
        let e = [entry("tiny.gguf", &data)];
        // One byte changed: same size, different checksum.
        let mut changed = data.clone();
        changed[500] ^= 1;
        fs::write(d.path().join("tiny.gguf"), &changed).unwrap();
        let c = check(&d.path().join("tiny.gguf"), &e).unwrap();
        assert_eq!(
            c,
            Check::Mismatch {
                id: "tiny".into(),
                why: "its checksum is different"
            }
        );
        let w = warning("tiny.gguf", &c).unwrap();
        assert!(
            w.contains("not the catalogue's copy of tiny") && w.contains("checksum"),
            "{w}"
        );
        // Cut short: refused on size, without needing to hash.
        fs::write(d.path().join("tiny.gguf"), &data[..data.len() - 10]).unwrap();
        let c = check(&d.path().join("tiny.gguf"), &e).unwrap();
        assert_eq!(
            c,
            Check::Mismatch {
                id: "tiny".into(),
                why: "its size is different"
            }
        );
    }

    #[test]
    fn a_file_the_catalogue_does_not_know_is_unknown_not_refused() {
        let d = dir();
        fs::write(d.path().join("mine.gguf"), b"whatever").unwrap();
        let e = [entry("tiny.gguf", b"x")];
        assert_eq!(
            check(&d.path().join("mine.gguf"), &e).unwrap(),
            Check::Unknown
        );
        assert!(!known("mine.gguf", &e) && known("tiny.gguf", &e));
        assert!(warning("mine.gguf", &Check::Unknown).is_none());
        assert_eq!(
            check(&d.path().join("mine.gguf"), &[]).unwrap(),
            Check::Unknown
        );
    }

    #[test]
    fn an_unreadable_file_is_an_error_not_a_pass() {
        let d = dir();
        let e = [entry("gone.gguf", b"x")];
        assert!(check(&d.path().join("gone.gguf"), &e).is_err());
    }
}
