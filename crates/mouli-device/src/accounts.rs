//! Accounts: who may sign in, with which role, and how their passphrase is checked.
//!
//! A passphrase is never stored. The file keeps, for each account, a random salt and the
//! PBKDF2-HMAC-SHA-256 stretch of the passphrase with it, so a stolen file only allows slow
//! guessing. The role belongs to the account: signing in never takes a role from what the person
//! types. The file is replaced atomically and readable by its owner only.
//!
//! What this does not show: a short or common passphrase is still guessable, however stretched.
//! There is no recovery: if every owner forgets their passphrase, the file on the data disk has to
//! be removed by someone with physical access.

use mouli_crypto::hmac::pbkdf2_sha256;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Shortest passphrase accepted, in characters.
pub(crate) const MIN_PASSPHRASE: usize = 8;
/// Longest passphrase accepted, in characters.
const MAX_PASSPHRASE: usize = 200;
const FILE: &str = "users.txt";
/// Stretching rounds. Debug builds (used for tests) use few so tests stay fast; a file records the
/// rounds it was made with, so changing this never locks anyone out.
#[cfg(debug_assertions)]
const ROUNDS: u32 = 1_000;
#[cfg(not(debug_assertions))]
const ROUNDS: u32 = 200_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Account {
    name: String,
    role: String,
    salt: [u8; 16],
    rounds: u32,
    hash: [u8; 32],
}

/// The result of checking a passphrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Right; carries the account's role.
    Ok(String),
    /// Wrong passphrase or no such account (deliberately not told apart).
    Wrong,
}

/// The accounts on this device.
#[derive(Debug)]
pub(crate) struct Accounts {
    path: PathBuf,
    accounts: Vec<Account>,
}

/// Whether `name` can be an account name: 1 to 32 letters, digits, `.`, `-` or `_`.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn valid_role(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= 32
        && !role.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex_array<const N: usize>(s: &str) -> Option<[u8; N]> {
    mouli_crypto::from_hex(s)?.try_into().ok()
}

fn random_salt() -> Result<[u8; 16], String> {
    let mut b = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("no random source: {e}"))?;
    Ok(b)
}

