//! What the menu bar's system area shows: the date and time, the network, Bluetooth and the battery.
//!
//! Everything reads from paths and runs programs through [`Runner`], both injectable, so the whole
//! module is tested with fake files and a fake runner. The desktop (`mouli-desk`) only draws the
//! result.

use crate::bluetooth;
use crate::civil_time::DateTime;
use crate::net;
use crate::wifi::{self, Runner};
use mouli_desk::status::{Battery, Bluetooth, NetKind, Network, SystemStatus};
use std::path::{Path, PathBuf};

/// The smallest UTC offset accepted, in minutes (UTC-12:00).
pub const MIN_OFFSET: i32 = -12 * 60;
/// The largest UTC offset accepted, in minutes (UTC+14:00).
pub const MAX_OFFSET: i32 = 14 * 60;
/// The settings key that keeps the UTC offset, in minutes.
pub const OFFSET_KEY: &str = "utc_offset";
/// Calls of [`Collector::status`] between reads of the slower items (network, Bluetooth, battery).
const SLOW_EVERY: u32 = 5;

/// Where to look, and what to ask.
#[derive(Debug, Clone)]
pub struct SysEnv {
    /// Network adapters (`/sys/class/net`).
    pub sys_net: PathBuf,
    /// Bluetooth adapters (`/sys/class/bluetooth`).
    pub sys_bt: PathBuf,
    /// Power supplies (`/sys/class/power_supply`).
    pub power: PathBuf,
    /// The message bus socket; BlueZ is only asked when it exists.
    pub dbus_socket: PathBuf,
}

impl SysEnv {
    /// The real system.
    #[must_use]
    pub fn real() -> Self {
        Self {
            sys_net: PathBuf::from(net::SYS_NET),
            sys_bt: PathBuf::from(bluetooth::SYS_BLUETOOTH),
            power: PathBuf::from("/sys/class/power_supply"),
            dbus_socket: PathBuf::from("/run/dbus/system_bus_socket"),
        }
    }
}

/// Reads a UTC offset such as `UTC`, `+5`, `+05:30`, `-3:30` or `330` (minutes) and returns it in
/// minutes, or `None` when it is malformed or outside UTC-12:00 to UTC+14:00.
#[must_use]
pub fn parse_offset(text: &str) -> Option<i32> {
    let t = text.trim();
    let t = t
        .strip_prefix("UTC")
        .or_else(|| t.strip_prefix("utc"))
        .unwrap_or(t);
    if t.is_empty() || t.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let (sign, rest) = match t.as_bytes().first()? {
        b'+' => (1, &t[1..]),
        b'-' => (-1, &t[1..]),
        _ => (1, t),
    };
    if rest.is_empty() || rest.len() > 5 || !rest.bytes().all(|b| b.is_ascii_digit() || b == b':') {
        return None;
    }
    let minutes = if let Some((h, m)) = rest.split_once(':') {
        if h.is_empty() || h.len() > 2 || m.len() != 2 || m.contains(':') {
            return None;
        }
        let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
        if m >= 60 {
            return None;
        }
        h * 60 + m
    } else if rest.len() <= 2 {
        rest.parse::<i32>().ok()? * 60
    } else {
        // Three or more digits with no colon: minutes, for example 330.
        let v: i32 = rest.parse().ok()?;
        if rest.len() > 4 {
            return None;
        }
        v
    };
    let v = sign * minutes;
    (MIN_OFFSET..=MAX_OFFSET).contains(&v).then_some(v)
}

/// `UTC` or `UTC+05:30` for an offset in minutes.
#[must_use]
pub fn zone_label(offset_min: i32) -> String {
    if offset_min == 0 {
        return "UTC".into();
    }
    let sign = if offset_min < 0 { '-' } else { '+' };
    let a = offset_min.abs();
    format!("UTC{sign}{:02}:{:02}", a / 60, a % 60)
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The date (`Tue 25 Nov`), the time (`17:42`) and the zone label for `unix` seconds shown at a UTC
/// offset. A time that cannot be shown (before 1970) gives `--:--`.
#[must_use]
pub fn clock(unix: i64, offset_min: i32) -> (String, String, String) {
    let zone = zone_label(offset_min);
    let local = unix.saturating_add(i64::from(offset_min) * 60);
    match DateTime::from_unix(local) {
        Some(dt) => {
            let days = local.div_euclid(86_400);
            let weekday = DAYS[usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0)];
            let month = MONTHS[usize::from(dt.month.clamp(1, 12)) - 1];
            (
                format!("{weekday} {} {month}", dt.day),
                format!("{:02}:{:02}", dt.hour, dt.minute),
                zone,
            )
        }
        None => (String::new(), "--:--".into(), zone),
    }
}

