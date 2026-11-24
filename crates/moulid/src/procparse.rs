//! Parsers for Linux procfs and sysfs text snapshots.
//!
//! This module provides allocation-based, non-fatal parsers for selected Linux
//! status files. All parsing functions accept `&str` and use only the Rust
//! standard library. Malformed or incomplete records are skipped.
//!
//! Supported formats:
//! - `/proc/partitions`
//! - `/proc/mounts`
//! - `/proc/meminfo`
//! - `/proc/net/wireless`
//! - `/sys/.../uevent`
//! - `/etc/os-release`
//! - `/proc/uptime`
//! - `/proc/loadavg`
//! - `/proc/cmdline`

use std::collections::BTreeMap;

/// A partition entry from `/proc/partitions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Major device number.
    pub major: u32,
    /// Minor device number.
    pub minor: u32,
    /// Partition size in 1 KiB blocks.
    pub blocks_1k: u64,
    /// Device name.
    pub name: String,
}

/// A mounted filesystem from `/proc/mounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Device or source name.
    pub device: String,
    /// Mount point.
    pub mount_point: String,
    /// Filesystem type.
    pub fs_type: String,
    /// Mount options.
    pub options: Vec<String>,
}

/// A wireless network interface from `/proc/net/wireless`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirelessLink {
    /// Network interface name.
    pub iface: String,
    /// Wireless interface status flags.
    pub status: u32,
    /// Link quality.
    pub link_quality: i32,
    /// Signal strength in dBm.
    pub signal_dbm: i32,
    /// Noise level in dBm.
    pub noise_dbm: i32,
}

/// Determines whether a device name represents a whole disk.
///
/// Recognizes common Linux disk naming conventions:
/// - `sdX`, `vdX`, `hdX`, `xvdX`
/// - `nvmeXnY`
/// - `mmcblkX`
///
/// Partition names such as `sda1`, `nvme0n1p1`, and `mmcblk0p2`
/// return `false`. Virtual devices such as loop, ram, device mapper,
/// optical, and zram devices also return `false`.
#[must_use]
pub fn is_whole_disk(name: &str) -> bool {
    if ["loop", "ram", "sr", "dm-", "zram"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return false;
    }

    for prefix in ["sd", "vd", "hd", "xvd"] {
        if let Some(suffix) = name.strip_prefix(prefix) {
            return !suffix.is_empty()
                && suffix
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_ascii_digit());
        }
    }

    for prefix in ["nvme", "mmcblk"] {
        if let Some(suffix) = name.strip_prefix(prefix) {
            if let Some(position) = suffix.rfind('p') {
                let partition_number = suffix.get(position + 1..).unwrap_or_default();

                if !partition_number.is_empty()
                    && partition_number.chars().all(|ch| ch.is_ascii_digit())
                {
                    return false;
                }
            }
        }
    }

    true
}

/// Parses `/proc/partitions`.
///
/// The header and blank lines are ignored. Malformed records are skipped.
#[must_use]
pub fn parse_partitions(text: &str) -> Vec<Partition> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();

            let major = fields.next()?.parse::<u32>().ok()?;
            let minor = fields.next()?.parse::<u32>().ok()?;
            let blocks_1k = fields.next()?.parse::<u64>().ok()?;
            let name = fields.next()?;

            if fields.next().is_some() || name.is_empty() {
                return None;
            }

            Some(Partition {
                major,
                minor,
                blocks_1k,
                name: name.to_owned(),
            })
        })
        .collect()
}

/// Decodes the four octal escape sequences used in `/proc/mounts`.
///
/// Supported sequences:
/// - `\040`: space
/// - `\011`: tab
/// - `\012`: newline
/// - `\134`: backslash
fn decode_mount_field(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let mut escape = String::new();

            for _ in 0..3 {
                if let Some(next_char) = chars.next() {
                    escape.push(next_char);
                }
            }

            match escape.as_str() {
                "040" => result.push(' '),
                "011" => result.push('\t'),
                "012" => result.push('\n'),
                "134" => result.push('\\'),
                _ => {
                    result.push('\\');
                    result.push_str(&escape);
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Parses `/proc/mounts`.
///
/// Decodes octal escapes in the device and mount-point fields.
/// Malformed records are skipped.
#[must_use]
pub fn parse_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();

            let device = fields.next()?;
            let mount_point = fields.next()?;
            let fs_type = fields.next()?;
            let options = fields.next()?;

            Some(Mount {
                device: decode_mount_field(device),
                mount_point: decode_mount_field(mount_point),
                fs_type: fs_type.to_owned(),
                options: options.split(',').map(str::to_owned).collect(),
            })
        })
        .collect()
}

