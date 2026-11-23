//! The data partition: settings and chat history that survive a reboot.
//!
//! AEON runs from memory, so nothing is saved unless the user provides a filesystem labelled
//! `AEON-DATA` (ext4 or FAT, on the same USB stick after the image or on any other disk). It is
//! mounted read-write at `/aeon/data`. Models may also live in `/aeon/data/models`. Without it
//! the system works exactly as before and says that nothing will be saved.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Volume label that marks the data partition.
pub const DATA_LABEL: &str = "AEON-DATA";
/// Where it is mounted.
pub const DATA_MOUNT: &str = "/aeon/data";

/// Label of the device `/dev/<name>`.
#[must_use]
pub fn device_label(name: &str) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(format!("/dev/{name}"))
        .ok()?
        .take(crate::fslabel::PROBE_BYTES as u64)
        .read_to_end(&mut head)
        .ok()?;
    crate::fslabel::read_label(&head)
}

/// Find and mount the data partition read-write. Returns its mount point.
#[cfg(target_os = "linux")]
pub fn mount_data() -> Option<PathBuf> {
    use nix::mount::{mount, MsFlags};
    let table = std::fs::read_to_string("/proc/partitions").unwrap_or_default();
    let names: Vec<String> = crate::models::candidate_partitions(&table)
        .into_iter()
        .filter(|n| !n.starts_with("sr"))
        .collect();
    let dev = crate::fslabel::find_labelled(&names, DATA_LABEL, device_label)?;
    let target = PathBuf::from(DATA_MOUNT);
    std::fs::create_dir_all(&target).ok()?;
    let src = format!("/dev/{dev}");
    ["ext4", "vfat", "exfat", "btrfs", "xfs"]
        .iter()
        .any(|fs| {
            mount(
                Some(src.as_str()),
                &target,
                Some(*fs),
                MsFlags::MS_NOATIME,
                None::<&str>,
            )
            .is_ok()
        })
        .then_some(target)
}

/// Development-mode stand-in.
#[cfg(not(target_os = "linux"))]
pub fn mount_data() -> Option<PathBuf> {
    None
}

/// Flush and unmount the data partition so the filesystem is clean before power-off.
#[cfg(target_os = "linux")]
pub fn unmount(path: &Path) {
    nix::unistd::sync();
    let _ = nix::mount::umount(path);
}

/// Development-mode stand-in.
#[cfg(not(target_os = "linux"))]
pub fn unmount(_path: &Path) {}

/// Settings and history stored under one directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o
}

impl Store {
    /// Use `root`, creating `history/` and `models/` inside it.
    ///
    /// # Errors
    /// I/O errors, for example a read-only filesystem.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root.join("history"))?;
        std::fs::create_dir_all(root.join("models"))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// The directory where models may be kept.
    #[must_use]
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    /// The mount root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All settings. A missing file, or a line that is not `key=value`, is skipped.
    #[must_use]
    pub fn settings(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.root.join("settings"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, _)| valid_key(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Save one setting. The file is replaced atomically (written beside it, synced, renamed),
    /// so a power cut leaves the old or the new content, never half of one.
    ///
    /// # Errors
    /// [`std::io::ErrorKind::InvalidInput`] for a bad key or a value with a line break; I/O
    /// errors otherwise.
    pub fn set(&self, key: &str, value: &str) -> std::io::Result<()> {
        if !valid_key(key) || value.contains(['\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "bad setting key or value",
            ));
        }
        let mut all = self.settings();
        all.insert(key.into(), value.into());
        let tmp = self.root.join("settings.tmp");
        let mut f = std::fs::File::create(&tmp)?;
        for (k, v) in &all {
            writeln!(f, "{k}={v}")?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, self.root.join("settings"))
    }

    /// Name for a new chat log: `chat-0001`, `chat-0002`, ... (no clock is assumed).
    #[must_use]
    pub fn new_session(&self) -> String {
        let last = std::fs::read_dir(self.root.join("history"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.strip_prefix("chat-")?
                    .strip_suffix(".log")?
                    .parse::<u32>()
                    .ok()
            })
            .max()
            .unwrap_or(0);
        format!("chat-{:04}", last + 1)
    }

    /// Append one turn to a chat log as `role<TAB>text` with line breaks escaped.
    ///
    /// # Errors
    /// I/O errors.
    pub fn log_turn(&self, session: &str, role: &str, text: &str) -> std::io::Result<()> {
        if session.is_empty()
            || !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "bad session name",
            ));
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("history").join(format!("{session}.log")))?;
        writeln!(f, "{role}\t{}", escape(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aeond-persist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn settings_round_trip_and_validation() {
        let d = tmp("settings");
        let s = Store::open(&d).unwrap();
        assert!(s.settings().is_empty());
        s.set("last_model", "tiny.gguf").unwrap();
        s.set("theme", "dark").unwrap();
        s.set("last_model", "other = model.gguf").unwrap();
        let got = Store::open(&d).unwrap().settings();
        assert_eq!(got["last_model"], "other = model.gguf");
        assert_eq!(got["theme"], "dark");
        for bad in ["", "Upper", "a b", "a=b"] {
            assert!(s.set(bad, "v").is_err(), "{bad:?}");
        }
        assert!(s.set("k", "two\nlines").is_err());
        assert!(!d.join("settings.tmp").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn bad_lines_in_the_settings_file_are_ignored() {
        let d = tmp("badlines");
        let s = Store::open(&d).unwrap();
        std::fs::write(d.join("settings"), "ok=1\nnonsense\nBAD KEY=2\n=3\n").unwrap();
        assert_eq!(s.settings().len(), 1);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn history_sessions_count_up_and_text_is_escaped() {
        let d = tmp("history");
        let s = Store::open(&d).unwrap();
        let a = s.new_session();
        assert_eq!(a, "chat-0001");
        s.log_turn(&a, "user", "hello\nthere\\").unwrap();
        s.log_turn(&a, "assistant", "hi\tyou").unwrap();
        assert_eq!(s.new_session(), "chat-0002");
        let text = std::fs::read_to_string(d.join("history/chat-0001.log")).unwrap();
        assert_eq!(text, "user\thello\\nthere\\\\\nassistant\thi\\tyou\n");
        assert!(s.log_turn("../escape", "user", "x").is_err());
        assert!(s.log_turn("", "user", "x").is_err());
        assert_eq!(s.models_dir(), d.join("models"));
        assert!(s.models_dir().is_dir());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn open_fails_on_a_path_that_cannot_be_a_directory() {
        let d = tmp("file");
        std::fs::write(&d, b"x").unwrap();
        assert!(Store::open(&d).is_err());
        std::fs::remove_file(&d).unwrap();
    }
}
