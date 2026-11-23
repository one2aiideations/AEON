//! Wired networking: find Ethernet adapters, wait for a cable, get an address by DHCP and read
//! back what was configured.
//!
//! The work is done by the image's BusyBox (`ip`, `udhcpc` and its default script, which sets the
//! address, route and `/etc/resolv.conf`). Nothing here talks to the internet: it only asks the
//! local network for an address. Wi-Fi is not handled (see the OS README).

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

/// Where the kernel lists network interfaces.
pub const SYS_NET: &str = "/sys/class/net";

/// Ethernet-type adapters (not loopback, not wireless, not virtual bridges or tunnels), sorted.
#[must_use]
pub fn wired_interfaces(sys_net: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(sys_net)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let arphrd_ether = std::fs::read_to_string(p.join("type"))
                .ok()
                .is_some_and(|t| t.trim() == "1");
            let wireless = p.join("wireless").exists() || p.join("phy80211").exists();
            // Virtual devices have no `device` link to real hardware, except virtio/QEMU NICs,
            // which do; bridges, tunnels and veth pairs do not.
            let hardware = p.join("device").exists();
            (arphrd_ether && !wireless && hardware).then_some(name)
        })
        .collect();
    out.sort();
    out
}

/// Whether a cable is connected, from the interface's `carrier` file (`1` when up).
#[must_use]
pub fn has_carrier(sys_net: &Path, iface: &str) -> bool {
    std::fs::read_to_string(sys_net.join(iface).join("carrier"))
        .ok()
        .is_some_and(|c| c.trim() == "1")
}

/// First IPv4 address with prefix (`10.0.2.15/24`) in the output of `ip -4 -o addr show`.
#[must_use]
pub fn parse_ipv4(ip_output: &str) -> Option<String> {
    ip_output.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        it.find(|w| *w == "inet")?;
        it.next().map(str::to_string)
    })
}

/// Name servers listed in the text of `/etc/resolv.conf`, in order.
#[must_use]
pub fn parse_nameservers(resolv: &str) -> Vec<String> {
    resolv
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            (it.next() == Some("nameserver")).then(|| it.next().map(str::to_string))?
        })
        .collect()
}

/// Result of trying to get online on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// An address was obtained.
    Up {
        /// Interface name.
        iface: String,
        /// Address with prefix.
        addr: String,
        /// Name servers.
        dns: Vec<String>,
    },
    /// Adapters exist but none has a cable or a DHCP server.
    NoLease(Vec<String>),
    /// No Ethernet adapter was found.
    NoAdapter,
}

impl Link {
    /// One line for the boot screen.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Up { iface, addr, dns } => format!(
                "AEON network: {iface} {addr}, dns {}",
                if dns.is_empty() {
                    "none".to_string()
                } else {
                    dns.join(" ")
                }
            ),
            Self::NoLease(n) => format!(
                "AEON network: {} found but no cable or DHCP answer - offline",
                n.join(", ")
            ),
            Self::NoAdapter => "AEON network: no wired adapter found - offline".into(),
        }
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
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

/// Try each wired adapter in turn: bring it up, wait up to `link_wait` seconds for a cable, ask
/// for an address (about ten seconds at most), and stop at the first that gets one.
pub fn bring_up_wired(link_wait: u32) -> Link {
    let sys = Path::new(SYS_NET);
    let ifaces = wired_interfaces(sys);
    if ifaces.is_empty() {
        return Link::NoAdapter;
    }
    for i in &ifaces {
        let _ = run("ip", &["link", "set", i, "up"]);
    }
    for i in &ifaces {
        for _ in 0..link_wait.saturating_mul(4) {
            if has_carrier(sys, i) {
                break;
            }
            sleep(Duration::from_millis(250));
        }
        if !has_carrier(sys, i) {
            continue;
        }
        if let Some(link) = lease(i) {
            return link;
        }
    }
    Link::NoLease(ifaces)
}

/// Ask for an address on `iface` with DHCP (about ten seconds at most) and describe the result.
pub fn lease(iface: &str) -> Option<Link> {
    // -n: give up if no lease, -q: exit once configured, -t/-T: 3 tries 2 s apart.
    let leased = Command::new("udhcpc")
        .args(["-i", iface, "-n", "-q", "-t", "3", "-T", "2"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !leased {
        return None;
    }
    let addr = run("ip", &["-4", "-o", "addr", "show", "dev", iface])
        .as_deref()
        .and_then(parse_ipv4)?;
    let dns = parse_nameservers(&std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default());
    Some(Link::Up {
        iface: iface.to_string(),
        addr,
        dns,
    })
}

/// Set the system clock from the hardware clock (RTC), so file times and, later, certificate
/// checks are not stuck in 1970. Returns whether it worked. Does not use the network.
pub fn clock_from_rtc() -> bool {
    run("hwclock", &["-s", "-u"]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_iface(
        root: &Path,
        name: &str,
        typ: &str,
        wireless: bool,
        hardware: bool,
        carrier: &str,
    ) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("type"), typ).unwrap();
        std::fs::write(d.join("carrier"), carrier).unwrap();
        if wireless {
            std::fs::create_dir_all(d.join("wireless")).unwrap();
        }
        if hardware {
            std::fs::create_dir_all(d.join("device")).unwrap();
        }
    }

    #[test]
    fn picks_only_real_ethernet() {
        let root = std::env::temp_dir().join(format!("aeond-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fake_iface(&root, "lo", "772\n", false, false, "1\n");
        fake_iface(&root, "eth1", "1\n", false, true, "0\n");
        fake_iface(&root, "eth0", "1\n", false, true, "1\n");
        fake_iface(&root, "wlan0", "1\n", true, true, "1\n");
        fake_iface(&root, "br0", "1\n", false, false, "1\n");
        fake_iface(&root, "tun0", "65534\n", false, false, "1\n");
        assert_eq!(wired_interfaces(&root), ["eth0", "eth1"]);
        assert!(has_carrier(&root, "eth0"));
        assert!(!has_carrier(&root, "eth1") && !has_carrier(&root, "nope"));
        assert!(wired_interfaces(&root.join("missing")).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parses_ip_and_resolv_output() {
        let ip = "2: eth0    inet 10.0.2.15/24 brd 10.0.2.255 scope global eth0\\       valid_lft forever\n";
        assert_eq!(parse_ipv4(ip).as_deref(), Some("10.0.2.15/24"));
        assert_eq!(parse_ipv4(""), None);
        assert_eq!(parse_ipv4("2: eth0 link/ether aa:bb\n"), None);
        let r =
            "# generated\nsearch lan\nnameserver 10.0.2.3\nnameserver  192.168.1.1 \nnameserver\n";
        assert_eq!(parse_nameservers(r), ["10.0.2.3", "192.168.1.1"]);
        assert!(parse_nameservers("").is_empty());
    }

    #[test]
    fn summaries_say_what_happened() {
        let up = Link::Up {
            iface: "eth0".into(),
            addr: "10.0.2.15/24".into(),
            dns: vec!["10.0.2.3".into()],
        };
        assert_eq!(
            up.summary(),
            "AEON network: eth0 10.0.2.15/24, dns 10.0.2.3"
        );
        assert!(Link::NoAdapter.summary().contains("offline"));
        assert!(Link::NoLease(vec!["eth0".into()])
            .summary()
            .contains("eth0 found"));
        let nodns = Link::Up {
            iface: "e".into(),
            addr: "1.2.3.4/8".into(),
            dns: vec![],
        };
        assert!(nodns.summary().ends_with("dns none"));
    }
}
