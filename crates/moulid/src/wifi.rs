//! Wi-Fi: find wireless adapters, scan, and connect to a WPA2 (or open) network with
//! `wpa_supplicant`, then get an address with DHCP.
//!
//! The kernel drivers and firmware come with the image; `wpa_supplicant` does the radio
//! handshake and is driven with `wpa_cli`. Passwords are written only to a mode-0600 file in the
//! RAM-backed `/run`. Supported: open and WPA/WPA2 personal (passphrase). Not supported: WPA3-only,
//! enterprise (802.1X) and hidden networks.

use crate::net;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

/// Where the supplicant keeps its control sockets.
pub const CTRL_DIR: &str = "/run/wpa_supplicant";

/// How a network is secured, as far as this client can tell from the scan flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// No encryption.
    Open,
    /// WPA or WPA2 with a passphrase (also WPA2/WPA3 transition mode).
    Personal,
    /// Something this client cannot join (WPA3-only, 802.1X, WEP).
    Unsupported,
}

/// One network seen in a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    /// Network name.
    pub ssid: String,
    /// Signal level in dBm (closer to 0 is stronger).
    pub signal: i32,
    /// Security.
    pub security: Security,
}

/// Run external programs; replaced in tests.
pub trait Runner {
    /// Run `cmd` with `args`; the standard output when it succeeded.
    fn run(&mut self, cmd: &str, args: &[&str]) -> Option<String>;
    /// Wait.
    fn pause(&mut self, d: Duration);
    /// Where configuration files go (RAM-backed on the real system).
    fn conf_dir(&self) -> String {
        "/run".into()
    }
}

/// The real system.
pub struct System;

