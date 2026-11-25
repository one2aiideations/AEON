//! Bluetooth: pairing mice, keyboards and trackpads.
//!
//! The kernel talks to the adapter and BlueZ (`bluetoothd`, over dbus) does the pairing; this module
//! starts them and drives `bluetoothctl`, the same way [`crate::wifi`] drives `wpa_supplicant`.
//! A paired device shows up as an ordinary input device (`/dev/input/event*`), so the desktop needs
//! nothing else. Pairing keys are kept on the MOULI-DATA partition when there is one (in plain
//! text, like the chat logs), so a paired mouse reconnects after a restart; without it, pairing is
//! lost at power-off.
//!
//! Supported: mice, trackpads and other devices that pair without a code ("just works"), over
//! Bluetooth classic and Bluetooth LE. Not supported: keyboards that show a code to type, headsets
//! and other audio. Adapters that need a firmware file we do not ship (many Broadcom and some
//! Realtek ones) may not start.

use crate::wifi::Runner;
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// `bluetoothctl`, the BlueZ command line client.
pub const BLUETOOTHCTL: &str = "/usr/bin/bluetoothctl";
/// The BlueZ daemon.
pub const BLUETOOTHD: &str = "/usr/libexec/bluetooth/bluetoothd";
/// The message bus BlueZ talks over.
pub const DBUS_DAEMON: &str = "/usr/bin/dbus-daemon";
/// Makes the bus's machine id.
pub const DBUS_UUIDGEN: &str = "/usr/bin/dbus-uuidgen";
/// Where the kernel lists Bluetooth adapters.
pub const SYS_BLUETOOTH: &str = "/sys/class/bluetooth";
/// Where BlueZ keeps pairing keys.
pub const STATE_DIR: &str = "/var/lib/bluetooth";

/// How many seconds a scan listens.
const SCAN_SECONDS: &str = "10";
/// The longest device name shown.
const MAX_NAME: usize = 40;
/// The most devices listed.
const MAX_DEVICES: usize = 30;

/// A Bluetooth device seen by the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Address, in `AA:BB:CC:DD:EE:FF` form.
    pub mac: String,
    /// Name it announced (cleaned); the address when it has none yet.
    pub name: String,
}

/// What the module needs from the system, on top of running programs.
pub trait Tools: Runner {
    /// Starts `cmd` in the background (it keeps running); whether it could be started.
    fn spawn(&mut self, cmd: &str, args: &[&str]) -> bool;
}

impl Tools for crate::wifi::System {
    fn spawn(&mut self, cmd: &str, args: &[&str]) -> bool {
        Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }
}

/// Whether `s` is a Bluetooth address in the form `AA:BB:CC:DD:EE:FF`.
#[must_use]
pub fn valid_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A device name made safe to print: printable characters only (names come from any device in
/// range, so they are not trusted), cut to a short length.
#[must_use]
pub fn clean_name(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.chars().take(MAX_NAME).collect()
}

/// Reads devices from `bluetoothctl devices` or scan output: lines like
/// `Device AA:BB:CC:DD:EE:FF Name` or `[NEW] Device AA:BB:CC:DD:EE:FF Name`. Duplicates and
/// invalid addresses are dropped; devices with a name come first.
#[must_use]
pub fn parse_devices(text: &str) -> Vec<Device> {
    let mut out: Vec<Device> = Vec::new();
    for line in text.lines() {
        let line = strip_ansi(line);
        let Some(pos) = line.find("Device ") else {
            continue;
        };
        // `[NEW]` lines and plain `devices` lines carry the name directly. `[CHG]` lines carry one
        // changed property (`RSSI:`, `UUIDs:`...); only a name or alias is of use. Others ([DEL]...)
        // are ignored.
        let prefix = line[..pos].trim();
        let rest = &line[pos + "Device ".len()..];
        let mut it = rest.splitn(2, ' ');
        let mac = it.next().unwrap_or("").trim().to_ascii_uppercase();
        if !valid_mac(&mac) {
            continue;
        }
        let tail = it.next().unwrap_or("").trim();
        let raw_name = match prefix {
            "" | "[NEW]" => tail,
            "[CHG]" => match tail
                .strip_prefix("Name: ")
                .or_else(|| tail.strip_prefix("Alias: "))
            {
                Some(n) => n,
                None => continue,
            },
            _ => continue,
        };
        let name = clean_name(raw_name);
        // A name that is just the address with dashes is how BlueZ shows a device without one.
        let unnamed = name.is_empty() || name == mac.replace(':', "-");
        let name = if unnamed { mac.clone() } else { name };
        if let Some(old) = out.iter_mut().find(|d| d.mac == mac) {
            if old.name == old.mac && name != mac {
                old.name = name;
            }
            continue;
        }
        if out.len() < MAX_DEVICES {
            out.push(Device { mac, name });
        }
    }
    out.sort_by_key(|d| d.name == d.mac);
    out
}

/// Removes terminal colour codes that `bluetoothctl` adds around its output.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '\r' {
            continue;
        }
        out.push(c);
    }
    out
}