/// Parses `/proc/meminfo`.
///
/// Returns a sorted map of memory keys to numeric values in kB.
/// Entries without a valid numeric value are ignored.
/// Values without a unit, such as `HugePages_Total`, are retained.
#[must_use]
pub fn parse_meminfo(text: &str) -> BTreeMap<String, u64> {
    let mut result = BTreeMap::new();

    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };

        let Some(number) = value.split_whitespace().next() else {
            continue;
        };

        if let Ok(parsed) = number.parse::<u64>() {
            let key = key.trim();

            if !key.is_empty() {
                result.insert(key.to_owned(), parsed);
            }
        }
    }

    result
}

/// Parses an integer that may have a trailing decimal point.
///
/// Wireless statistics commonly use representations such as `70.` and
/// `-40.` despite containing integer values.
fn parse_wireless_integer(value: &str) -> Option<i32> {
    value.strip_suffix('.').unwrap_or(value).parse::<i32>().ok()
}

/// Parses `/proc/net/wireless`.
///
/// Skips the two header lines. Status is interpreted as hexadecimal.
/// Malformed interface records are skipped.
#[must_use]
pub fn parse_net_wireless(text: &str) -> Vec<WirelessLink> {
    text.lines()
        .skip(2)
        .filter_map(|line| {
            let (iface, statistics) = line.split_once(':')?;

            let iface = iface.trim();

            if iface.is_empty() {
                return None;
            }

            let mut fields = statistics.split_whitespace();

            let status = u32::from_str_radix(fields.next()?, 16).ok()?;
            let link_quality = parse_wireless_integer(fields.next()?)?;
            let signal_dbm = parse_wireless_integer(fields.next()?)?;
            let noise_dbm = parse_wireless_integer(fields.next()?)?;

            Some(WirelessLink {
                iface: iface.to_owned(),
                status,
                link_quality,
                signal_dbm,
                noise_dbm,
            })
        })
        .collect()
}

/// Parses a sysfs `uevent` file.
///
/// Extracts `KEY=VALUE` pairs into a sorted map.
/// Empty keys and lines without an equals sign are ignored.
#[must_use]
pub fn parse_uevent(text: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();

    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();

        if !key.is_empty() {
            result.insert(key.to_owned(), value.to_owned());
        }
    }

    result
}

/// Parses a PCI vendor/device identifier.
///
/// Accepts hexadecimal identifiers in the form `8086:2725`.
/// Hexadecimal letters are case-insensitive.
///
/// Returns `(vendor_id, device_id)` when both identifiers are valid.
#[must_use]
pub fn parse_pci_id(s: &str) -> Option<(u16, u16)> {
    let (vendor, device) = s.trim().split_once(':')?;

    if vendor.len() != 4 || device.len() != 4 {
        return None;
    }

    let vendor_id = u16::from_str_radix(vendor, 16).ok()?;
    let device_id = u16::from_str_radix(device, 16).ok()?;

    Some((vendor_id, device_id))
}

/// Removes matching outer single or double quotes from a value.
fn strip_outer_quotes(value: &str) -> &str {
    let value = value.trim();

    if value.len() >= 2 {
        let mut chars = value.chars();
        let first = chars.next();
        let last = chars.next_back();

        if matches!(
            (first, last),
            (Some('\''), Some('\'')) | (Some('"'), Some('"'))
        ) {
            return value.get(1..value.len() - 1).unwrap_or(value);
        }
    }

    value
}

