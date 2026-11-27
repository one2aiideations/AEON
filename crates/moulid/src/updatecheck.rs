//! `/update`: asks an update server whether a newer MOULI exists, and checks the answer.
//!
//! This is the first half of updating and only that half: it fetches the signed manifest and its
//! signature over HTTPS, verifies them against the keys the owner trusts (`mouli-update`), decides
//! with the same rules the updater uses, and tells the person what it found. It downloads no
//! image, writes nothing to the disk and changes nothing. Installing an update (a second boot
//! slot, a switch in the boot loader, rollback) is not part of this image, so a person who wants
//! the new version still writes the new ISO to the USB stick.
//!
//! Nothing here runs by itself and nothing is built in: the server address and the trusted keys
//! are files the owner puts on the data disk.
//!
//! - `updates/update.conf`: `url https://host/path` (the manifest is `<url>/manifest` and its
//!   signature `<url>/manifest.sig`), and optionally `channel stable|beta|nightly`.
//! - `updates/trust.keys`: the `mouli-update` trust file (`threshold N`, `key <name> <64 hex>`,
//!   `revoke <id>`).
//!
//! The running version is the first line of `/etc/mouli-version` when the image has that file,
//! otherwise the version of this program, which is not the release number: the answer then says
//! which one it used.

use crate::fetch::Fetcher;
use mouli_update::{
    decide, verify_manifest, BlockedReason, Channel, Decision, DetachedSig, Environment, Policy,
    SemVer, State, TrustStore,
};
use std::io::Read;
use std::path::Path;

/// Where the image may say which release it is.
pub const VERSION_FILE: &str = "/etc/mouli-version";

/// 2024-01-01: a clock before this has not been set.
const CLOCK_SET_AFTER: i64 = 1_704_067_200;

const MANIFEST_LIMIT: u64 = 64 * 1024 + 1;

/// What to check and against whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Base address, always `https://`.
    pub url: String,
    /// Channel asked for.
    pub channel: Channel,
}

impl Config {
    /// Parses `update.conf`.
    ///
    /// # Errors
    /// A message for a missing, unknown or non-HTTPS line.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut url = None;
        let mut channel = Channel::Stable;
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            match line.split_once(char::is_whitespace) {
                Some(("url", v)) => {
                    let v = v.trim().trim_end_matches('/');
                    if !v.starts_with("https://") || v.len() <= "https://".len() {
                        return Err("the update address must start with https://".into());
                    }
                    url = Some(v.to_string());
                }
                Some(("channel", v)) => {
                    channel = Channel::parse(v.trim())
                        .map_err(|_| "channel must be stable, beta or nightly".to_string())?;
                }
                _ => return Err(format!("update.conf: cannot read the line \"{line}\"")),
            }
        }
        Ok(Self {
            url: url.ok_or("update.conf has no \"url\" line")?,
            channel,
        })
    }
}

fn get_all<F: Fetcher>(f: &F, url: &str) -> Result<Vec<u8>, String> {
    let reply = f.get(url, &[])?;
    if reply.status != 200 {
        return Err(format!("the server answered {} for {url}", reply.status));
    }
    let mut body = Vec::new();
    reply
        .body
        .take(MANIFEST_LIMIT)
        .read_to_end(&mut body)
        .map_err(|e| format!("reading {url}: {e}"))?;
    Ok(body)
}

/// Fetches and checks the manifest, and says in a sentence or two what it means for a machine
/// running `current`. Never installs anything.
///
/// # Errors
/// A message when the server cannot be reached or its answer is refused (bad signature, expired,
/// malformed): such an answer must not be taken as "up to date".
pub fn check<F: Fetcher>(
    f: &F,
    cfg: &Config,
    trust: &TrustStore,
    current: &SemVer,
    now: i64,
    device_id: &str,
    free_bytes: u64,
) -> Result<String, String> {
    let manifest_bytes = get_all(f, &format!("{}/manifest", cfg.url))?;
    let sig_bytes = get_all(f, &format!("{}/manifest.sig", cfg.url))?;
    let sig_text = String::from_utf8(sig_bytes).map_err(|_| "the signature file is not text")?;
    let sig = DetachedSig::parse(&sig_text)
        .map_err(|e| format!("the signature file is damaged ({e})"))?;
    let manifest = verify_manifest(&manifest_bytes, &[sig], trust, now)
        .map_err(|e| format!("the update answer was refused ({e}); nothing was trusted"))?;
    let policy = Policy {
        channels: vec![cfg.channel],
        reserve_bytes: 0,
        skip_versions: Vec::new(),
        allow_metered: true,
        expected_product: "MOULI".into(),
    };
    let env = Environment {
        metered: false,
        free_bytes,
    };
    let state = State::initial(current.clone(), 0);
    Ok(match decide(&state, &manifest, &policy, &env, device_id) {
        Decision::UpToDate => format!(
            "MOULI {} is up to date (the server offers {}).",
            current.render(),
            manifest.version.render()
        ),
        Decision::Update(p) => format!(
            "MOULI {} is available (you run {}).{} It was not downloaded: this image cannot install updates itself. Write the new ISO to the USB stick.",
            p.manifest.version.render(),
            current.render(),
            if p.manifest.notes.is_empty() {
                String::new()
            } else {
                format!(" Notes: {}", p.manifest.notes)
            }
        ),
        Decision::Blocked(why) => format!(
            "MOULI {} exists but is not offered to this machine: {}.",
            manifest.version.render(),
            match why {
                BlockedReason::ProductMismatch => "it is for another product".to_string(),
                BlockedReason::ChannelMismatch => "it is on another channel".to_string(),
                BlockedReason::Downgrade => "it is older than a version already refused".to_string(),
                BlockedReason::NeedsIntermediate(v) => format!("install {v} first"),
                BlockedReason::StagedRolloutIneligible => "it is still rolling out to others".to_string(),
                BlockedReason::Metered => "the connection is metered".to_string(),
                BlockedReason::NoSpace => "there is not enough free space".to_string(),
                BlockedReason::Skipped => "it is on the skip list".to_string(),
            }
        ),
    })
}