/// The Bluetooth adapters the kernel found (`hci0`, ...), sorted.
#[must_use]
pub fn adapters(sys: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(sys)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("hci") && n[3..].chars().all(|c| c.is_ascii_digit()))
        .collect();
    v.sort();
    v
}

/// One line for the boot screen.
#[must_use]
pub fn summary(adapters: &[String]) -> String {
    if adapters.is_empty() {
        "MOULI bluetooth: no adapter found".into()
    } else {
        format!(
            "MOULI bluetooth: {} (press b in the model list to pair a mouse)",
            adapters.join(", ")
        )
    }
}

/// Makes BlueZ keep its pairing keys on the MOULI-DATA partition: `/var/lib/bluetooth` becomes a
/// link to `<data>/bluetooth`. Without a data partition the keys stay in memory.
///
/// # Errors
/// A message when the folder or the link cannot be made.
pub fn persist_keys(data_root: &Path, state_dir: &Path) -> Result<(), String> {
    let target = data_root.join("bluetooth");
    std::fs::create_dir_all(&target)
        .map_err(|e| format!("cannot make {}: {e}", target.display()))?;
    if let Some(parent) = state_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(state_dir) {
        if meta.file_type().is_symlink() {
            return Ok(());
        }
        // An empty folder left by the image can be replaced; keep anything else.
        std::fs::remove_dir(state_dir)
            .map_err(|_| format!("{} is not empty; keys stay in memory", state_dir.display()))?;
    }
    std::os::unix::fs::symlink(&target, state_dir)
        .map_err(|e| format!("cannot link {}: {e}", state_dir.display()))
}

