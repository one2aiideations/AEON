//! Saved Wi-Fi networks on the data partition.
//!
//! The list lives in `<data root>/wifi-networks` (format: see [`crate::known_networks`]) and is
//! readable by root only. After a successful connection the network is remembered, and at boot
//! [`auto_connect`] joins the strongest remembered network that is in range.

use crate::known_networks::{Book, BookError, Entry, Secret};
use crate::net;
use crate::wifi::{self, Network, Runner, Security};
use std::io::{self, Write};
use std::path::Path;

const FILE: &str = "wifi-networks";

/// Load the saved networks. A missing or unreadable file gives an empty book; damaged lines are
/// skipped.
#[must_use]
pub fn load(root: &Path) -> Book {
    let text = std::fs::read_to_string(root.join(FILE)).unwrap_or_default();
    Book::parse(&text).0
}

/// Save the book atomically (written beside the file, synced, renamed) with mode 0600.
///
/// # Errors
/// I/O errors, for example a read-only filesystem.
pub fn save(root: &Path, book: &Book) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = root.join(format!("{FILE}.tmp"));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(book.render().as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, root.join(FILE))
}

fn bad(e: &BookError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
}

/// Remember (or update) one network. `now` orders networks by recency; any increasing number
/// will do, a Unix time is typical.
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`] when the name or password cannot be stored, I/O errors
/// otherwise.
pub fn remember(root: &Path, ssid: &str, passphrase: Option<&str>, now: u64) -> io::Result<()> {
    let mut book = load(root);
    let passphrase = match passphrase {
        Some(p) => Some(Secret::new(p.to_owned()).map_err(|e| bad(&e))?),
        None => None,
    };
    book.upsert(Entry {
        ssid: ssid.as_bytes().to_vec(),
        passphrase,
        last_used: now,
    })
    .map_err(|e| bad(&e))?;
    save(root, &book)
}

/// Forget one network. Returns whether it was saved.
///
/// # Errors
/// I/O errors while rewriting the file.
pub fn forget(root: &Path, ssid: &str) -> io::Result<bool> {
    let mut book = load(root);
    let found = book.forget(ssid.as_bytes());
    if found {
        save(root, &book)?;
    }
    Ok(found)
}

/// The saved network to join, given a scan: its name and password.
#[must_use]
pub fn pick(book: &Book, scan: &[Network]) -> Option<(String, Option<String>)> {
    let visible: Vec<(Vec<u8>, i32)> = scan
        .iter()
        .filter(|n| n.security != Security::Unsupported)
        .map(|n| (n.ssid.as_bytes().to_vec(), n.signal))
        .collect();
    let entry = book.best_match(&visible)?;
    let ssid = String::from_utf8(entry.ssid.clone()).ok()?;
    Some((
        ssid,
        entry.passphrase.as_ref().map(|p| p.expose().to_owned()),
    ))
}

/// Join the best saved network in range. `None` when there is nothing to try (no saved
/// networks, no adapter, none in range); otherwise the outcome, with the network name.
pub fn auto_connect<R: Runner>(
    r: &mut R,
    sys_net: &Path,
    root: &Path,
) -> Option<(String, Result<net::Link, String>)> {
    let book = load(root);
    if book.is_empty() {
        return None;
    }
    let iface = wifi::wireless_interfaces(sys_net).into_iter().next()?;
    let nets = wifi::scan(r, &iface, 3).ok()?;
    let (ssid, pass) = pick(&book, &nets)?;
    let result = wifi::connect(r, &iface, &ssid, pass.as_deref());
    Some((ssid, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("mouli-wifi-saved-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn net(ssid: &str, signal: i32, security: Security) -> Network {
        Network {
            ssid: ssid.into(),
            signal,
            security,
        }
    }

    #[test]
    fn missing_file_is_an_empty_book() {
        let d = tmp("missing");
        assert!(load(&d).is_empty());
    }

    #[test]
    fn remember_round_trips_and_is_private() {
        let d = tmp("round");
        remember(&d, "Home", Some("correct horse"), 10).unwrap();
        remember(&d, "Cafe", None, 20).unwrap();
        let book = load(&d);
        assert_eq!(book.len(), 2);
        let mode = std::fs::metadata(d.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "no access for group or others");
        let text = std::fs::read_to_string(d.join(FILE)).unwrap();
        assert!(
            !text.contains("correct horse"),
            "the password is not stored as readable text"
        );
        assert!(!d.join(format!("{FILE}.tmp")).exists());
    }

    #[test]
    fn remembering_again_updates_instead_of_duplicating() {
        let d = tmp("update");
        remember(&d, "Home", Some("first-password"), 1).unwrap();
        remember(&d, "Home", Some("second-password"), 2).unwrap();
        let book = load(&d);
        assert_eq!(book.len(), 1);
        let scan = [net("Home", -50, Security::Personal)];
        assert_eq!(
            pick(&book, &scan),
            Some(("Home".into(), Some("second-password".into())))
        );
    }

    #[test]
    fn bad_passwords_are_refused_not_saved() {
        let d = tmp("bad");
        let e = remember(&d, "Home", Some("short"), 1).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(load(&d).is_empty());
        assert!(remember(&d, "", None, 1).is_err());
    }

    #[test]
    fn forget_removes_only_that_network() {
        let d = tmp("forget");
        remember(&d, "A", None, 1).unwrap();
        remember(&d, "B", None, 2).unwrap();
        assert!(forget(&d, "A").unwrap());
        assert!(!forget(&d, "A").unwrap());
        assert_eq!(load(&d).len(), 1);
    }

    #[test]
    fn pick_prefers_the_stronger_saved_network_and_ignores_unknown_ones() {
        let d = tmp("pick");
        remember(&d, "Home", Some("home-password"), 1).unwrap();
        remember(&d, "Work", Some("work-password"), 2).unwrap();
        let book = load(&d);
        let scan = [
            net("Stranger", -30, Security::Open),
            net("Home", -70, Security::Personal),
            net("Work", -50, Security::Personal),
        ];
        assert_eq!(
            pick(&book, &scan),
            Some(("Work".into(), Some("work-password".into())))
        );
        assert_eq!(pick(&book, &[net("Stranger", -30, Security::Open)]), None);
        assert_eq!(pick(&book, &[]), None);
    }

    #[test]
    fn pick_skips_networks_this_client_cannot_join() {
        let d = tmp("unsupported");
        remember(&d, "Work", Some("work-password"), 1).unwrap();
        let book = load(&d);
        assert_eq!(
            pick(&book, &[net("Work", -40, Security::Unsupported)]),
            None
        );
    }

    #[test]
    fn damaged_lines_do_not_lose_the_good_ones() {
        let d = tmp("damaged");
        remember(&d, "Home", Some("home-password"), 1).unwrap();
        let mut text = std::fs::read_to_string(d.join(FILE)).unwrap();
        text.push_str("this is not a record\n");
        std::fs::write(d.join(FILE), text).unwrap();
        assert_eq!(load(&d).len(), 1);
    }
}