/// Reads the output of `iw dev <name> link`: the network name and the signal in dBm. `None` when
/// not connected.
#[must_use]
pub fn parse_iw_link(text: &str) -> Option<(String, i32)> {
    if !text.contains("Connected to") {
        return None;
    }
    let mut ssid = None;
    let mut dbm = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(s) = l.strip_prefix("SSID:") {
            ssid = Some(
                bluetooth::clean_name(s)
                    .chars()
                    .take(32)
                    .collect::<String>(),
            );
        } else if let Some(s) = l.strip_prefix("signal:") {
            dbm = s
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<i32>().ok());
        }
    }
    Some((ssid.filter(|s| !s.is_empty())?, dbm.unwrap_or(-80)))
}

/// Signal bars (1 to 4) from a signal strength in dBm.
#[must_use]
pub fn bars_for_dbm(dbm: i32) -> u8 {
    match dbm {
        d if d >= -55 => 4,
        d if d >= -65 => 3,
        d if d >= -75 => 2,
        _ => 1,
    }
}

/// Whether `bluetoothctl show` says the adapter is powered on.
#[must_use]
pub fn parse_powered(show: &str) -> bool {
    show.lines().any(|l| {
        l.trim()
            .strip_prefix("Powered:")
            .is_some_and(|v| v.trim() == "yes")
    })
}

/// The first battery under a power-supply directory (`BAT0`, `BAT1`, ...).
#[must_use]
pub fn read_battery(dir: &Path) -> Option<Battery> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("BAT"))
        .collect();
    names.sort();
    let p = dir.join(names.first()?);
    let percent: u8 = std::fs::read_to_string(p.join("capacity"))
        .ok()?
        .trim()
        .parse::<u16>()
        .ok()?
        .min(100)
        .try_into()
        .ok()?;
    let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
    Some(Battery {
        percent,
        charging: status.trim().eq_ignore_ascii_case("charging"),
    })
}

/// The network in use: a cable if one is plugged in, else the Wi-Fi network joined, else nothing.
pub fn network<R: Runner>(r: &mut R, env: &SysEnv) -> Network {
    if let Some(name) = net::wired_interfaces(&env.sys_net)
        .into_iter()
        .find(|i| net::has_carrier(&env.sys_net, i))
    {
        return Network {
            kind: NetKind::Ethernet,
            name,
            bars: 0,
        };
    }
    for iface in wifi::wireless_interfaces(&env.sys_net) {
        let up = std::fs::read_to_string(env.sys_net.join(&iface).join("operstate"))
            .is_ok_and(|s| s.trim() == "up");
        if !up {
            continue;
        }
        if let Some((ssid, dbm)) = r
            .run("iw", &["dev", &iface, "link"])
            .and_then(|t| parse_iw_link(&t))
        {
            return Network {
                kind: NetKind::Wifi,
                name: ssid,
                bars: bars_for_dbm(dbm),
            };
        }
    }
    Network::default()
}

/// Bluetooth: no adapter, an adapter whose service is not running, on, or on with devices connected.
pub fn bluetooth_state<R: Runner>(r: &mut R, env: &SysEnv) -> Bluetooth {
    if bluetooth::adapters(&env.sys_bt).is_empty() {
        return Bluetooth::NoAdapter;
    }
    // Without the message bus BlueZ is not running, and bluetoothctl would wait for it.
    if !env.dbus_socket.exists() {
        return Bluetooth::Off;
    }
    let show = r
        .run(bluetooth::BLUETOOTHCTL, &["--timeout", "3", "show"])
        .unwrap_or_default();
    if !parse_powered(&show) {
        return Bluetooth::Off;
    }
    let connected = r
        .run(
            bluetooth::BLUETOOTHCTL,
            &["--timeout", "3", "devices", "Connected"],
        )
        .map_or(0, |t| bluetooth::parse_devices(&t).len());
    match u8::try_from(connected) {
        Ok(0) => Bluetooth::On,
        Ok(n) => Bluetooth::Connected(n),
        Err(_) => Bluetooth::Connected(u8::MAX),
    }
}