/// Whether the data partition holds Bluetooth keys from an earlier session.
#[must_use]
pub fn has_saved_pairings(data_root: &Path) -> bool {
    std::fs::read_dir(data_root.join("bluetooth"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// Starts the message bus and `bluetoothd` and waits for an adapter to be usable.
///
/// # Errors
/// A message saying what is missing or did not start.
pub fn start_services<T: Tools>(t: &mut T, rfkill_dir: &Path, sys: &Path) -> Result<(), String> {
    if adapters(sys).is_empty() {
        return Err(
            "No Bluetooth adapter was found. Plug in a USB Bluetooth adapter, then press b again."
                .into(),
        );
    }
    crate::wifi::unblock_radios(rfkill_dir);
    if t.run(BLUETOOTHCTL, &["list"])
        .is_some_and(|o| o.contains("Controller"))
    {
        return Ok(()); // already running
    }
    let _ = std::fs::create_dir_all("/run/dbus");
    let _ = t.run(DBUS_UUIDGEN, &["--ensure=/etc/machine-id"]);
    if !t.spawn(DBUS_DAEMON, &["--system", "--nofork", "--nopidfile"]) {
        return Err("The message bus (dbus-daemon) could not be started.".into());
    }
    t.pause(Duration::from_millis(500));
    if !t.spawn(BLUETOOTHD, &["-n"]) {
        return Err("The Bluetooth service (bluetoothd) could not be started.".into());
    }
    for _ in 0..20 {
        t.pause(Duration::from_millis(500));
        if t.run(BLUETOOTHCTL, &["list"])
            .is_some_and(|o| o.contains("Controller"))
        {
            let _ = t.run(BLUETOOTHCTL, &["power", "on"]);
            return Ok(());
        }
    }
    Err("The Bluetooth adapter did not become ready. Some adapters need a firmware file that is not in this image.".into())
}

/// Listens for devices for a few seconds and returns what was found.
///
/// # Errors
/// A message when `bluetoothctl` could not run.
pub fn scan<T: Tools>(t: &mut T) -> Result<Vec<Device>, String> {
    let _ = t.run(BLUETOOTHCTL, &["power", "on"]);
    let heard = t
        .run(BLUETOOTHCTL, &["--timeout", SCAN_SECONDS, "scan", "on"])
        .unwrap_or_default();
    let known = t
        .run(BLUETOOTHCTL, &["devices"])
        .ok_or("bluetoothctl could not list devices")?;
    Ok(parse_devices(&format!("{heard}\n{known}")))
}

/// Pairs, trusts and connects `mac` (a device in pairing mode that needs no code).
///
/// # Errors
/// A message saying which step failed.
pub fn pair<T: Tools>(t: &mut T, mac: &str) -> Result<(), String> {
    if !valid_mac(mac) {
        return Err("not a Bluetooth address".into());
    }
    let paired = t
        .run(BLUETOOTHCTL, &["--agent", "NoInputNoOutput", "pair", mac])
        .is_some_and(|o| o.contains("Pairing successful") || o.contains("AlreadyExists"));
    if !paired {
        return Err("Pairing failed. Put the device in pairing mode (hold its pairing button until the light blinks) and try again. Devices that ask for a code are not supported.".into());
    }
    let _ = t.run(BLUETOOTHCTL, &["trust", mac]);
    let connected = t
        .run(BLUETOOTHCTL, &["connect", mac])
        .is_some_and(|o| o.contains("Connection successful"));
    if connected {
        Ok(())
    } else {
        Err(
            "Paired, but it did not connect. Move the device once; it may connect by itself."
                .into(),
        )
    }
}

/// The pairing screen: find devices, pick one, pair it.
///
/// # Errors
/// I/O errors on the console.
pub fn pair_flow<I: BufRead, O: Write, T: Tools>(
    input: &mut I,
    out: &mut O,
    t: &mut T,
    rfkill_dir: &Path,
    sys: &Path,
) -> std::io::Result<bool> {
    use crate::hosted::ask;
    writeln!(out, "\n  Bluetooth: starting...")?;
    out.flush()?;
    if let Err(e) = start_services(t, rfkill_dir, sys) {
        writeln!(out, "  {e}")?;
        return Ok(false);
    }
    writeln!(
        out,
        "  Put your mouse or trackpad in pairing mode now. Scanning for {SCAN_SECONDS} seconds..."
    )?;
    out.flush()?;
    let devices = match scan(t) {
        Ok(d) => d,
        Err(e) => {
            writeln!(out, "  {e}")?;
            return Ok(false);
        }
    };
    if devices.is_empty() {
        writeln!(out, "  No devices found. Check that the device is in pairing mode and close to the computer.")?;
        return Ok(false);
    }
    for (i, d) in devices.iter().enumerate() {
        if d.name == d.mac {
            writeln!(out, "  {:>2}) {}  (no name)", i + 1, d.mac)?;
        } else {
            writeln!(out, "  {:>2}) {}  {}", i + 1, d.name, d.mac)?;
        }
    }
    let Some(a) = ask(input, out, "\n  Number of the device (Enter to go back) > ")? else {
        return Ok(false);
    };
    let Some(dev) = a
        .parse::<usize>()
        .ok()
        .and_then(|k| devices.get(k.wrapping_sub(1)))
    else {
        return Ok(false);
    };
    writeln!(out, "  Pairing with {}...", dev.name)?;
    out.flush()?;
    match pair(t, &dev.mac) {
        Ok(()) => {
            writeln!(
                out,
                "  Connected. Move the mouse; the pointer appears in the desktop."
            )?;
            Ok(true)
        }
        Err(e) => {
            writeln!(out, "  {e}")?;
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        /// Responses to `run`, by the first matching command line prefix.
        replies: Vec<(String, Option<String>)>,
        log: Vec<String>,
        spawned: Vec<String>,
        spawn_ok: bool,
        list_after: usize,
        lists: usize,
    }

    impl Runner for Fake {
        fn run(&mut self, cmd: &str, args: &[&str]) -> Option<String> {
            let line = format!("{cmd} {}", args.join(" "));
            self.log.push(line.clone());
            if args == ["list"] {
                self.lists += 1;
                return (self.lists > self.list_after)
                    .then(|| "Controller 00:11:22:33:44:55 x".into());
            }
            self.replies
                .iter()
                .find(|(p, _)| line.contains(p.as_str()))
                .and_then(|(_, r)| r.clone())
        }
        fn pause(&mut self, _: Duration) {}
    }

    impl Tools for Fake {
        fn spawn(&mut self, cmd: &str, args: &[&str]) -> bool {
            self.spawned.push(format!("{cmd} {}", args.join(" ")));
            self.spawn_ok
        }
    }

    fn fake() -> Fake {
        Fake {
            spawn_ok: true,
            ..Fake::default()
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-bt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn addresses_are_checked_strictly() {
        assert!(valid_mac("AA:BB:CC:DD:EE:FF"));
        assert!(valid_mac("aa:bb:cc:00:11:22"));
        for bad in [
            "",
            "AA:BB:CC:DD:EE",
            "AA:BB:CC:DD:EE:FF:00",
            "AA-BB-CC-DD-EE-FF",
            "GG:BB:CC:DD:EE:FF",
            "A:BB:CC:DD:EE:FF",
            "AA:BB:CC:DD:EE:F;",
        ] {
            assert!(!valid_mac(bad), "{bad:?}");
        }
    }

    #[test]
    fn names_are_made_safe_and_short() {
        assert_eq!(clean_name("MX Master 3"), "MX Master 3");
        assert_eq!(clean_name("evil\x1b[2Jname\r\n"), "evil [2Jname");
        assert!(!clean_name("a\x07b\x00c").contains('\x07'));
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), MAX_NAME);
        assert_eq!(clean_name("   "), "");
    }

    #[test]
    fn devices_are_read_from_both_kinds_of_line() {
        let text = "\x1b[0;92m[NEW]\x1b[0m Device 11:22:33:44:55:66 Logitech MX\r\n\
Device AA:BB:CC:DD:EE:FF\n\
Device 77:88:99:AA:BB:CC 77-88-99-AA-BB-CC\n\
Device 11:22:33:44:55:66 Logitech MX\n\
Device nonsense Not A Mac\n\
Controller 00:11:22:33:44:55 host\n\
[CHG] Device AA:BB:CC:DD:EE:FF RSSI: -60\n\
[CHG] Device AA:BB:CC:DD:EE:FF UUIDs: 00001124-0000-1000-8000-00805f9b34fb\n\
[DEL] Device 77:88:99:AA:BB:CC 77-88-99-AA-BB-CC\n";
        let d = parse_devices(text);
        assert_eq!(d.len(), 3, "{d:?}");
        assert_eq!(d[0].name, "Logitech MX", "named devices come first");
        assert_eq!(d[0].mac, "11:22:33:44:55:66");
        assert!(d.iter().all(|x| valid_mac(&x.mac)));
        assert!(d
            .iter()
            .any(|x| x.mac == "AA:BB:CC:DD:EE:FF" && x.name == x.mac));
    }

    #[test]
    fn a_name_that_arrives_later_replaces_the_address() {
        let d = parse_devices(
            "Device AA:BB:CC:DD:EE:FF\n[CHG] Device AA:BB:CC:DD:EE:FF Name: Magic Mouse\n",
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "Magic Mouse");
    }

    #[test]
    fn the_device_list_is_bounded() {
        let mut text = String::new();
        for i in 0..200u32 {
            text.push_str(&format!(
                "Device 00:00:00:00:{:02X}:{:02X} Dev{i}\n",
                i / 256,
                i % 256
            ));
        }
        assert_eq!(parse_devices(&text).len(), MAX_DEVICES);
    }

    #[test]
    fn adapters_are_the_hci_nodes() {
        let d = tmp("sys");
        for n in ["hci1", "hci0", "hci0:256", "rfkill3", "hciX"] {
            std::fs::create_dir_all(d.join(n)).unwrap();
        }
        assert_eq!(adapters(&d), vec!["hci0", "hci1"]);
        assert!(adapters(Path::new("/nonexistent-bt")).is_empty());
        assert!(summary(&[]).contains("no adapter"));
        assert!(summary(&["hci0".into()]).contains("press b"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn services_are_not_started_without_an_adapter() {
        let d = tmp("noadapter");
        let mut f = fake();
        let e = start_services(&mut f, &d, &d).unwrap_err();
        assert!(e.contains("No Bluetooth adapter"));
        assert!(f.spawned.is_empty(), "nothing started");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn services_start_in_order_and_wait_for_the_adapter() {
        let d = tmp("start");
        std::fs::create_dir_all(d.join("hci0")).unwrap();
        let mut f = fake();
        f.list_after = 3; // the controller shows up only after a few checks
        start_services(&mut f, &d, &d).unwrap();
        assert_eq!(f.spawned.len(), 2);
        assert!(f.spawned[0].contains("dbus-daemon"));
        assert!(f.spawned[1].contains("bluetoothd"));
        assert!(f.log.iter().any(|l| l.contains("power on")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn services_already_running_are_left_alone() {
        let d = tmp("running");
        std::fs::create_dir_all(d.join("hci0")).unwrap();
        let mut f = fake();
        start_services(&mut f, &d, &d).unwrap();
        assert!(f.spawned.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn services_that_never_come_up_report_why() {
        let d = tmp("never");
        std::fs::create_dir_all(d.join("hci0")).unwrap();
        let mut f = fake();
        f.list_after = usize::MAX;
        let e = start_services(&mut f, &d, &d).unwrap_err();
        assert!(e.contains("firmware"));
        let mut bad = fake();
        bad.spawn_ok = false;
        bad.list_after = usize::MAX;
        assert!(start_services(&mut bad, &d, &d)
            .unwrap_err()
            .contains("dbus"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pairing_runs_pair_trust_connect_and_checks_each_answer() {
        let mut f = fake();
        f.replies = vec![
            ("pair".into(), Some("Pairing successful".into())),
            ("trust".into(), Some("trust succeeded".into())),
            ("connect".into(), Some("Connection successful".into())),
        ];
        pair(&mut f, "AA:BB:CC:DD:EE:FF").unwrap();
        let order: Vec<_> = f
            .log
            .iter()
            .map(|l| l.split_whitespace().rev().nth(1).unwrap().to_string())
            .collect();
        assert_eq!(order, vec!["pair", "trust", "connect"]);
        assert!(f.log[0].contains("--agent NoInputNoOutput"));
    }

    #[test]
    fn pairing_failures_say_what_to_do() {
        let mut f = fake();
        f.replies = vec![(
            "pair".into(),
            Some("Failed to pair: AuthenticationFailed".into()),
        )];
        let e = pair(&mut f, "AA:BB:CC:DD:EE:FF").unwrap_err();
        assert!(e.contains("pairing mode"));
        let mut g = fake();
        g.replies = vec![
            ("pair".into(), Some("Pairing successful".into())),
            ("connect".into(), Some("Failed to connect".into())),
        ];
        assert!(pair(&mut g, "AA:BB:CC:DD:EE:FF")
            .unwrap_err()
            .contains("did not connect"));
        let mut h = fake();
        assert!(pair(&mut h, "not-a-mac; rm -rf /").is_err());
        assert!(h.log.is_empty(), "nothing is run for a bad address");
    }

    #[test]
    fn the_flow_lists_devices_and_pairs_the_chosen_one() {
        let d = tmp("flow");
        std::fs::create_dir_all(d.join("hci0")).unwrap();
        let mut f = fake();
        f.replies = vec![
            (
                "scan on".into(),
                Some("[NEW] Device 11:22:33:44:55:66 Cool Mouse\n".into()),
            ),
            ("devices".into(), Some("Device AA:BB:CC:DD:EE:FF\n".into())),
            ("pair".into(), Some("Pairing successful".into())),
            ("connect".into(), Some("Connection successful".into())),
        ];
        let mut out = Vec::new();
        let mut input = std::io::Cursor::new(b"1\n".to_vec());
        let ok = pair_flow(&mut input, &mut out, &mut f, &d, &d).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(ok, "{text}");
        assert!(text.contains("Cool Mouse") && text.contains("(no name)"));
        assert!(
            f.log.iter().any(|l| l.contains("pair 11:22:33:44:55:66")),
            "named device is number 1"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_flow_backs_out_cleanly() {
        let d = tmp("back");
        std::fs::create_dir_all(d.join("hci0")).unwrap();
        for answer in ["\n", "0\n", "99\n", "x\n"] {
            let mut f = fake();
            f.replies = vec![(
                "devices".into(),
                Some("Device AA:BB:CC:DD:EE:FF Mouse\n".into()),
            )];
            let mut out = Vec::new();
            let mut input = std::io::Cursor::new(answer.as_bytes().to_vec());
            assert!(
                !pair_flow(&mut input, &mut out, &mut f, &d, &d).unwrap(),
                "{answer:?}"
            );
            assert!(!f.log.iter().any(|l| l.contains(" pair ")), "{answer:?}");
        }
        // No devices at all.
        let mut f = fake();
        f.replies = vec![("devices".into(), Some(String::new()))];
        let mut out = Vec::new();
        let mut input = std::io::Cursor::new(Vec::new());
        assert!(!pair_flow(&mut input, &mut out, &mut f, &d, &d).unwrap());
        assert!(String::from_utf8(out).unwrap().contains("No devices found"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn keys_are_kept_on_the_data_partition_through_a_link() {
        let root = tmp("keys");
        let (data, state) = (root.join("data"), root.join("var/lib/bluetooth"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&state).unwrap(); // an empty folder from the image
        assert!(!has_saved_pairings(&data));
        persist_keys(&data, &state).unwrap();
        assert!(std::fs::symlink_metadata(&state)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::write(state.join("key"), "x").unwrap();
        assert!(
            data.join("bluetooth/key").is_file(),
            "written through the link"
        );
        assert!(has_saved_pairings(&data));
        persist_keys(&data, &state).unwrap(); // second time: nothing to do
                                              // A folder that already has content is not replaced.
        let other = root.join("var/lib/other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("keep"), "y").unwrap();
        assert!(persist_keys(&data, &other).is_err());
        assert!(other.join("keep").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
