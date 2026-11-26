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

// ------------------------------------------------------------------ sealed keys

/// Passphrase stretching rounds. A stolen stick lets an attacker try passphrases offline, so each
/// guess is made to cost real work.
#[cfg(not(debug_assertions))]
const ROUNDS: u32 = 200_000;
/// Debug builds (used only for testing) use few rounds so tests stay fast; the algorithm itself
/// is checked against published vectors, and a file always records the rounds it was made with.
#[cfg(debug_assertions)]
const ROUNDS: u32 = 1_000;
const MAGIC: &[u8; 4] = b"MKS1";
/// Shortest passphrase accepted for a sealed key.
pub const MIN_PASSPHRASE: usize = 8;

fn sealed_path(dir: &Path, provider: &str) -> Result<PathBuf, Error> {
    let plain = path_for(dir, provider)?;
    Ok(plain.with_extension("sealed"))
}

/// PBKDF2 with HMAC-SHA-256 (RFC 8018), one 32-byte block.
fn pbkdf2(passphrase: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    use mouli_crypto::hmac::hmac_sha256;
    let mut first = salt.to_vec();
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(passphrase, &first);
    let mut out = u;
    for _ in 1..rounds {
        u = hmac_sha256(passphrase, &u);
        for (o, x) in out.iter_mut().zip(&u) {
            *o ^= x;
        }
    }
    out
}

fn random_bytes<const N: usize>() -> Result<[u8; N], Error> {
    use std::io::Read;
    let mut b = [0u8; N];
    fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

fn seal_to_bytes(
    provider: &str,
    key: &Secret,
    passphrase: &str,
    rounds: u32,
    salt: [u8; 16],
    nonce: [u8; 12],
) -> Vec<u8> {
    let k = pbkdf2(passphrase.as_bytes(), &salt, rounds);
    let mut aad = MAGIC.to_vec();
    aad.extend_from_slice(provider.as_bytes());
    let sealed = mouli_crypto::aead::seal(&k, &nonce, &aad, key.expose().as_bytes());
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&rounds.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    out
}

fn open_bytes(provider: &str, bytes: &[u8], passphrase: &str) -> Result<Secret, Error> {
    const BAD: Error = Error::Config("the passphrase is wrong, or the key file is damaged");
    if bytes.len() < 4 + 4 + 16 + 12 + 16 || &bytes[..4] != MAGIC {
        return Err(BAD);
    }
    let rounds = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if !(1..=5_000_000).contains(&rounds) {
        return Err(BAD);
    }
    let (salt, rest) = bytes[8..].split_at(16);
    let (nonce, sealed) = rest.split_at(12);
    let (Ok(nonce), Ok(salt)) = (<[u8; 12]>::try_from(nonce), <[u8; 16]>::try_from(salt)) else {
        return Err(BAD);
    };
    let k = pbkdf2(passphrase.as_bytes(), &salt, rounds);
    let mut aad = MAGIC.to_vec();
    aad.extend_from_slice(provider.as_bytes());
    let plain = mouli_crypto::aead::open(&k, &nonce, &aad, sealed).ok_or(BAD)?;
    let text = String::from_utf8(plain).map_err(|_| BAD)?;
    Secret::new(&text)
}

/// Whether an encrypted key is saved for `provider`.
#[must_use]
pub fn sealed_exists(dir: &Path, provider: &str) -> bool {
    sealed_path(dir, provider).is_ok_and(|p| p.exists())
}

/// Saves the key encrypted with `passphrase` (at least [`MIN_PASSPHRASE`] characters): the file
/// holds only salt, nonce and ciphertext, so without the passphrase it reveals nothing about the
/// key. Replaces any plain key file for the provider.
///
/// # Errors
/// [`Error`] for a short passphrase, a bad provider name, or a filesystem failure.
pub fn save_sealed(
    dir: &Path,
    provider: &str,
    key: &Secret,
    passphrase: &str,
) -> Result<(), Error> {
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(Error::Config("the passphrase needs at least 8 characters"));
    }
    let target = sealed_path(dir, provider)?;
    fs::create_dir_all(dir)?;
    let bytes = seal_to_bytes(
        provider,
        key,
        passphrase,
        ROUNDS,
        random_bytes::<16>()?,
        random_bytes::<12>()?,
    );
    let tmp = dir.join(format!(".{provider}.stmp"));
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, &target)?;
    // The plain copy must not outlive the sealed one.
    remove(dir, provider)
}

