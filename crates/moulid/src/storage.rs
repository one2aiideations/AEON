//! The Storage window's host side: what is on the data disk, and removing a model from it.
//!
//! Only the data disk (MOULI-DATA) is looked at. A model can be removed only when it is a regular
//! `.gguf` file directly inside `models/` (not a link, not in a sub-folder), and the request must
//! name a row of the list that was just shown, so a stale or invented index removes nothing.

use mouli_screen::screens::{StorageKind, StorageRow};
use std::path::{Path, PathBuf};

/// Most entries listed.
const MAX_ROWS: usize = 500;
/// Most files counted inside one folder before the size stops growing.
const MAX_WALK: usize = 20_000;
/// Deepest folder level counted.
const MAX_DEPTH: usize = 12;

/// What was found on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    /// Models first (largest first), then projects, the record and the rest.
    pub rows: Vec<StorageRow>,
    /// Free bytes, when known.
    pub free: Option<u64>,
    /// Total bytes, when known.
    pub total: Option<u64>,
}

fn folder_size(dir: &Path, depth: usize, seen: &mut usize) -> u64 {
    if depth > MAX_DEPTH {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for e in entries.flatten() {
        if *seen >= MAX_WALK {
            break;
        }
        *seen += 1;
        let Ok(meta) = e.path().symlink_metadata() else {
            continue;
        };
        if meta.is_dir() {
            total = total.saturating_add(folder_size(&e.path(), depth + 1, seen));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Total and free bytes of the filesystem holding `path`.
#[cfg(target_os = "linux")]
#[allow(clippy::useless_conversion)] // the field types differ between platforms
fn disk_space(path: &Path) -> (Option<u64>, Option<u64>) {
    match nix::sys::statvfs::statvfs(path) {
        Ok(s) => {
            let unit = u64::from(s.fragment_size());
            (
                Some(u64::from(s.blocks_available()).saturating_mul(unit)),
                Some(u64::from(s.blocks()).saturating_mul(unit)),
            )
        }
        Err(_) => (None, None),
    }
}

#[cfg(not(target_os = "linux"))]
fn disk_space(_path: &Path) -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// Looks at the data disk rooted at `root`.
#[must_use]
pub fn scan(root: &Path) -> Scan {
    let mut models = Vec::new();
    let mut others = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("models")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(meta) = e.path().symlink_metadata() else {
                continue;
            };
            if meta.is_file()
                && name.to_ascii_lowercase().ends_with(".gguf")
                && !name.starts_with('.')
            {
                models.push(StorageRow {
                    name,
                    bytes: meta.len(),
                    kind: StorageKind::Model,
                });
            }
        }
    }
    models.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(&b.name)));
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "models" || name.starts_with('.') {
                continue;
            }
            let Ok(meta) = e.path().symlink_metadata() else {
                continue;
            };
            let kind = if name == crate::datagraph::LOG_NAME {
                StorageKind::Record
            } else if name == "projects" {
                StorageKind::Project
            } else {
                StorageKind::Other
            };
            let bytes = if meta.is_dir() {
                folder_size(&e.path(), 0, &mut 0)
            } else if meta.is_file() {
                meta.len()
            } else {
                continue;
            };
            others.push(StorageRow { name, bytes, kind });
        }
    }
    others.sort_by(|a, b| {
        let rank = |k: StorageKind| match k {
            StorageKind::Project => 0,
            StorageKind::Record => 1,
            _ => 2,
        };
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then(b.bytes.cmp(&a.bytes))
            .then(a.name.cmp(&b.name))
    });
    let (free, total) = disk_space(root);
    let mut rows = models;
    rows.extend(others);
    rows.truncate(MAX_ROWS);
    Scan { rows, free, total }
}

/// Why a removal did not happen, or what was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// The model file was removed (name, bytes freed).
    Removed(String, u64),
    /// It was refused or failed (a message for the person).
    Refused(String),
}