/// Decodes escaped double quotes inside an os-release value.
///
/// Other backslashes are preserved.
fn decode_os_release_value(value: &str) -> String {
    let value = strip_outer_quotes(value);

    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&'"') {
            let _ = chars.next();
            result.push('"');
        } else {
            result.push(ch);
        }
    }

    result
}

/// Parses `/etc/os-release`.
///
/// Supports unquoted values, single-quoted values, double-quoted values,
/// and escaped double quotes inside double-quoted values.
/// Blank lines and comments are ignored.
#[must_use]
pub fn parse_os_release(text: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();

    for line in text.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();

        if key.is_empty() {
            continue;
        }

        result.insert(key.to_owned(), decode_os_release_value(value.trim()));
    }

    result
}

/// Parses `/proc/uptime`.
///
/// Returns the first numeric field, representing system uptime in seconds.
#[must_use]
pub fn parse_uptime(text: &str) -> Option<f64> {
    let value = text.split_whitespace().next()?;
    let uptime = value.parse::<f64>().ok()?;

    if uptime.is_finite() && uptime >= 0.0 {
        Some(uptime)
    } else {
        None
    }
}

/// Parses `/proc/loadavg`.
///
/// Returns the 1-minute, 5-minute, and 15-minute load averages.
/// Additional process and PID fields are ignored.
#[must_use]
pub fn parse_loadavg(text: &str) -> Option<(f64, f64, f64)> {
    let mut fields = text.split_whitespace();

    let one = fields.next()?.parse::<f64>().ok()?;
    let five = fields.next()?.parse::<f64>().ok()?;
    let fifteen = fields.next()?.parse::<f64>().ok()?;

    if [one, five, fifteen]
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
    {
        Some((one, five, fifteen))
    } else {
        None
    }
}

/// Tokenizes kernel command-line parameters.
///
/// Double quotes group whitespace into a single token.
/// Quote delimiters are removed. A double-quoted value may contain spaces.
fn tokenize_cmdline(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut started = false;

    for ch in text.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(ch);
                started = true;
            }
        }
    }

    if started {
        tokens.push(current);
    }

    tokens
}

