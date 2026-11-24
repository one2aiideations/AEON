//! Applying a checked patch to a real directory, and undoing it exactly.
//!
//! This is the only module that touches the file system. It never trusts a path: every path is
//! checked with [`safe_relative_path`], every existing directory on the way is checked not to be
//! a symbolic link, and the final location is confirmed to lie inside the workspace root. A patch
//! set is validated completely in memory first; if a later write fails, the files already
//! written are restored before the error is returned.

use crate::apply::{apply_patchset, safe_relative_path, ApplyError, ApplyOptions};
use crate::diff::FilePatch;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Makes temporary file names unique within a process, so concurrent writers never collide.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Why a workspace operation failed.
#[derive(Debug)]
pub enum WorkspaceError {
    /// The patch could not be applied to the current files (path, context or file-existence).
    Apply(ApplyError),
    /// A path component is a symbolic link, or the path resolves outside the root.
    Escape(String),
    /// A file is not valid UTF-8 text.
    NotText(String),
    /// A file changed on disk after the patch was applied, so rollback was refused.
    ChangedSince(String),
    /// An operating-system error.
    Io {
        /// What was being done.
        action: String,
        /// The underlying error.
        source: io::Error,
    },
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceError::Apply(e) => write!(f, "{e}"),
            WorkspaceError::Escape(p) => {
                write!(f, "'{p}' is a symbolic link or leaves the workspace")
            }
            WorkspaceError::NotText(p) => write!(f, "'{p}' is not UTF-8 text"),
            WorkspaceError::ChangedSince(p) => write!(
                f,
                "'{p}' changed after the patch, so it was not rolled back"
            ),
            WorkspaceError::Io { action, source } => write!(f, "{action}: {source}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<ApplyError> for WorkspaceError {
    fn from(e: ApplyError) -> Self {
        WorkspaceError::Apply(e)
    }
}

fn io_err(action: impl Into<String>, source: io::Error) -> WorkspaceError {
    WorkspaceError::Io {
        action: action.into(),
        source,
    }
}

/// What a successful [`Workspace::apply`] did, with everything needed to undo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// For each changed path, its content BEFORE the patch (`None` if the file was created).
    before: BTreeMap<String, Option<String>>,
    /// For each changed path, the content the patch WROTE (`None` if the file was deleted).
    after: BTreeMap<String, Option<String>>,
}

impl Receipt {
    /// The paths the patch changed.
    pub fn changed_paths(&self) -> Vec<&str> {
        self.before.keys().map(String::as_str).collect()
    }
}

/// A directory tree that patches are applied to.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Opens `root`, which must exist and is resolved to its real path.
    pub fn open(root: &Path) -> Result<Self, WorkspaceError> {
        let root =
            fs::canonicalize(root).map_err(|e| io_err(format!("open {}", root.display()), e))?;
        if !root.is_dir() {
            return Err(io_err(
                format!("open {}", root.display()),
                io::Error::other("not a directory"),
            ));
        }
        Ok(Self { root })
    }

    /// The workspace root (its real path).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The on-disk location of `rel`, after the safety checks. The file itself need not exist,
    /// but no directory on the way may be a symbolic link, and a final component that exists must
    /// not be one either.
    fn resolve(&self, rel: &str) -> Result<PathBuf, WorkspaceError> {
        let rel = safe_relative_path(rel)?;
        let mut cur = self.root.clone();
        for part in rel.split('/') {
            cur.push(part);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(WorkspaceError::Escape(rel.to_string()))
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(format!("inspect {rel}"), e)),
            }
        }
        if !cur.starts_with(&self.root) {
            return Err(WorkspaceError::Escape(rel.to_string()));
        }
        Ok(cur)
    }

    /// Reads a text file; `None` if it does not exist.
    pub fn read(&self, rel: &str) -> Result<Option<String>, WorkspaceError> {
        let path = self.resolve(rel)?;
        match fs::read(&path) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| WorkspaceError::NotText(rel.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(format!("read {rel}"), e)),
        }
    }

    /// Reads several files into a map, skipping the ones that do not exist.
    pub fn read_many<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Result<BTreeMap<String, String>, WorkspaceError> {
        let mut out = BTreeMap::new();
        for p in paths {
            if let Some(text) = self.read(p)? {
                out.insert(p.to_string(), text);
            }
        }
        Ok(out)
    }

    fn write(&self, rel: &str, content: &str) -> Result<(), WorkspaceError> {
        let path = self.resolve(rel)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| io_err(format!("create directory for {rel}"), e))?;
        }
        // Write to a temporary file in the same directory, then rename over the target, so a
        // crash never leaves a half-written file. Keep the original permissions.
        let unique = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_file_name(format!(".mouli-tmp-{}-{unique}", std::process::id()));
        fs::write(&tmp, content).map_err(|e| io_err(format!("write {rel}"), e))?;
        if let Ok(meta) = fs::metadata(&path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            io_err(format!("replace {rel}"), e)
        })
    }

    fn remove(&self, rel: &str) -> Result<(), WorkspaceError> {
        let path = self.resolve(rel)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(format!("remove {rel}"), e)),
        }
    }

    /// Applies `patches` to the files on disk, all or nothing.
    ///
    /// The current files are read, the whole patch set is applied in memory and checked, and only
    /// then are files written. If a write fails part way, everything already written is restored
    /// before the error is returned. On success the [`Receipt`] can undo the change exactly.
    pub fn apply(
        &self,
        patches: &[FilePatch],
        opts: ApplyOptions,
    ) -> Result<Receipt, WorkspaceError> {
        let paths: Vec<String> = patches.iter().map(|p| p.path().to_string()).collect();
        for p in &paths {
            self.resolve(p)?;
        }
        let current = self.read_many(paths.iter().map(String::as_str))?;
        let updated = apply_patchset(&current, patches, opts)?;
        let mut before = BTreeMap::new();
        let mut after = BTreeMap::new();
        for p in &paths {
            before.insert(p.clone(), current.get(p).cloned());
            after.insert(p.clone(), updated.get(p).cloned());
        }
        let receipt = Receipt { before, after };
        let mut done: Vec<&String> = Vec::new();
        for p in &paths {
            let result = match updated.get(p) {
                Some(content) => self.write(p, content),
                None => self.remove(p),
            };
            if let Err(e) = result {
                // Put back whatever was already changed, then report the original failure.
                for d in done.iter().rev() {
                    let _ = self.restore_one(d, receipt.before.get(*d).and_then(Option::as_deref));
                }
                return Err(e);
            }
            done.push(p);
        }
        Ok(receipt)
    }

    fn restore_one(&self, rel: &str, original: Option<&str>) -> Result<(), WorkspaceError> {
        match original {
            Some(text) => self.write(rel, text),
            None => self.remove(rel),
        }
    }

    /// Undoes exactly what [`Workspace::apply`] did, and nothing else.
    ///
    /// Each changed file is first checked to still hold what the patch wrote; if anything was
    /// edited since, nothing is touched and [`WorkspaceError::ChangedSince`] names the file.
    pub fn rollback(&self, receipt: &Receipt) -> Result<(), WorkspaceError> {
        for (path, written) in &receipt.after {
            if self.read(path)?.as_deref() != written.as_deref() {
                return Err(WorkspaceError::ChangedSince(path.clone()));
            }
        }
        for (path, original) in &receipt.before {
            self.restore_one(path, original.as_deref())?;
        }
        Ok(())
    }
}