/// The whole command for the desktop: reads the two files under `root/updates`, finds the
/// running version, and runs [`check`].
#[must_use]
pub fn command<F: Fetcher>(f: &F, root: Option<&Path>, now: i64, device_id: &str) -> String {
    let Some(root) = root else {
        return "No data disk, so there is nowhere to read update.conf and trust.keys from.".into();
    };
    if now < CLOCK_SET_AFTER {
        // An expired manifest would look fresh to a clock stuck near 1970.
        return "Update check not run: the clock is not set yet (connect to a network first, or set the time).".into();
    }
    let dir = root.join("updates");
    let cfg = match std::fs::read_to_string(dir.join("update.conf")) {
        Ok(t) => match Config::parse(&t) {
            Ok(c) => c,
            Err(e) => return format!("Update check not run: {e}."),
        },
        Err(_) => {
            return "Update check not set up: put update.conf (a line \"url https://...\") and trust.keys in the updates folder of the data disk.".into();
        }
    };
    let trust = match std::fs::read_to_string(dir.join("trust.keys")) {
        Ok(t) => match TrustStore::parse_file(&t) {
            Ok(s) if !s.keys.is_empty() => s,
            _ => return "Update check not run: trust.keys has no usable key.".into(),
        },
        Err(_) => {
            return "Update check not run: trust.keys is missing from the updates folder.".into()
        }
    };
    let (current, from) = match std::fs::read_to_string(VERSION_FILE)
        .ok()
        .and_then(|t| SemVer::parse(t.lines().next().unwrap_or("").trim()).ok())
    {
        Some(v) => (v, "the image's release number"),
        None => (
            SemVer::parse(crate::VERSION).unwrap_or(SemVer {
                major: 0,
                minor: 0,
                patch: 0,
                pre_release: None,
            }),
            "the program's own version, because the image does not state its release",
        ),
    };
    match check(
        f,
        &cfg,
        &trust,
        &current,
        now,
        device_id,
        crate::fetch::free_bytes(root),
    ) {
        Ok(s) => format!("{s} (Compared using {from}.)"),
        Err(e) => format!("Update check failed: {e}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::Reply;
    use mouli_update::{Artifact, Manifest, ReleaseTool};
    use std::collections::HashMap;

    struct Fake(HashMap<String, (u16, Vec<u8>)>);
    impl Fetcher for Fake {
        fn get(&self, url: &str, _: &[(String, String)]) -> Result<Reply, String> {
            match self.0.get(url) {
                Some((status, body)) => Ok(Reply {
                    status: *status,
                    headers: vec![],
                    body: Box::new(std::io::Cursor::new(body.clone())),
                }),
                None => Err("connection failed".into()),
            }
        }
    }

    const SEED: [u8; 32] = [7; 32];
    const NOW: i64 = 1_800_000_000;

    fn manifest(version: &str, channel: Channel, product: &str) -> Manifest {
        Manifest {
            product: product.into(),
            channel,
            version: SemVer::parse(version).unwrap(),
            rollback_index: 1,
            min_current_version: SemVer::parse("0.0.0").unwrap(),
            released_at: NOW - 100,
            expires_at: NOW + 1000,
            rollout_percent: 100,
            notes: "Faster start.".into(),
            artifacts: vec![Artifact {
                name: "mouli.iso".into(),
                size: 10,
                sha256: [0; 32],
                url: "https://x.example/mouli.iso".into(),
            }],
            signed_by: String::new(),
        }
    }

    fn server(m: &mut Manifest, seed: [u8; 32]) -> Fake {
        let (bytes, sig) = ReleaseTool::sign(m, seed);
        let mut map = HashMap::new();
        map.insert("https://u.example/m/manifest".into(), (200, bytes));
        map.insert(
            "https://u.example/m/manifest.sig".into(),
            (200, sig.render().into_bytes()),
        );
        Fake(map)
    }

    fn trust() -> TrustStore {
        let key = ed25519_dalek::SigningKey::from_bytes(&SEED).verifying_key();
        let mut t = TrustStore::new(1);
        t.add_key("release", key);
        t
    }

    fn cfg(channel: Channel) -> Config {
        Config {
            url: "https://u.example/m".into(),
            channel,
        }
    }

    fn run(f: &Fake, current: &str) -> Result<String, String> {
        check(
            f,
            &cfg(Channel::Stable),
            &trust(),
            &SemVer::parse(current).unwrap(),
            NOW,
            "dev",
            1 << 30,
        )
    }

    #[test]
    fn a_newer_signed_release_is_reported_and_not_installed() {
        let f = server(&mut manifest("1.7.0", Channel::Stable, "MOULI"), SEED);
        let s = run(&f, "1.6.0").unwrap();
        assert!(s.contains("1.7.0 is available"), "{s}");
        assert!(s.contains("Faster start."), "{s}");
        assert!(s.contains("not downloaded"), "{s}");
    }

    #[test]
    fn the_same_or_an_older_release_is_up_to_date() {
        let f = server(&mut manifest("1.6.0", Channel::Stable, "MOULI"), SEED);
        assert!(run(&f, "1.6.0").unwrap().contains("up to date"));
        assert!(run(&f, "1.9.0").unwrap().contains("up to date"));
    }

    #[test]
    fn an_answer_signed_by_an_unknown_key_is_refused_not_trusted() {
        let f = server(&mut manifest("9.0.0", Channel::Stable, "MOULI"), [9; 32]);
        let e = run(&f, "1.6.0").unwrap_err();
        assert!(
            e.contains("refused") && e.contains("nothing was trusted"),
            "{e}"
        );
    }

    #[test]
    fn a_tampered_manifest_or_a_missing_signature_is_refused() {
        let mut f = server(&mut manifest("1.7.0", Channel::Stable, "MOULI"), SEED);
        let url = "https://u.example/m/manifest".to_string();
        let (st, mut body) = f.0.get(&url).unwrap().clone();
        let at = body.iter().position(|b| *b == b'7').unwrap();
        body[at] = b'8';
        f.0.insert(url, (st, body));
        assert!(run(&f, "1.6.0").unwrap_err().contains("refused"));
        f.0.remove("https://u.example/m/manifest.sig");
        assert!(run(&f, "1.6.0").is_err());
    }

    #[test]
    fn an_expired_manifest_is_refused() {
        let mut m = manifest("1.7.0", Channel::Stable, "MOULI");
        m.expires_at = NOW - 1;
        let f = server(&mut m, SEED);
        assert!(run(&f, "1.6.0").unwrap_err().contains("refused"));
    }

    #[test]
    fn other_channels_and_products_are_not_offered() {
        let f = server(&mut manifest("1.7.0", Channel::Beta, "MOULI"), SEED);
        assert!(run(&f, "1.6.0").unwrap().contains("another channel"));
        let f = server(&mut manifest("1.7.0", Channel::Stable, "Other"), SEED);
        assert!(run(&f, "1.6.0").unwrap().contains("another product"));
    }

    #[test]
    fn a_server_error_is_an_error_and_never_up_to_date() {
        let mut f = server(&mut manifest("1.7.0", Channel::Stable, "MOULI"), SEED);
        f.0.insert("https://u.example/m/manifest".into(), (404, vec![]));
        assert!(run(&f, "1.6.0").unwrap_err().contains("404"));
        assert!(run(&Fake(HashMap::new()), "1.6.0").is_err());
    }

    #[test]
    fn the_config_needs_https_and_a_url() {
        let c = Config::parse("# x\nurl https://u.example/m/\nchannel beta\n").unwrap();
        assert_eq!(
            (c.url.as_str(), c.channel),
            ("https://u.example/m", Channel::Beta)
        );
        assert!(Config::parse("url http://u.example").is_err());
        assert!(Config::parse("channel beta").is_err());
        assert!(Config::parse("url https://\n").is_err());
        assert!(Config::parse("url https://a\nwat\n").is_err());
        assert!(Config::parse("url https://a\nchannel gold\n").is_err());
    }

    #[test]
    fn the_command_explains_what_is_missing() {
        let f = Fake(HashMap::new());
        assert!(command(&f, None, NOW, "d").contains("No data disk"));
        let dir0 = std::env::temp_dir();
        assert!(command(&f, Some(&dir0), 5, "d").contains("clock is not set"));
        let dir = std::env::temp_dir().join(format!("moulid-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("updates")).unwrap();
        assert!(command(&f, Some(&dir), NOW, "d").contains("not set up"));
        std::fs::write(dir.join("updates/update.conf"), "url https://u.example/m\n").unwrap();
        assert!(command(&f, Some(&dir), NOW, "d").contains("trust.keys is missing"));
        std::fs::write(dir.join("updates/trust.keys"), "threshold 1\n").unwrap();
        assert!(command(&f, Some(&dir), NOW, "d").contains("no usable key"));
        let key = ed25519_dalek::SigningKey::from_bytes(&SEED).verifying_key();
        let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(
            dir.join("updates/trust.keys"),
            format!("threshold 1\nkey release {hex}\n"),
        )
        .unwrap();
        let s = command(&f, Some(&dir), NOW, "d");
        assert!(s.starts_with("Update check failed"), "{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