/// Builds [`SystemStatus`] values. The clock is read on every call; the network, Bluetooth and
/// battery, which cost a program run each, only every few calls.
pub struct Collector {
    env: SysEnv,
    calls: u32,
    slow: Option<(Network, Bluetooth, Option<Battery>)>,
}

impl Collector {
    /// A collector for `env`.
    #[must_use]
    pub fn new(env: SysEnv) -> Self {
        Self {
            env,
            calls: 0,
            slow: None,
        }
    }

    /// The current status for `now_unix` seconds at `offset_min`.
    pub fn status<R: Runner>(&mut self, r: &mut R, now_unix: i64, offset_min: i32) -> SystemStatus {
        if self.slow.is_none() || self.calls.is_multiple_of(SLOW_EVERY) {
            self.slow = Some((
                network(r, &self.env),
                bluetooth_state(r, &self.env),
                read_battery(&self.env.power),
            ));
        }
        self.calls = self.calls.wrapping_add(1);
        let (date, time, zone) = clock(now_unix, offset_min);
        let (network, bluetooth, battery) = self.slow.clone().unwrap_or_default();
        SystemStatus {
            time,
            date,
            zone,
            network,
            bluetooth,
            battery,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        iw: Option<String>,
        show: Option<String>,
        connected: Option<String>,
        log: Vec<String>,
    }

    impl Runner for Fake {
        fn run(&mut self, cmd: &str, args: &[&str]) -> Option<String> {
            self.log.push(format!("{cmd} {}", args.join(" ")));
            match (cmd, args.last().copied()) {
                ("iw", _) => self.iw.clone(),
                (_, Some("show")) => self.show.clone(),
                (_, Some("Connected")) => self.connected.clone(),
                _ => None,
            }
        }
        fn pause(&mut self, _: std::time::Duration) {}
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-sysinfo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn env(root: &Path) -> SysEnv {
        SysEnv {
            sys_net: root.join("net"),
            sys_bt: root.join("bt"),
            power: root.join("power"),
            dbus_socket: root.join("dbus-socket"),
        }
    }

    fn nic(root: &Path, name: &str, wireless: bool, carrier: &str, operstate: &str) {
        let d = root.join("net").join(name);
        std::fs::create_dir_all(d.join("device")).unwrap();
        std::fs::write(d.join("type"), "1\n").unwrap();
        std::fs::write(d.join("carrier"), format!("{carrier}\n")).unwrap();
        std::fs::write(d.join("operstate"), format!("{operstate}\n")).unwrap();
        if wireless {
            std::fs::create_dir_all(d.join("wireless")).unwrap();
        }
    }

    #[test]
    fn time_zone_offsets_are_read_strictly() {
        for (text, want) in [
            ("UTC", Some(0)),
            ("utc", Some(0)),
            ("Z", Some(0)),
            ("", Some(0)),
            ("+5", Some(300)),
            ("+05", Some(300)),
            ("+05:30", Some(330)),
            ("UTC+05:30", Some(330)),
            ("-3:30", Some(-210)),
            ("-12:00", Some(-720)),
            ("+14:00", Some(840)),
            ("330", Some(330)),
            ("  +1  ", Some(60)),
            ("+14:01", None),
            ("-12:01", None),
            ("+15", None),
            ("-13", None),
            ("+5:7", None),
            ("+05:60", None),
            ("+5:30:00", None),
            ("abc", None),
            ("+", None),
            ("+-5", None),
            ("1234567", None),
            ("+05:3x", None),
            ("--5", None),
        ] {
            assert_eq!(parse_offset(text), want, "{text:?}");
        }
    }

    #[test]
    fn zones_are_labelled_for_people() {
        assert_eq!(zone_label(0), "UTC");
        assert_eq!(zone_label(330), "UTC+05:30");
        assert_eq!(zone_label(-210), "UTC-03:30");
        assert_eq!(zone_label(840), "UTC+14:00");
        for m in [-720, -210, 0, 60, 330, 840] {
            assert_eq!(parse_offset(&zone_label(m)), Some(m), "round trip {m}");
        }
    }

    #[test]
    fn the_clock_shows_weekday_date_and_time() {
        // 2026-11-25 17:42:09 UTC is a Wednesday.
        let unix = 1_795_628_529;
        let dt = DateTime::from_unix(unix).unwrap();
        assert_eq!(
            (dt.year, dt.month, dt.day, dt.hour, dt.minute),
            (2026, 11, 25, 17, 42)
        );
        let (date, time, zone) = clock(unix, 0);
        assert_eq!(
            (date.as_str(), time.as_str(), zone.as_str()),
            ("Wed 25 Nov", "17:42", "UTC")
        );
        // The same instant in India is 23:12 on the same day.
        let (date, time, zone) = clock(unix, 330);
        assert_eq!(
            (date.as_str(), time.as_str(), zone.as_str()),
            ("Wed 25 Nov", "23:12", "UTC+05:30")
        );
        // An offset that crosses midnight changes the date and the weekday.
        let (date, time, _) = clock(unix, 420);
        assert_eq!((date.as_str(), time.as_str()), ("Thu 26 Nov", "00:42"));
        let (date, time, _) = clock(unix, -1080);
        assert_eq!((date.as_str(), time.as_str()), ("Tue 24 Nov", "23:42"));
        // The epoch was a Thursday.
        assert_eq!(clock(0, 0).0, "Thu 1 Jan");
        // Before 1970 there is nothing to show.
        assert_eq!(clock(-86_400 * 400, 0).1, "--:--");
        // A leap day.
        let (d, _, _) = clock(1_709_164_800, 0); // 2024-02-29 00:00 UTC
        assert_eq!(d, "Thu 29 Feb");
    }

    #[test]
    fn iw_link_output_gives_the_network_and_signal() {
        let up = "Connected to 66:77:88:99:aa:bb (on wlan0)\n\tSSID: Home Net\n\tfreq: 5180\n\tsignal: -58 dBm\n\ttx bitrate: 300.0 MBit/s\n";
        assert_eq!(parse_iw_link(up), Some(("Home Net".to_string(), -58)));
        assert_eq!(parse_iw_link("Not connected.\n"), None);
        assert_eq!(parse_iw_link(""), None);
        // A name with control characters is cleaned and shortened.
        let evil = format!(
            "Connected to x\n\tSSID: a\x1b[2Jb{}\n\tsignal: -40 dBm\n",
            "y".repeat(100)
        );
        let (name, _) = parse_iw_link(&evil).unwrap();
        assert!(
            !name.contains('\x1b') && name.chars().count() <= 32,
            "{name:?}"
        );
        // No signal line: a weak default, still connected.
        assert_eq!(
            parse_iw_link("Connected to x\n\tSSID: N\n"),
            Some(("N".to_string(), -80))
        );
        assert_eq!(
            parse_iw_link("Connected to x\n"),
            None,
            "no name, nothing to show"
        );
    }

    #[test]
    fn signal_strength_maps_to_bars() {
        for (dbm, bars) in [
            (-30, 4),
            (-55, 4),
            (-56, 3),
            (-65, 3),
            (-66, 2),
            (-75, 2),
            (-76, 1),
            (-100, 1),
        ] {
            assert_eq!(bars_for_dbm(dbm), bars, "{dbm}");
        }
    }

    #[test]
    fn a_cable_beats_wifi_and_wifi_beats_nothing() {
        let root = tmp("net");
        let e = env(&root);
        let mut f = Fake {
            iw: Some("Connected to x\n\tSSID: Cafe\n\tsignal: -60 dBm\n".into()),
            ..Fake::default()
        };
        nic(&root, "eth0", false, "0", "down");
        nic(&root, "wlan0", true, "1", "up");
        let n = network(&mut f, &e);
        assert_eq!(
            (n.kind, n.name.as_str(), n.bars),
            (NetKind::Wifi, "Cafe", 3)
        );
        nic(&root, "eth0", false, "1", "up");
        let n = network(&mut f, &e);
        assert_eq!((n.kind, n.name.as_str()), (NetKind::Ethernet, "eth0"));
        // Wi-Fi that is not associated is not shown as connected.
        let root2 = tmp("net2");
        nic(&root2, "wlan0", true, "1", "up");
        let mut none = Fake {
            iw: Some("Not connected.\n".into()),
            ..Fake::default()
        };
        assert_eq!(network(&mut none, &env(&root2)).kind, NetKind::None);
        // An adapter that is down is not even asked.
        let root3 = tmp("net3");
        nic(&root3, "wlan0", true, "0", "down");
        let mut quiet = Fake::default();
        assert_eq!(network(&mut quiet, &env(&root3)).kind, NetKind::None);
        assert!(quiet.log.is_empty());
        // No adapters at all.
        assert_eq!(network(&mut quiet, &env(&tmp("net4"))).kind, NetKind::None);
        for r in [root, root2, root3] {
            let _ = std::fs::remove_dir_all(r);
        }
    }

    #[test]
    fn bluetooth_states_follow_the_adapter_the_service_and_the_devices() {
        let root = tmp("bt");
        let e = env(&root);
        let mut f = Fake::default();
        assert_eq!(bluetooth_state(&mut f, &e), Bluetooth::NoAdapter);
        assert!(f.log.is_empty(), "nothing is run without an adapter");
        std::fs::create_dir_all(root.join("bt/hci0")).unwrap();
        assert_eq!(
            bluetooth_state(&mut f, &e),
            Bluetooth::Off,
            "no message bus yet"
        );
        assert!(
            f.log.is_empty(),
            "bluetoothctl is not run without the bus (it would wait)"
        );
        std::fs::write(&e.dbus_socket, b"").unwrap();
        f.show = Some("Controller AA\n\tPowered: no\n".into());
        assert_eq!(bluetooth_state(&mut f, &e), Bluetooth::Off);
        f.show = Some("Controller AA\n\tPowered: yes\n".into());
        f.connected = Some(String::new());
        assert_eq!(bluetooth_state(&mut f, &e), Bluetooth::On);
        f.connected =
            Some("Device 11:22:33:44:55:66 Mouse\nDevice AA:BB:CC:DD:EE:FF Keys\n".into());
        assert_eq!(bluetooth_state(&mut f, &e), Bluetooth::Connected(2));
        f.show = None;
        assert_eq!(
            bluetooth_state(&mut f, &e),
            Bluetooth::Off,
            "bluetoothctl failed"
        );
        assert!(
            f.log.iter().all(|l| l.contains("--timeout")),
            "every call has a time limit"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_battery_is_read_from_the_power_supply() {
        let root = tmp("power");
        assert_eq!(read_battery(&root), None, "no battery");
        assert_eq!(read_battery(&root.join("missing")), None);
        let ac = root.join("AC");
        std::fs::create_dir_all(&ac).unwrap();
        assert_eq!(read_battery(&root), None, "mains is not a battery");
        let bat = root.join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("capacity"), "87\n").unwrap();
        std::fs::write(bat.join("status"), "Charging\n").unwrap();
        assert_eq!(
            read_battery(&root),
            Some(Battery {
                percent: 87,
                charging: true
            })
        );
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        assert_eq!(
            read_battery(&root),
            Some(Battery {
                percent: 87,
                charging: false
            })
        );
        std::fs::write(bat.join("capacity"), "250\n").unwrap();
        assert_eq!(read_battery(&root).unwrap().percent, 100, "clamped");
        std::fs::write(bat.join("capacity"), "garbage\n").unwrap();
        assert_eq!(read_battery(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_collector_reads_the_clock_every_time_and_the_slow_items_now_and_then() {
        let root = tmp("collector");
        let e = env(&root);
        nic(&root, "wlan0", true, "1", "up");
        let mut f = Fake {
            iw: Some("Connected to x\n\tSSID: A\n\tsignal: -50 dBm\n".into()),
            ..Fake::default()
        };
        let mut c = Collector::new(e);
        let s1 = c.status(&mut f, 1_795_628_529, 0);
        assert_eq!(s1.time, "17:42");
        assert_eq!(s1.network.name, "A");
        let runs_after_first = f.log.len();
        assert!(runs_after_first >= 1);
        // The next calls change the time but do not run programs again.
        let s2 = c.status(&mut f, 1_795_628_529 + 60, 0);
        assert_eq!(s2.time, "17:43");
        for i in 0..(SLOW_EVERY - 2) {
            c.status(&mut f, 1_795_628_529 + 61 + i64::from(i), 0);
        }
        assert_eq!(f.log.len(), runs_after_first, "no extra program runs");
        // After SLOW_EVERY calls it looks again.
        f.iw = Some("Not connected.\n".into());
        c.status(&mut f, 1_795_628_600, 0);
        let s = c.status(&mut f, 1_795_628_601, 0);
        assert_eq!(
            s.network.kind,
            NetKind::None,
            "the change is noticed: {s:?}"
        );
        // A new offset applies at once.
        assert_eq!(c.status(&mut f, 1_795_628_529, 330).time, "23:12");
        let _ = std::fs::remove_dir_all(&root);
    }
}