/// Opens the encrypted key for `provider`. `Ok(None)` when none is saved.
///
/// # Errors
/// [`Error`] for a wrong passphrase (indistinguishable from a damaged file), a bad provider
/// name, or an unreadable file.
pub fn load_sealed(dir: &Path, provider: &str, passphrase: &str) -> Result<Option<Secret>, Error> {
    let path = sealed_path(dir, provider)?;
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    open_bytes(provider, &bytes, passphrase).map(Some)
}

/// Deletes the encrypted key for `provider`. Succeeds when there was none.
///
/// # Errors
/// [`Error`] for a bad provider name or a filesystem failure.
pub fn remove_sealed(dir: &Path, provider: &str) -> Result<(), Error> {
    match fs::remove_file(sealed_path(dir, provider)?) {
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

    #[test]
    fn pbkdf2_matches_the_rfc_6070_style_vector_for_sha256() {
        // PBKDF2-HMAC-SHA256, password "password", salt "salt", 1 and 2 iterations
        // (published test vectors).
        let one = pbkdf2(b"password", b"salt", 1);
        assert_eq!(
            mouli_crypto::from_hex(
                "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
            )
            .unwrap(),
            one
        );
        let two = pbkdf2(b"password", b"salt", 2);
        assert_eq!(
            mouli_crypto::from_hex(
                "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
            )
            .unwrap(),
            two
        );
    }

    #[test]
    fn a_sealed_key_needs_its_passphrase_and_reveals_nothing_without_it() {
        let dir = tmp("sealed");
        let key = Secret::new("sk-very-secret-123").unwrap();
        assert!(
            !sealed_exists(&dir, "acme")
                && load_sealed(&dir, "acme", "whatever1").unwrap().is_none()
        );
        assert!(save_sealed(&dir, "acme", &key, "short").is_err());
        save_sealed(&dir, "acme", &key, "correct horse battery").unwrap();
        assert!(sealed_exists(&dir, "acme"));
        let file = fs::read(dir.join("acme.sealed")).unwrap();
        assert!(
            !String::from_utf8_lossy(&file).contains("sk-very-secret"),
            "the key is not in the file"
        );
        let got = load_sealed(&dir, "acme", "correct horse battery")
            .unwrap()
            .unwrap();
        assert_eq!(got.expose(), "sk-very-secret-123");
        assert!(load_sealed(&dir, "acme", "wrong passphrase").is_err());
        // Two saves of the same key and passphrase differ (fresh salt and nonce).
        save_sealed(&dir, "acme", &key, "correct horse battery").unwrap();
        assert_ne!(fs::read(dir.join("acme.sealed")).unwrap(), file);
        // A file cannot be moved to another provider's name, and damage is detected.
        fs::copy(dir.join("acme.sealed"), dir.join("other.sealed")).unwrap();
        assert!(load_sealed(&dir, "other", "correct horse battery").is_err());
        let mut damaged = fs::read(dir.join("acme.sealed")).unwrap();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        fs::write(dir.join("acme.sealed"), &damaged).unwrap();
        assert!(load_sealed(&dir, "acme", "correct horse battery").is_err());
        for cut in [0, 3, 8, 30, 40] {
            assert!(open_bytes("acme", &damaged[..cut.min(damaged.len())], "x").is_err());
        }
        // Saving sealed removes an older plain copy; removal works and is repeatable.
        save(&dir, "acme", &key).unwrap();
        save_sealed(&dir, "acme", &key, "correct horse battery").unwrap();
        assert!(load(&dir, "acme").unwrap().is_none());
        remove_sealed(&dir, "acme").unwrap();
        remove_sealed(&dir, "acme").unwrap();
        assert!(!sealed_exists(&dir, "acme"));
        let _ = fs::remove_dir_all(&dir);
    }
}