impl Accounts {
    /// Reads the accounts file in `dir` (none yet is fine: no accounts).
    ///
    /// # Errors
    /// A message naming the line when the file is damaged. The file is not changed.
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        let path = dir.join(FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut accounts: Vec<Account> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let f: Vec<&str> = line.split('\t').collect();
            let account = (|| {
                if f.len() != 5 || !valid_name(f[0]) || !valid_role(f[1]) {
                    return None;
                }
                Some(Account {
                    name: f[0].to_string(),
                    role: f[1].to_string(),
                    salt: from_hex_array(f[2])?,
                    rounds: f[3].parse().ok().filter(|r| *r >= 1)?,
                    hash: from_hex_array(f[4])?,
                })
            })()
            .ok_or_else(|| format!("{FILE} line {} is damaged", i + 1))?;
            if accounts.iter().any(|a| a.name == account.name) {
                return Err(format!(
                    "{FILE} line {}: {} appears twice",
                    i + 1,
                    account.name
                ));
            }
            accounts.push(account);
        }
        Ok(Self { path, accounts })
    }

    /// True when nobody has an account yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Every account as (name, role), by name.
    pub(crate) fn list(&self) -> Vec<(String, String)> {
        let mut v: Vec<_> = self
            .accounts
            .iter()
            .map(|a| (a.name.clone(), a.role.clone()))
            .collect();
        v.sort();
        v
    }

    /// The role of `name`, if there is such an account.
    pub(crate) fn role_of(&self, name: &str) -> Option<&str> {
        self.accounts
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.role.as_str())
    }

    fn save(&self) -> Result<(), String> {
        let text: String = self
            .accounts
            .iter()
            .map(|a| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    a.name,
                    a.role,
                    to_hex(&a.salt),
                    a.rounds,
                    to_hex(&a.hash)
                )
            })
            .collect();
        let tmp = self.path.with_extension("tmp");
        let write = || -> std::io::Result<()> {
            let mut opts = OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
            let mut f = opts.open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            if let Some(dir) = self.path.parent() {
                if let Ok(d) = File::open(dir) {
                    let _ = d.sync_all();
                }
            }
            Ok(())
        };
        write().map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// Creates the account or replaces its passphrase and role.
    ///
    /// # Errors
    /// A message for a bad name, role or passphrase, a failed write, or a change that would leave
    /// no account with the role `keep_one_of` (so the last administrator cannot be demoted).
    pub(crate) fn set(
        &mut self,
        name: &str,
        role: &str,
        passphrase: &str,
        keep_one_of: &str,
    ) -> Result<(), String> {
        if !valid_name(name) {
            return Err("names use 1 to 32 letters, digits, . - and _".into());
        }
        if !valid_role(role) {
            return Err("that is not a role name".into());
        }
        let n = passphrase.chars().count();
        if n < MIN_PASSPHRASE {
            return Err(format!(
                "the passphrase must be at least {MIN_PASSPHRASE} characters"
            ));
        }
        if n > MAX_PASSPHRASE {
            return Err(format!(
                "the passphrase can be at most {MAX_PASSPHRASE} characters"
            ));
        }
        let salt = random_salt()?;
        let hash =
            pbkdf2_sha256(passphrase.as_bytes(), &salt, ROUNDS).ok_or("stretching failed")?;
        let account = Account {
            name: name.to_string(),
            role: role.to_string(),
            salt,
            rounds: ROUNDS,
            hash,
        };
        let before = self.accounts.clone();
        match self.accounts.iter_mut().find(|a| a.name == name) {
            Some(a) => *a = account,
            None => self.accounts.push(account),
        }
        self.commit(before, keep_one_of)
    }

    /// Changes an account's role without touching its passphrase.
    ///
    /// # Errors
    /// A message when there is no such account, the role is bad, or `keep_one_of` would be left
    /// without an account.
    pub(crate) fn set_role(
        &mut self,
        name: &str,
        role: &str,
        keep_one_of: &str,
    ) -> Result<(), String> {
        if !valid_role(role) {
            return Err("that is not a role name".into());
        }
        let before = self.accounts.clone();
        let a = self
            .accounts
            .iter_mut()
            .find(|a| a.name == name)
            .ok_or_else(|| format!("there is no account called {name}"))?;
        a.role = role.to_string();
        self.commit(before, keep_one_of)
    }

    /// Removes an account.
    ///
    /// # Errors
    /// A message when there is no such account or `keep_one_of` would be left without one.
    pub(crate) fn remove(&mut self, name: &str, keep_one_of: &str) -> Result<(), String> {
        let before = self.accounts.clone();
        let n = self.accounts.len();
        self.accounts.retain(|a| a.name != name);
        if self.accounts.len() == n {
            return Err(format!("there is no account called {name}"));
        }
        self.commit(before, keep_one_of)
    }

    /// Saves, or puts `before` back when the change breaks the last-administrator rule or the
    /// write fails.
    fn commit(&mut self, before: Vec<Account>, keep_one_of: &str) -> Result<(), String> {
        let had = before.iter().any(|a| a.role == keep_one_of);
        let has = self.accounts.iter().any(|a| a.role == keep_one_of);
        if had && !has {
            self.accounts = before;
            return Err(format!(
                "there must always be an account with the {keep_one_of} role"
            ));
        }
        if let Err(e) = self.save() {
            self.accounts = before;
            return Err(e);
        }
        Ok(())
    }

    /// Checks a passphrase. The work done is the same for a name that has no account, so timing
    /// does not tell which names exist.
    pub(crate) fn verify(&self, name: &str, passphrase: &str) -> Verdict {
        match self.accounts.iter().find(|a| a.name == name) {
            Some(a) => match pbkdf2_sha256(passphrase.as_bytes(), &a.salt, a.rounds) {
                Some(h) if mouli_crypto::ct_eq(&h, &a.hash) => Verdict::Ok(a.role.clone()),
                _ => Verdict::Wrong,
            },
            None => {
                let _ = pbkdf2_sha256(passphrase.as_bytes(), &[0u8; 16], ROUNDS);
                Verdict::Wrong
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_new_directory_has_no_accounts() {
        let d = dir();
        let a = Accounts::open(d.path()).unwrap();
        assert!(a.is_empty() && a.list().is_empty());
        assert!(!d.path().join(FILE).exists());
    }

    #[test]
    fn the_right_passphrase_gives_the_role_and_a_wrong_one_does_not() {
        let d = dir();
        let mut a = Accounts::open(d.path()).unwrap();
        a.set("boss", "owner", "correct horse battery", "owner")
            .unwrap();
        assert_eq!(
            a.verify("boss", "correct horse battery"),
            Verdict::Ok("owner".into())
        );
        assert_eq!(a.verify("boss", "correct horse batterY"), Verdict::Wrong);
        assert_eq!(a.verify("boss", ""), Verdict::Wrong);
        assert_eq!(a.verify("nobody", "correct horse battery"), Verdict::Wrong);
        assert_eq!(a.role_of("boss"), Some("owner"));
    }

    #[test]
    fn accounts_survive_a_restart_and_the_file_never_holds_the_passphrase() {
        let d = dir();
        {
            let mut a = Accounts::open(d.path()).unwrap();
            a.set("boss", "owner", "correct horse battery", "owner")
                .unwrap();
            a.set("amy", "clerk", "another long phrase", "owner")
                .unwrap();
        }
        let text = fs::read_to_string(d.path().join(FILE)).unwrap();
        assert!(!text.contains("correct horse") && !text.contains("another long"));
        let a = Accounts::open(d.path()).unwrap();
        assert_eq!(
            a.list(),
            vec![
                ("amy".into(), "clerk".into()),
                ("boss".into(), "owner".into())
            ]
        );
        assert_eq!(
            a.verify("amy", "another long phrase"),
            Verdict::Ok("clerk".into())
        );
    }

    #[test]
    fn two_accounts_with_the_same_passphrase_store_different_hashes() {
        let d = dir();
        let mut a = Accounts::open(d.path()).unwrap();
        a.set("one", "owner", "same passphrase here", "owner")
            .unwrap();
        a.set("two", "clerk", "same passphrase here", "owner")
            .unwrap();
        assert_ne!(a.accounts[0].hash, a.accounts[1].hash);
        assert_ne!(a.accounts[0].salt, a.accounts[1].salt);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir();
        let mut a = Accounts::open(d.path()).unwrap();
        a.set("boss", "owner", "correct horse battery", "owner")
            .unwrap();
        let mode = fs::metadata(d.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn bad_names_roles_and_passphrases_are_refused() {
        let d = dir();
        let mut a = Accounts::open(d.path()).unwrap();
        for name in ["", "has space", "tab\tname", &"x".repeat(33), "naïve"] {
            assert!(
                a.set(name, "owner", "correct horse battery", "owner")
                    .is_err(),
                "{name:?}"
            );
        }
        assert!(a.set("ok", "", "correct horse battery", "owner").is_err());
        assert!(a
            .set("ok", "two words", "correct horse battery", "owner")
            .is_err());
        assert!(a.set("ok", "owner", "short", "owner").is_err());
        assert!(a.set("ok", "owner", &"x".repeat(201), "owner").is_err());
        assert!(a.is_empty());
    }

    #[test]
    fn the_last_owner_cannot_be_demoted_or_removed_and_nothing_is_saved_when_refused() {
        let d = dir();
        let mut a = Accounts::open(d.path()).unwrap();
        a.set("boss", "owner", "correct horse battery", "owner")
            .unwrap();
        a.set("amy", "clerk", "another long phrase", "owner")
            .unwrap();
        assert!(a.set_role("boss", "clerk", "owner").is_err());
        assert!(a.remove("boss", "owner").is_err());
        assert!(a
            .set("boss", "viewer", "correct horse battery", "owner")
            .is_err());
        assert_eq!(a.role_of("boss"), Some("owner"));
        assert_eq!(
            Accounts::open(d.path()).unwrap().role_of("boss"),
            Some("owner")
        );
        // With a second owner the first can go.
        a.set_role("amy", "owner", "owner").unwrap();
        a.remove("boss", "owner").unwrap();
        assert_eq!(a.list(), vec![("amy".into(), "owner".into())]);
        assert!(a.remove("ghost", "owner").is_err());
        assert!(a.set_role("ghost", "clerk", "owner").is_err());
    }

    #[test]
    fn a_new_passphrase_replaces_the_old_one() {
        let d = dir();
        let mut a = Accounts::open(d.path()).unwrap();
        a.set("boss", "owner", "first passphrase here", "owner")
            .unwrap();
        a.set("boss", "owner", "second passphrase here", "owner")
            .unwrap();
        assert_eq!(a.verify("boss", "first passphrase here"), Verdict::Wrong);
        assert_eq!(
            a.verify("boss", "second passphrase here"),
            Verdict::Ok("owner".into())
        );
        assert_eq!(a.list().len(), 1);
    }

    #[test]
    fn a_damaged_file_is_refused_and_left_alone() {
        let d = dir();
        {
            let mut a = Accounts::open(d.path()).unwrap();
            a.set("boss", "owner", "correct horse battery", "owner")
                .unwrap();
        }
        let path = d.path().join(FILE);
        let good = fs::read_to_string(&path).unwrap();
        for bad in [
            "boss\towner\tzz\t1000\t00".to_string(),
            "boss\towner".to_string(),
            good.replace("\towner\t", "\t\t"),
            good.replace("\t1000\t", "\t0\t"),
            format!("{good}{good}"),
            "bad name\towner\t00\t1\t00".to_string(),
        ] {
            fs::write(&path, &bad).unwrap();
            let e = Accounts::open(d.path()).unwrap_err();
            assert!(e.contains("line"), "{e}");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                bad,
                "the file must not be rewritten"
            );
        }
    }

    #[test]
    fn a_file_made_with_other_rounds_still_verifies() {
        let d = dir();
        let salt = [7u8; 16];
        let hash = pbkdf2_sha256(b"long enough phrase", &salt, 3).unwrap();
        fs::write(
            d.path().join(FILE),
            format!("boss\towner\t{}\t3\t{}\n", to_hex(&salt), to_hex(&hash)),
        )
        .unwrap();
        let a = Accounts::open(d.path()).unwrap();
        assert_eq!(
            a.verify("boss", "long enough phrase"),
            Verdict::Ok("owner".into())
        );
        assert_eq!(a.verify("boss", "long enough phrasE"), Verdict::Wrong);
    }
}