/// Parses `/proc/cmdline`.
///
/// Returns kernel parameters as a sorted map:
/// - `key=value` becomes `Some(value)`.
/// - A bare flag becomes `None`.
/// - Double-quoted values can contain spaces.
/// - `--` terminates parsing; subsequent tokens are ignored.
///
/// When a key occurs more than once, its last occurrence is retained.
#[must_use]
pub fn parse_cmdline(text: &str) -> BTreeMap<String, Option<String>> {
    let mut result = BTreeMap::new();

    for token in tokenize_cmdline(text) {
        if token == "--" {
            break;
        }

        if token.is_empty() {
            continue;
        }

        let (key, value) = match token.split_once('=') {
            Some((key, value)) => (key, Some(value.to_owned())),
            None => (token.as_str(), None),
        };

        if !key.is_empty() {
            result.insert(key.to_owned(), value);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_realistic_snapshot() {
        let input = "\
major minor  #blocks  name

   7        0      65536 loop0
   8        0  976762584 sda
   8        1     524288 sda1
 259        0  976762584 nvme0n1
 259        1     524288 nvme0n1p1
 179        0   62333952 mmcblk0
 179        2    1048576 mmcblk0p2
 252        0   10485760 dm-0
";

        let partitions = parse_partitions(input);

        assert_eq!(partitions.len(), 8);

        assert_eq!(
            partitions.first(),
            Some(&Partition {
                major: 7,
                minor: 0,
                blocks_1k: 65536,
                name: "loop0".to_owned(),
            })
        );

        assert_eq!(
            partitions.get(4).map(|partition| partition.name.as_str()),
            Some("nvme0n1p1")
        );

        assert_eq!(
            partitions.get(6).map(|partition| partition.name.as_str()),
            Some("mmcblk0p2")
        );
    }

    #[test]
    fn partitions_skip_malformed_records() {
        let input = "\
major minor  #blocks  name
8 0 1024 sda
invalid
8 x 1024 sdb
8 1 bad sda1
8 2 2048
8 3 4096 sdc extra
";

        let partitions = parse_partitions(input);

        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions.first().map(|p| p.name.as_str()), Some("sda"));
    }

    #[test]
    fn whole_disk_detection() {
        let cases = [
            ("sda", true),
            ("sdb", true),
            ("sda1", false),
            ("sdb12", false),
            ("vda", true),
            ("vda1", false),
            ("hda", true),
            ("hda2", false),
            ("xvda", true),
            ("xvda1", false),
            ("nvme0n1", true),
            ("nvme0n1p1", false),
            ("nvme1n2p12", false),
            ("mmcblk0", true),
            ("mmcblk0p2", false),
            ("loop0", false),
            ("ram0", false),
            ("sr0", false),
            ("dm-0", false),
            ("zram0", false),
        ];

        for (name, expected) in cases {
            assert_eq!(is_whole_disk(name), expected, "{name}");
        }
    }

    #[test]
    fn mounts_decode_escaped_paths() {
        let input = "\
/dev/sda1 / ext4 rw,relatime 0 1
/dev/sdb1 /media/My\\040Drive ext4 rw,nosuid,nodev 0 0
/dev/sdc1 /media/Tab\\011Drive ext4 ro 0 0
/dev/sdd1 /media/New\\012Drive ext4 rw 0 0
/dev/sde1 /media/Back\\134Slash ext4 rw 0 0
";

        let mounts = parse_mounts(input);

        assert_eq!(mounts.len(), 5);

        assert_eq!(
            mounts.get(1).map(|m| m.mount_point.as_str()),
            Some("/media/My Drive")
        );

        assert_eq!(
            mounts.get(2).map(|m| m.mount_point.as_str()),
            Some("/media/Tab\tDrive")
        );

        assert_eq!(
            mounts.get(3).map(|m| m.mount_point.as_str()),
            Some("/media/New\nDrive")
        );

        assert_eq!(
            mounts.get(4).map(|m| m.mount_point.as_str()),
            Some("/media/Back\\Slash")
        );

        assert_eq!(
            mounts.first().map(|m| m.options.clone()),
            Some(vec!["rw".to_owned(), "relatime".to_owned()])
        );
    }

    #[test]
    fn mounts_skip_incomplete_records() {
        let input = "\
/dev/sda1 / ext4 rw 0 1
/dev/sdb1 /data
malformed
";

        assert_eq!(parse_mounts(input).len(), 1);
    }

    #[test]
    fn meminfo_realistic_snapshot() {
        let input = "\
MemTotal:       16384256 kB
MemFree:         1024000 kB
MemAvailable:    8192000 kB
Buffers:          128000 kB
Cached:          4096000 kB
SwapCached:            0 kB
Active:          2048000 kB
Inactive:        4096000 kB
HugePages_Total:       0
HugePages_Free:        0
HugePages_Rsvd:        0
Hugepagesize:       2048 kB
InvalidEntry: invalid kB
NoNumber:
";

        let meminfo = parse_meminfo(input);

        assert_eq!(meminfo.get("MemTotal"), Some(&16_384_256));
        assert_eq!(meminfo.get("MemFree"), Some(&1_024_000));
        assert_eq!(meminfo.get("MemAvailable"), Some(&8_192_000));
        assert_eq!(meminfo.get("HugePages_Total"), Some(&0));
        assert_eq!(meminfo.get("HugePages_Free"), Some(&0));
        assert_eq!(meminfo.get("Hugepagesize"), Some(&2048));
        assert!(!meminfo.contains_key("InvalidEntry"));
        assert!(!meminfo.contains_key("NoNumber"));
    }

    #[test]
    fn wireless_realistic_snapshot() {
        let input = "\
Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE
 face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22
wlan0: 0000   70.  -40.  -256.       0      0      0      0      0        0
wlp2s0: 0000   65.  -45.  -95.       0      0      0      0      0        0
";

        let links = parse_net_wireless(input);

        assert_eq!(links.len(), 2);

        assert_eq!(
            links.first(),
            Some(&WirelessLink {
                iface: "wlan0".to_owned(),
                status: 0,
                link_quality: 70,
                signal_dbm: -40,
                noise_dbm: -256,
            })
        );

        assert_eq!(
            links.get(1),
            Some(&WirelessLink {
                iface: "wlp2s0".to_owned(),
                status: 0,
                link_quality: 65,
                signal_dbm: -45,
                noise_dbm: -95,
            })
        );
    }

    #[test]
    fn wireless_skips_malformed_records() {
        let input = "\
header one
header two
wlan0: 0000 70. -40. -256.
wlan1: invalid 70. -40. -256.
wlan2: 0000 bad -40. -256.
wlan3: 0000 60. -45.
";

        assert_eq!(parse_net_wireless(input).len(), 1);
    }

    #[test]
    fn uevent_intel_wifi_card() {
        let input = "\
DRIVER=iwlwifi
PCI_CLASS=028000
PCI_ID=8086:2725
PCI_SUBSYS_ID=8086:0024
PCI_SLOT_NAME=0000:00:14.3
MODALIAS=pci:v00008086d00002725sv00008086sd00000024bc02sc80i00
";

        let event = parse_uevent(input);

        assert_eq!(event.get("DRIVER").map(String::as_str), Some("iwlwifi"));
        assert_eq!(event.get("PCI_ID").map(String::as_str), Some("8086:2725"));
        assert!(event.contains_key("MODALIAS"));

        assert_eq!(parse_pci_id("8086:2725"), Some((0x8086, 0x2725)));
        assert_eq!(parse_pci_id("8086:ABCD"), Some((0x8086, 0xabcd)));
        assert_eq!(parse_pci_id("abcd:1234"), Some((0xabcd, 0x1234)));
    }

    #[test]
    fn uevent_and_pci_id_malformed_input() {
        let input = "\
DRIVER=iwlwifi
INVALID_LINE
=empty_key
";

        let event = parse_uevent(input);

        assert_eq!(event.len(), 1);
        assert_eq!(event.get("DRIVER").map(String::as_str), Some("iwlwifi"));

        assert_eq!(parse_pci_id(""), None);
        assert_eq!(parse_pci_id("8086"), None);
        assert_eq!(parse_pci_id("8086:272"), None);
        assert_eq!(parse_pci_id("GGGG:2725"), None);
        assert_eq!(parse_pci_id("10000:2725"), None);
        assert_eq!(parse_pci_id("8086:10000"), None);
        assert_eq!(parse_pci_id("8086:2725:1234"), None);
    }

    #[test]
    fn os_release_realistic_snapshot() {
        let input = r#"NAME="Ubuntu"
VERSION="24.04.1 LTS (Noble Numbat)"
ID=ubuntu
ID_LIKE=debian
PRETTY_NAME="Ubuntu 24.04.1 LTS"
VERSION_ID="24.04"
HOME_URL="https://www.ubuntu.com/"
SUPPORT_URL="https://help.ubuntu.com/"
BUG_REPORT_URL="https://bugs.launchpad.net/ubuntu/"
CUSTOM='single quoted value'
ESCAPED="a \"quoted\" value"
"#;

        let release = parse_os_release(input);

        assert_eq!(release.get("NAME").map(String::as_str), Some("Ubuntu"));

        assert_eq!(
            release.get("VERSION").map(String::as_str),
            Some("24.04.1 LTS (Noble Numbat)")
        );

        assert_eq!(release.get("ID").map(String::as_str), Some("ubuntu"));
        assert_eq!(release.get("ID_LIKE").map(String::as_str), Some("debian"));

        assert_eq!(
            release.get("PRETTY_NAME").map(String::as_str),
            Some("Ubuntu 24.04.1 LTS")
        );

        assert_eq!(
            release.get("CUSTOM").map(String::as_str),
            Some("single quoted value")
        );

        assert_eq!(
            release.get("ESCAPED").map(String::as_str),
            Some("a \"quoted\" value")
        );
    }

    #[test]
    fn os_release_skips_comments_and_malformed_lines() {
        let input = "\
# Comment
NAME=Linux
INVALID_LINE
=INVALID
VERSION=
";

        let release = parse_os_release(input);

        assert_eq!(release.len(), 2);
        assert_eq!(release.get("NAME").map(String::as_str), Some("Linux"));
        assert_eq!(release.get("VERSION").map(String::as_str), Some(""));
    }

    #[test]
    fn uptime_realistic_snapshot() {
        assert_eq!(parse_uptime("123456.78 98765.43"), Some(123_456.78));
        assert_eq!(parse_uptime("0.00 0.00"), Some(0.0));
        assert_eq!(parse_uptime("invalid 100.0"), None);
        assert_eq!(parse_uptime(""), None);
        assert_eq!(parse_uptime("-1.0 0.0"), None);
        assert_eq!(parse_uptime("NaN 0.0"), None);
        assert_eq!(parse_uptime("inf 0.0"), None);
    }

    #[test]
    fn loadavg_realistic_snapshot() {
        assert_eq!(
            parse_loadavg("0.42 0.35 0.28 2/1234 5678"),
            Some((0.42, 0.35, 0.28))
        );

        assert_eq!(
            parse_loadavg("1.00 2.00 3.00 1/100 200"),
            Some((1.0, 2.0, 3.0))
        );

        assert_eq!(parse_loadavg(""), None);
        assert_eq!(parse_loadavg("1.0 2.0"), None);
        assert_eq!(parse_loadavg("bad 2.0 3.0"), None);
        assert_eq!(parse_loadavg("-1.0 2.0 3.0"), None);
        assert_eq!(parse_loadavg("NaN 2.0 3.0"), None);
    }

    #[test]
    fn cmdline_realistic_snapshot() {
        let input = r#"BOOT_IMAGE=/boot/bzImage quiet loglevel=3 mouli.name="my box" -- ignored"#;

        let parameters = parse_cmdline(input);

        assert_eq!(
            parameters.get("BOOT_IMAGE"),
            Some(&Some("/boot/bzImage".to_owned()))
        );

        assert_eq!(parameters.get("quiet"), Some(&None));

        assert_eq!(parameters.get("loglevel"), Some(&Some("3".to_owned())));

        assert_eq!(
            parameters.get("mouli.name"),
            Some(&Some("my box".to_owned()))
        );

        assert!(!parameters.contains_key("--"));
        assert!(!parameters.contains_key("ignored"));
    }

    #[test]
    fn cmdline_handles_empty_and_repeated_values() {
        let input = r#"quiet debug= loglevel=3 loglevel=5 name="two words" bare"#;

        let parameters = parse_cmdline(input);

        assert_eq!(parameters.get("quiet"), Some(&None));
        assert_eq!(parameters.get("debug"), Some(&Some(String::new())));

        assert_eq!(parameters.get("loglevel"), Some(&Some("5".to_owned())));

        assert_eq!(parameters.get("name"), Some(&Some("two words".to_owned())));

        assert_eq!(parameters.get("bare"), Some(&None));
    }

    #[test]
    fn cmdline_ignores_everything_after_terminator() {
        let parameters = parse_cmdline("root=/dev/sda1 -- init=/bin/sh debug");

        assert_eq!(parameters.get("root"), Some(&Some("/dev/sda1".to_owned())));

        assert!(!parameters.contains_key("init"));
        assert!(!parameters.contains_key("debug"));
    }

    #[test]
    fn cmdline_handles_malformed_quotes_without_panicking() {
        let parameters = parse_cmdline(r#"quiet name="unfinished value loglevel=3"#);

        assert_eq!(parameters.get("quiet"), Some(&None));

        assert_eq!(
            parameters.get("name"),
            Some(&Some("unfinished value loglevel=3".to_owned()))
        );
    }

    #[test]
    fn empty_inputs_are_safe() {
        assert!(parse_partitions("").is_empty());
        assert!(parse_mounts("").is_empty());
        assert!(parse_meminfo("").is_empty());
        assert!(parse_net_wireless("").is_empty());
        assert!(parse_uevent("").is_empty());
        assert!(parse_os_release("").is_empty());
        assert_eq!(parse_uptime(""), None);
        assert_eq!(parse_loadavg(""), None);
        assert!(parse_cmdline("").is_empty());
    }
}
