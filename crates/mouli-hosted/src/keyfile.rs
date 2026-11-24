//! API key files: one file per provider in a directory on the data partition, owner-only.

use crate::chat::Secret;
use crate::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A loaded key and whether its file was readable by others.
pub struct Loaded {
    /// The key.
    pub key: Secret,
    /// The file's permissions allow group or others to read it (always false where the
    /// filesystem has no permission bits, such as FAT, and on non-Unix systems).
    pub loose_permissions: bool,
}

fn path_for(dir: &Path, provider: &str) -> Result<PathBuf, Error> {
    let ok = !provider.is_empty()
        && provider.len() <= 40
        && provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return Err(Error::Config(
            "provider names use letters, digits, - and _ only",
        ));
    }
    Ok(dir.join(format!("{provider}.key")))
}

/// Save a key for `provider` in `dir` (created if needed), owner-only, replacing atomically.
///
/// # Errors
/// [`Error`] for a bad provider name or a filesystem failure.
pub fn save(dir: &Path, provider: &str, key: &Secret) -> Result<(), Error> {
    let target = path_for(dir, provider)?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{provider}.tmp"));
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(key.expose().as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(&tmp, &target)?;
    Ok(())
}

/// Load the key for `provider` from `dir`. `Ok(None)` when none is saved.
///
/// # Errors
/// [`Error`] for a bad provider name, an unreadable file, or a file that holds no usable key.
pub fn load(dir: &Path, provider: &str) -> Result<Option<Loaded>, Error> {
    let path = path_for(dir, provider)?;
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    #[cfg(unix)]
    let loose = {
        use std::os::unix::fs::PermissionsExt;
        // FAT and friends report one fixed mode for every file; only trust ext-style modes.
        fs::metadata(&path)?.permissions().mode() & 0o044 != 0
    };
    #[cfg(not(unix))]
    let loose = false;
    Ok(Some(Loaded {
        key: Secret::new(&text)?,
        loose_permissions: loose,
    }))
}

/// Delete the saved key for `provider`. Succeeds when there was none.
///
/// # Errors
/// [`Error`] for a bad provider name or a filesystem failure.
pub fn remove(dir: &Path, provider: &str) -> Result<(), Error> {
    match fs::remove_file(path_for(dir, provider)?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mouli-keys-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn save_load_remove() {
        let dir = tmp("a");
        assert!(load(&dir, "acme").unwrap().is_none());
        save(&dir, "acme", &Secret::new("sk-1").unwrap()).unwrap();
        let l = load(&dir, "acme").unwrap().unwrap();
        assert_eq!(l.key.expose(), "sk-1");
        assert!(!l.loose_permissions);
        save(&dir, "acme", &Secret::new("sk-2").unwrap()).unwrap();
        assert_eq!(load(&dir, "acme").unwrap().unwrap().key.expose(), "sk-2");
        remove(&dir, "acme").unwrap();
        remove(&dir, "acme").unwrap();
        assert!(load(&dir, "acme").unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only_and_loose_ones_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp("b");
        save(&dir, "p", &Secret::new("k").unwrap()).unwrap();
        let path = dir.join("p.key");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&dir, "p").unwrap().unwrap().loose_permissions);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_names_cannot_escape_the_directory() {
        let dir = tmp("c");
        for bad in ["", "../x", "a/b", "a.b", "a b", &"x".repeat(41)] {
            assert!(
                save(&dir, bad, &Secret::new("k").unwrap()).is_err(),
                "{bad}"
            );
            assert!(load(&dir, bad).is_err());
        }
    }

    #[test]
    fn empty_key_file_is_an_error() {
        let dir = tmp("d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("p.key"), "\n").unwrap();
        assert!(load(&dir, "p").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