impl Runner for System {
    fn run(&mut self, cmd: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn pause(&mut self, d: Duration) {
        sleep(d);
    }
}

/// Wireless adapters (names), sorted.
#[must_use]
pub fn wireless_interfaces(sys_net: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(sys_net)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().join("wireless").exists() || e.path().join("phy80211").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Turn off software radio blocks (a laptop may boot with Wi-Fi "soft blocked"). Returns how
/// many switches were changed. Hardware (key) blocks cannot be changed from software.
pub fn unblock_radios(rfkill_dir: &Path) -> usize {
    let mut n = 0;
    for e in fs::read_dir(rfkill_dir).into_iter().flatten().flatten() {
        let soft = e.path().join("soft");
        if fs::read_to_string(&soft).is_ok_and(|s| s.trim() == "1") && fs::write(&soft, "0").is_ok()
        {
            n += 1;
        }
    }
    n
}

/// One line for the boot screen.
#[must_use]
pub fn summary(ifaces: &[String]) -> String {
    if ifaces.is_empty() {
        "MOULI wifi: no wireless adapter found".into()
    } else {
        format!(
            "MOULI wifi: {} (press w in the model list to connect)",
            ifaces.join(", ")
        )
    }
}

/// Decode `\xNN`, `\\`, `\"`, `\n` escapes as printed by `wpa_cli` in SSIDs.
fn unescape_ssid(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            match b[i + 1] {
                b'x' if i + 3 < b.len() => {
                    if let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16) {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                b'\\' | b'"' => {
                    out.push(b[i + 1]);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn security_of(flags: &str) -> Security {
    let has = |s: &str| flags.contains(s);
    if has("EAP") || has("WEP") || has("OWE") || (has("SAE") && !has("PSK")) {
        Security::Unsupported
    } else if has("PSK") {
        Security::Personal
    } else if has("WPA") || has("RSN") {
        Security::Unsupported
    } else {
        Security::Open
    }
}

/// Parse `wpa_cli scan_results`: a header line, then `bssid<TAB>freq<TAB>signal<TAB>flags<TAB>ssid`.
/// Hidden networks (empty names) and names that are not valid UTF-8 or contain control
/// characters are skipped; the strongest entry per name is kept; sorted strongest first.
#[must_use]
pub fn parse_scan(text: &str) -> Vec<Network> {
    let mut best: Vec<Network> = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 || f[0].len() != 17 {
            continue;
        }
        let Ok(signal) = f[2].trim().parse::<i32>() else {
            continue;
        };
        let Ok(ssid) = String::from_utf8(unescape_ssid(f[4])) else {
            continue;
        };
        if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
            continue;
        }
        let n = Network {
            ssid,
            signal,
            security: security_of(f[3]),
        };
        match best.iter_mut().find(|b| b.ssid == n.ssid) {
            Some(b) if b.signal < n.signal => *b = n,
            Some(_) => {}
            None => best.push(n),
        }
    }
    best.sort_by(|a, b| b.signal.cmp(&a.signal).then(a.ssid.cmp(&b.ssid)));
    best
}

/// A valid WPA passphrase is 8 to 63 printable ASCII characters.
#[must_use]
pub fn valid_passphrase(p: &str) -> bool {
    (8..=63).contains(&p.len()) && p.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// A `wpa_supplicant.conf` with a control socket and, optionally, one network. The name goes in as
/// hex (no quoting problems); the passphrase is quoted (the supplicant takes everything between
/// the first and the last quote). `None` passphrase means an open network.
///
/// # Errors
/// When the passphrase is not valid.
pub fn config_text(ssid: Option<&str>, passphrase: Option<&str>) -> Result<String, String> {
    let mut c = format!("ctrl_interface={CTRL_DIR}\nupdate_config=0\nap_scan=1\n");
    if let Some(ssid) = ssid {
        c.push_str("network={\n");
        c.push_str(&format!("    ssid={}\n", hex(ssid)));
        match passphrase {
            Some(p) => {
                if !valid_passphrase(p) {
                    return Err(
                        "a Wi-Fi password has 8 to 63 characters (letters, digits, symbols)".into(),
                    );
                }
                c.push_str(&format!("    psk=\"{p}\"\n    key_mgmt=WPA-PSK\n"));
            }
            None => c.push_str("    key_mgmt=NONE\n"),
        }
        c.push_str("}\n");
    }
    Ok(c)
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(tmp, path)
}

/// Path of the supplicant configuration for `iface`.
fn conf_path<R: Runner>(r: &R, iface: &str) -> String {
    format!("{}/wpa_supplicant-{iface}.conf", r.conf_dir())
}

/// Start the supplicant on `iface` with no networks (enough to scan). Idempotent.
///
/// # Errors
/// A message when the supplicant cannot start.
pub fn start<R: Runner>(r: &mut R, iface: &str) -> Result<(), String> {
    if r.run("wpa_cli", &["-p", CTRL_DIR, "-i", iface, "status"])
        .is_some()
    {
        return Ok(());
    }
    let conf = conf_path(r, iface);
    let _ = fs::create_dir_all(CTRL_DIR);
    write_private(Path::new(&conf), &config_text(None, None)?)
        .map_err(|e| format!("cannot write {conf}: {e}"))?;
    let _ = r.run("ip", &["link", "set", iface, "up"]);
    r.run(
        "wpa_supplicant",
        &[
            "-B",
            "-i",
            iface,
            "-c",
            &conf,
            "-D",
            "nl80211",
            "-f",
            &format!("{}/wpa_supplicant-{iface}.log", r.conf_dir()),
            "-P",
            &format!("{}/wpa_supplicant-{iface}.pid", r.conf_dir()),
        ],
    )
    .ok_or("wpa_supplicant did not start (is the adapter's firmware present?)")?;
    for _ in 0..20 {
        if r.run("wpa_cli", &["-p", CTRL_DIR, "-i", iface, "status"])
            .is_some()
        {
            return Ok(());
        }
        r.pause(Duration::from_millis(250));
    }
    let log = fs::read_to_string(format!("{}/wpa_supplicant-{iface}.log", r.conf_dir()))
        .unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(3).collect();
    Err(format!(
        "wpa_supplicant started but does not answer ({})",
        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
    ))
}

/// Scan and list networks, retrying while the adapter wakes up.
///
/// # Errors
/// A message when the supplicant cannot be started.
pub fn scan<R: Runner>(r: &mut R, iface: &str, attempts: u32) -> Result<Vec<Network>, String> {
    start(r, iface)?;
    for _ in 0..attempts {
        let _ = r.run("wpa_cli", &["-p", CTRL_DIR, "-i", iface, "scan"]);
        r.pause(Duration::from_secs(3));
        if let Some(t) = r.run("wpa_cli", &["-p", CTRL_DIR, "-i", iface, "scan_results"]) {
            let nets = parse_scan(&t);
            if !nets.is_empty() {
                return Ok(nets);
            }
        }
    }
    Ok(Vec::new())
}

/// Join `ssid` and wait until the supplicant reports the link as up.
///
/// # Errors
/// A message describing the failure, with the likely cause.
pub fn join<R: Runner>(
    r: &mut R,
    iface: &str,
    ssid: &str,
    passphrase: Option<&str>,
    wait_secs: u32,
) -> Result<(), String> {
    start(r, iface)?;
    let conf = conf_path(r, iface);
    write_private(Path::new(&conf), &config_text(Some(ssid), passphrase)?)
        .map_err(|e| format!("cannot write {conf}: {e}"))?;
    r.run("wpa_cli", &["-p", CTRL_DIR, "-i", iface, "reconfigure"])
        .ok_or("the supplicant did not accept the settings")?;
    for _ in 0..wait_secs.saturating_mul(2) {
        r.pause(Duration::from_millis(500));
        if r.run("wpa_cli", &["-p", CTRL_DIR, "-i", iface, "status"])
            .is_some_and(|s| s.lines().any(|l| l == "wpa_state=COMPLETED"))
        {
            return Ok(());
        }
    }
    Err(if passphrase.is_some() {
        "could not connect: the password may be wrong, or the signal too weak".into()
    } else {
        "could not connect to the open network".into()
    })
}

/// Join and get an address.
///
/// # Errors
/// A message for the person.
pub fn connect<R: Runner>(
    r: &mut R,
    iface: &str,
    ssid: &str,
    passphrase: Option<&str>,
) -> Result<net::Link, String> {
    join(r, iface, ssid, passphrase, 25)?;
    net::lease(iface)
        .ok_or_else(|| "connected to the access point, but it gave no address (DHCP)".into())
}

fn bars(signal: i32) -> &'static str {
    match signal {
        s if s >= -55 => "****",
        s if s >= -67 => "*** ",
        s if s >= -78 => "**  ",
        _ => "*   ",
    }
}

/// The connect screen: pick an adapter and a network, enter the password, connect.
/// `Ok(None)` when the person backs out or nothing could be done.
///
/// # Errors
/// I/O errors on the console.
pub fn connect_flow<I: std::io::BufRead, O: std::io::Write, R: Runner>(
    input: &mut I,
    out: &mut O,
    r: &mut R,
    sys_net: &Path,
    save_to: Option<&Path>,
) -> std::io::Result<Option<net::Link>> {
    use crate::hosted::{ask, ask_secret};
    let ifaces = wireless_interfaces(sys_net);
    let Some(iface) = ifaces.first() else {
        writeln!(
            out,
            "\n  No Wi-Fi adapter was found. Use a network cable, or an adapter the kernel knows."
        )?;
        return Ok(None);
    };
    writeln!(
        out,
        "\n  Wi-Fi ({iface}): scanning, this takes a few seconds..."
    )?;
    out.flush()?;
    let nets = match scan(r, iface, 3) {
        Ok(n) => n,
        Err(e) => {
            writeln!(out, "  {e}")?;
            return Ok(None);
        }
    };
    if nets.is_empty() {
        writeln!(out, "  No networks found. Move closer to the router, or check the Wi-Fi switch on the laptop.")?;
        return Ok(None);
    }
    for (i, n) in nets.iter().enumerate() {
        let sec = match n.security {
            Security::Open => "open",
            Security::Personal => "password",
            Security::Unsupported => "not supported",
        };
        writeln!(
            out,
            "  {:>2}) {} {}  [{sec}]",
            i + 1,
            bars(n.signal),
            n.ssid
        )?;
    }
    let Some(a) = ask(
        input,
        out,
        "\n  Number of the network (Enter to go back) > ",
    )?
    else {
        return Ok(None);
    };
    let Some(net) = a
        .parse::<usize>()
        .ok()
        .and_then(|k| nets.get(k.wrapping_sub(1)))
    else {
        return Ok(None);
    };
    let pass = match net.security {
        Security::Unsupported => {
            writeln!(
                out,
                "  This network uses WPA3-only or enterprise login, which is not supported yet."
            )?;
            return Ok(None);
        }
        Security::Open => None,
        Security::Personal => {
            let Some(p) = ask_secret(input, out, "  Wi-Fi password (typing is hidden) > ")? else {
                return Ok(None);
            };
            Some(p)
        }
    };
    writeln!(out, "  Connecting to {}...", net.ssid)?;
    out.flush()?;
    match connect(r, iface, &net.ssid, pass.as_deref()) {
        Ok(link) => {
            writeln!(out, "  {}", link.summary())?;
            if let Some(root) = save_to {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                match crate::wifi_saved::remember(root, &net.ssid, pass.as_deref(), now) {
                    Ok(()) => writeln!(out, "  Saved: MOULI will join this network next time.")?,
                    Err(e) => writeln!(out, "  (could not save this network: {e})")?,
                }
            }
            Ok(Some(link))
        }
        Err(e) => {
            writeln!(out, "  {e}")?;
            Ok(None)
        }
    }
}

/// Test the whole Wi-Fi path on two virtual radios (`mac80211_hwsim.radios=2`): one runs an
/// access point, the other scans, refuses a wrong password and joins with the right one.
///
/// # Errors
/// A message naming the step that failed.
pub fn selftest<R: Runner>(r: &mut R, sys_net: &Path) -> Result<(), String> {
    const SSID: &str = "mouli-test";
    const PASS: &str = "mouli-test-pass";
    let ifaces = wireless_interfaces(sys_net);
    let [client, ap, ..] = ifaces.as_slice() else {
        return Err(format!(
            "need two wireless interfaces, found {}",
            ifaces.len()
        ));
    };
    let ap_conf = format!("{}/wpa_supplicant-ap.conf", r.conf_dir());
    let ap_ctrl = "/run/wpa_supplicant-ap";
    let text = format!(
        "ctrl_interface={ap_ctrl}\nnetwork={{\n    ssid=\"{SSID}\"\n    mode=2\n    frequency=2412\n    key_mgmt=WPA-PSK\n    proto=RSN\n    pairwise=CCMP\n    group=CCMP\n    psk=\"{PASS}\"\n}}\n"
    );
    write_private(Path::new(&ap_conf), &text)
        .map_err(|e| format!("cannot write {ap_conf}: {e}"))?;
    let _ = fs::create_dir_all(ap_ctrl);
    let _ = r.run("ip", &["link", "set", ap, "up"]);
    r.run(
        "wpa_supplicant",
        &["-B", "-i", ap, "-c", &ap_conf, "-D", "nl80211"],
    )
    .ok_or("the access point side of wpa_supplicant did not start")?;
    let mut up = false;
    for _ in 0..40 {
        if r.run("wpa_cli", &["-p", ap_ctrl, "-i", ap, "status"])
            .is_some_and(|s| s.lines().any(|l| l == "wpa_state=COMPLETED"))
        {
            up = true;
            break;
        }
        r.pause(Duration::from_millis(250));
    }
    if !up {
        return Err("the virtual access point did not come up".into());
    }
    let nets = scan(r, client, 4)?;
    let found = nets.iter().find(|n| n.ssid == SSID).ok_or_else(|| {
        format!(
            "the test network was not found among {} networks",
            nets.len()
        )
    })?;
    if found.security != Security::Personal {
        return Err("the test network was not seen as password-protected".into());
    }
    if join(r, client, SSID, Some("wrong-password-1"), 8).is_ok() {
        return Err("a wrong password was accepted".into());
    }
    join(r, client, SSID, Some(PASS), 25)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "bssid / frequency / signal level / flags / ssid\n\
        aa:bb:cc:dd:ee:01\t2412\t-40\t[WPA2-PSK-CCMP][ESS]\tHome\n\
        aa:bb:cc:dd:ee:02\t5180\t-30\t[WPA2-PSK-CCMP][ESS]\tHome\n\
        aa:bb:cc:dd:ee:03\t2437\t-60\t[ESS]\tCafe Guest\n\
        aa:bb:cc:dd:ee:04\t2462\t-55\t[WPA2-EAP-CCMP][ESS]\tOffice\n\
        aa:bb:cc:dd:ee:05\t2462\t-70\t[RSN-SAE-CCMP][ESS]\tModern\n\
        aa:bb:cc:dd:ee:06\t2462\t-50\t[WPA-PSK-TKIP][WPA2-PSK-CCMP][ESS]\tOld\n\
        aa:bb:cc:dd:ee:07\t2462\t-45\t[WPA2-PSK-CCMP][ESS]\t\n\
        aa:bb:cc:dd:ee:08\t2462\t-65\t[WPA2-PSK-CCMP][ESS]\tCaf\\xc3\\xa9\n\
        garbage line\n";

    #[test]
    fn scan_results_are_parsed_deduplicated_and_sorted() {
        let n = parse_scan(SCAN);
        let names: Vec<&str> = n.iter().map(|x| x.ssid.as_str()).collect();
        assert_eq!(
            names,
            ["Home", "Old", "Office", "Cafe Guest", "Caf\u{e9}", "Modern"]
        );
        assert_eq!(n[0].signal, -30);
        let sec = |name: &str| n.iter().find(|x| x.ssid == name).unwrap().security;
        assert_eq!(sec("Home"), Security::Personal);
        assert_eq!(sec("Old"), Security::Personal);
        assert_eq!(sec("Cafe Guest"), Security::Open);
        assert_eq!(sec("Office"), Security::Unsupported);
        assert_eq!(sec("Modern"), Security::Unsupported);
    }

    #[test]
    fn config_has_hex_name_and_quoted_passphrase() {
        let c = config_text(Some("My Wi-Fi"), Some("pass\"word 123")).unwrap();
        assert!(c.contains("ssid=4d792057692d4669"));
        assert!(c.contains("psk=\"pass\"word 123\""));
        assert!(c.contains("key_mgmt=WPA-PSK"));
        assert!(c.starts_with("ctrl_interface=/run/wpa_supplicant\n"));
        let open = config_text(Some("Cafe"), None).unwrap();
        assert!(open.contains("key_mgmt=NONE") && !open.contains("psk"));
        assert!(!config_text(None, None).unwrap().contains("network"));
        assert!(config_text(Some("x"), Some("short")).is_err());
        assert!(config_text(Some("x"), Some("line\nbreak-12345")).is_err());
        assert!(!valid_passphrase(&"a".repeat(64)));
    }

    /// Plays back canned answers and records what was run.
    struct Fake {
        dir: std::path::PathBuf,
        log: Vec<String>,
        running: bool,
        status_after_reconfigure: &'static str,
        reconfigured: bool,
    }

    impl Runner for Fake {
        fn run(&mut self, cmd: &str, args: &[&str]) -> Option<String> {
            self.log.push(format!("{cmd} {}", args.join(" ")));
            match (cmd, args.last().copied()) {
                ("wpa_supplicant", _) => {
                    self.running = true;
                    Some(String::new())
                }
                ("wpa_cli", Some("status")) if !self.running => None,
                ("wpa_cli", Some("status")) => Some(if self.reconfigured {
                    self.status_after_reconfigure.to_string()
                } else {
                    "wpa_state=DISCONNECTED\n".into()
                }),
                ("wpa_cli", Some("reconfigure")) => {
                    self.reconfigured = true;
                    Some("OK\n".into())
                }
                ("wpa_cli", Some("scan_results")) => Some(SCAN.to_string()),
                _ => Some(String::new()),
            }
        }
        fn pause(&mut self, _: Duration) {}
        fn conf_dir(&self) -> String {
            self.dir.to_string_lossy().into_owned()
        }
    }

    fn fake(tag: &str, status: &'static str) -> Fake {
        let dir = std::env::temp_dir().join(format!("moulid-wifi-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Fake {
            dir,
            log: Vec::new(),
            running: false,
            status_after_reconfigure: status,
            reconfigured: false,
        }
    }

    #[test]
    fn scan_starts_the_supplicant_once() {
        let mut f = fake("scan", "");
        let nets = scan(&mut f, "wlan0", 1).unwrap();
        assert_eq!(nets.len(), 6);
        assert_eq!(
            f.log
                .iter()
                .filter(|l| l.starts_with("wpa_supplicant"))
                .count(),
            1
        );
        scan(&mut f, "wlan0", 1).unwrap();
        assert_eq!(
            f.log
                .iter()
                .filter(|l| l.starts_with("wpa_supplicant"))
                .count(),
            1
        );
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn join_writes_a_private_config_and_waits_for_completion() {
        let mut f = fake("join", "wpa_state=COMPLETED\nssid=Home\n");
        join(&mut f, "wlan0", "Home", Some("correct horse"), 5).unwrap();
        let conf = f.dir.join("wpa_supplicant-wlan0.conf");
        assert!(fs::read_to_string(&conf)
            .unwrap()
            .contains("psk=\"correct horse\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(f
            .log
            .iter()
            .any(|l| l == "wpa_cli -p /run/wpa_supplicant -i wlan0 reconfigure"));
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_wrong_password_times_out_with_a_helpful_message() {
        let mut f = fake("wrong", "wpa_state=4WAY_HANDSHAKE\n");
        let e = join(&mut f, "wlan0", "Home", Some("wrong-pass-1"), 2).unwrap_err();
        assert!(e.contains("password may be wrong"), "{e}");
        let mut f2 = fake("badpw", "");
        assert!(join(&mut f2, "wlan0", "Home", Some("short"), 1).is_err());
        let _ = fs::remove_dir_all(&f.dir);
        let _ = fs::remove_dir_all(&f2.dir);
    }

    #[test]
    fn adapters_and_radio_switches() {
        let d = std::env::temp_dir().join(format!("moulid-wifi-sys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for (name, wireless) in [("wlan0", true), ("eth0", false), ("wlp2s0", true)] {
            fs::create_dir_all(d.join(name)).unwrap();
            if wireless {
                fs::create_dir_all(d.join(name).join("wireless")).unwrap();
            }
        }
        assert_eq!(wireless_interfaces(&d), ["wlan0", "wlp2s0"]);
        assert_eq!(summary(&[]), "MOULI wifi: no wireless adapter found");
        assert!(summary(&["wlan0".into()]).contains("wlan0"));
        let rf = d.join("rfkill");
        for (n, v) in [("rfkill0", "1"), ("rfkill1", "0")] {
            fs::create_dir_all(rf.join(n)).unwrap();
            fs::write(rf.join(n).join("soft"), v).unwrap();
        }
        assert_eq!(unblock_radios(&rf), 1);
        assert_eq!(fs::read_to_string(rf.join("rfkill0/soft")).unwrap(), "0");
        let _ = fs::remove_dir_all(&d);
    }
}