/// Removes the model shown as `shown` (the row the person chose, from the list on screen).
///
/// The row is checked again against the disk: it must still be a regular `.gguf` file directly in
/// `models/` with the same name and size as shown, so a list that went stale (another program
/// changed the file) removes nothing.
#[must_use]
pub fn remove_model(root: &Path, shown: &StorageRow) -> Removal {
    if shown.kind != StorageKind::Model {
        return Removal::Refused(format!("{} is not a model.", shown.name));
    }
    let name = &shown.name;
    if name.is_empty()
        || name.contains(['/', '\\', '\0'])
        || name == "."
        || name == ".."
        || name.starts_with('.')
        || !name.to_ascii_lowercase().ends_with(".gguf")
    {
        return Removal::Refused("That is not a model file name.".into());
    }
    let path: PathBuf = root.join("models").join(name);
    let Ok(meta) = path.symlink_metadata() else {
        return Removal::Refused(format!("{name} is no longer there."));
    };
    if !meta.is_file() {
        return Removal::Refused(format!("{name} is not a plain file, so it was left alone."));
    }
    if meta.len() != shown.bytes {
        return Removal::Refused(format!(
            "{name} changed since the list was made; look again (r)."
        ));
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Removal::Removed(name.clone(), meta.len()),
        Err(e) => Removal::Refused(format!("{name} could not be removed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-storage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("models")).unwrap();
        d
    }

    fn put(path: &Path, n: usize) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, vec![7u8; n]).unwrap();
    }

    #[test]
    fn the_scan_lists_models_projects_and_the_record_with_sizes() {
        let d = tmp("scan");
        put(&d.join("models/small.gguf"), 100);
        put(&d.join("models/BIG.GGUF"), 5000);
        put(&d.join("models/notes.txt"), 10);
        put(&d.join("models/.hidden.gguf"), 10);
        put(&d.join("projects/a/src/main.rs"), 300);
        put(&d.join("projects/b.txt"), 50);
        put(&d.join(crate::datagraph::LOG_NAME), 40);
        put(&d.join("settings"), 5);
        std::fs::create_dir_all(d.join("history")).unwrap();
        let s = scan(&d);
        let names: Vec<(&str, u64, StorageKind)> = s
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.bytes, r.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("BIG.GGUF", 5000, StorageKind::Model),
                ("small.gguf", 100, StorageKind::Model),
                ("projects", 350, StorageKind::Project),
                (crate::datagraph::LOG_NAME, 40, StorageKind::Record),
                ("settings", 5, StorageKind::Other),
                ("history", 0, StorageKind::Other),
            ]
        );
        #[cfg(target_os = "linux")]
        assert!(s.total.unwrap() > 0 && s.free.unwrap() <= s.total.unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_or_missing_disk_scans_to_nothing() {
        let d = tmp("empty");
        assert!(scan(&d).rows.is_empty());
        assert!(scan(&d.join("not-there")).rows.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed_or_counted_as_models() {
        let d = tmp("links");
        let outside = tmp("outside");
        put(&outside.join("secret.gguf"), 999);
        std::os::unix::fs::symlink(outside.join("secret.gguf"), d.join("models/link.gguf"))
            .unwrap();
        std::os::unix::fs::symlink(&outside, d.join("projects")).unwrap();
        let s = scan(&d);
        assert!(
            !s.rows.iter().any(|r| r.name == "link.gguf"),
            "{:?}",
            s.rows
        );
        assert!(
            s.rows
                .iter()
                .all(|r| r.bytes == 0 || r.kind != StorageKind::Project),
            "a linked folder is not walked"
        );
        // Even if a row named link.gguf is claimed, nothing is removed.
        let fake = StorageRow {
            name: "link.gguf".into(),
            bytes: 999,
            kind: StorageKind::Model,
        };
        assert!(matches!(remove_model(&d, &fake), Removal::Refused(m) if m.contains("plain file")));
        assert!(outside.join("secret.gguf").exists());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_model_is_removed_once_and_the_space_is_reported() {
        let d = tmp("remove");
        put(&d.join("models/a.gguf"), 123);
        let row = scan(&d).rows[0].clone();
        assert_eq!(
            remove_model(&d, &row),
            Removal::Removed("a.gguf".into(), 123)
        );
        assert!(!d.join("models/a.gguf").exists());
        assert!(
            matches!(remove_model(&d, &row), Removal::Refused(m) if m.contains("no longer there"))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_a_matching_regular_model_file_can_be_removed() {
        let d = tmp("refuse");
        put(&d.join("models/a.gguf"), 10);
        put(&d.join("models/a.txt"), 10);
        put(&d.join("keep.gguf"), 10);
        put(&d.join(crate::datagraph::LOG_NAME), 10);
        let row = |name: &str, bytes: u64, kind: StorageKind| StorageRow {
            name: name.into(),
            bytes,
            kind,
        };
        for bad in [
            row("a.gguf", 10, StorageKind::Project),
            row("a.gguf", 10, StorageKind::Record),
            row("a.txt", 10, StorageKind::Model),
            row("../keep.gguf", 10, StorageKind::Model),
            row("..", 0, StorageKind::Model),
            row(".", 0, StorageKind::Model),
            row("", 0, StorageKind::Model),
            row("sub/a.gguf", 10, StorageKind::Model),
            row("sub\\a.gguf", 10, StorageKind::Model),
            row("a\0.gguf", 10, StorageKind::Model),
            row(".hidden.gguf", 10, StorageKind::Model),
            row("/etc/passwd.gguf", 10, StorageKind::Model),
        ] {
            assert!(
                matches!(remove_model(&d, &bad), Removal::Refused(_)),
                "{bad:?}"
            );
        }
        assert!(d.join("models/a.gguf").exists() && d.join("models/a.txt").exists());
        assert!(d.join("keep.gguf").exists() && d.join(crate::datagraph::LOG_NAME).exists());
        // A stale size (the file changed) is refused.
        assert!(
            matches!(remove_model(&d, &row("a.gguf", 11, StorageKind::Model)), Removal::Refused(m) if m.contains("changed"))
        );
        assert!(d.join("models/a.gguf").exists());
        // A folder with a model-like name is not a plain file.
        std::fs::create_dir_all(d.join("models/dir.gguf")).unwrap();
        assert!(
            matches!(remove_model(&d, &row("dir.gguf", 0, StorageKind::Model)), Removal::Refused(m) if m.contains("plain file"))
        );
        assert!(d.join("models/dir.gguf").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
