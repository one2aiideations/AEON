//! MOULI-UTILITY: the system-related functions of MOULI OS in one crate.
//!
//! | Area | Modules |
//! |---|---|
//! | Network | [`net`] (adapters and carrier), [`wifi`] (scan, join, saved networks), [`wifi_saved`], [`known_networks`], [`bluetooth`] |
//! | Time | [`civil_time`] (dates without a time-zone database), [`sntp`] (network time), [`clocksync`] |
//! | Hardware | [`hw`] (probe and run plan), [`hwprobe`], [`hwreport`], [`procparse`] (`/proc` parsers) |
//! | Disks | [`fslabel`] (filesystem labels), [`partition_table`] (reading and checking partition tables) |
//! | Machine | [`system`] (power off, restart) |
//! | Terminal | [`prompt`] (questions, and hidden input for keys) |
//!
//! Everything reads from paths and runs programs through traits or parameters that tests can
//! replace, so the logic is tested with fake `/sys`, `/proc` and commands.
//! Moved here from `moulid` and `mouli-hw`; behaviour and tests are unchanged.

pub mod hw {
    //! Hardware probe and run plan for moulid.
    //!
    //! Rust port of the former C `aeon_hw.c`. Behaviour matched the C version on well-formed input
    //! (checked by a differential test before the C code was retired).
    //!
    //! Known deliberate differences from the C version:
    //! * `probe` reads the whole of `/proc/cpuinfo`; the C version read only the first 64 KiB, which
    //!   undercounts threads on machines with very large cpuinfo files.
    //! * Parsing is line based, so a value never continues onto the next line (the C `strtoull` skipped
    //!   newlines).
    //! * The budget calculation saturates instead of wrapping on absurd inputs.

    #![forbid(unsafe_code)]

    use std::fmt::Write as _;
    use std::fs;

    /// Maximum number of GPUs recorded.
    pub const MAX_GPUS: usize = 8;

    /// PCI vendor ids MOULI knows by name.
    pub mod vendor {
        /// NVIDIA.
        pub const NVIDIA: u32 = 0x10de;
        /// AMD.
        pub const AMD: u32 = 0x1002;
        /// Intel.
        pub const INTEL: u32 = 0x8086;
        /// virtio (virtual machines).
        pub const VIRTIO: u32 = 0x1af4;
        /// QEMU standard VGA.
        pub const QEMU_STD: u32 = 0x1234;
    }

    /// Errors from the hardware parsers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HwError {
        /// `MemTotal` was missing or zero.
        NoMemTotal,
        /// More than [`MAX_GPUS`] GPUs were added.
        TooManyGpus,
    }

    impl std::fmt::Display for HwError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                HwError::NoMemTotal => f.write_str("MemTotal missing or zero"),
                HwError::TooManyGpus => f.write_str("too many GPUs"),
            }
        }
    }

    impl std::error::Error for HwError {}

    /// What the probe found.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Hw {
        /// Logical CPU threads (at least 1 after parsing).
        pub cpu_threads: u32,
        /// Total RAM in KiB.
        pub ram_total_kb: u64,
        /// Available RAM in KiB.
        pub ram_avail_kb: u64,
        /// CPU supports AVX2.
        pub avx2: bool,
        /// CPU supports AVX-512F.
        pub avx512: bool,
        /// CPU supports NEON/ASIMD (kept for parity; MOULI targets x86_64).
        pub neon: bool,
        /// PCI vendor id of each GPU found.
        pub gpu_vendor: Vec<u32>,
    }

    /// Run plan derived from the hardware.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Plan {
        /// Worker threads for inference.
        pub threads: u32,
        /// Context size in tokens.
        pub ctx: u32,
        /// RAM budget for model weights in MiB.
        pub model_budget_mb: u64,
        /// Backend description.
        pub backend: &'static str,
    }

    const WS: [char; 6] = [' ', '\t', '\n', '\x0b', '\x0c', '\r'];

    /// Parses a leading unsigned decimal like C `strtoull` (whitespace, optional `+`, digits).
    fn parse_leading_u64(s: &str) -> u64 {
        let s = s.trim_start_matches(WS);
        let s = s.strip_prefix('+').unwrap_or(s);
        let mut v: u64 = 0;
        for c in s.chars() {
            match c.to_digit(10) {
                Some(d) => v = v.saturating_mul(10).saturating_add(u64::from(d)),
                None => break,
            }
        }
        v
    }

    /// Parses a leading hexadecimal number like C `strtoul(.., 16)` (optional `0x`).
    fn parse_leading_hex(s: &str) -> u32 {
        let s = s.trim_start_matches(WS);
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        let mut v: u32 = 0;
        for c in s.chars() {
            match c.to_digit(16) {
                Some(d) => v = v.saturating_mul(16).saturating_add(d),
                None => break,
            }
        }
        v
    }

    fn meminfo_field(text: &str, key: &str) -> u64 {
        for line in text.split('\n') {
            if let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) {
                return parse_leading_u64(rest);
            }
        }
        0
    }

    /// True when `word` occurs in `line` delimited by start/space/tab/colon before and end/space after.
    fn line_has_word(line: &str, word: &str) -> bool {
        let (l, w) = (line.as_bytes(), word.as_bytes());
        if w.is_empty() || l.len() < w.len() {
            return false;
        }
        (0..=l.len() - w.len()).any(|i| {
            &l[i..i + w.len()] == w
                && (i == 0 || matches!(l[i - 1], b' ' | b'\t' | b':'))
                && (i + w.len() == l.len() || l[i + w.len()] == b' ')
        })
    }

    impl Hw {
        /// Fills RAM fields from `/proc/meminfo` text.
        ///
        /// # Errors
        /// [`HwError::NoMemTotal`] when `MemTotal` is missing or zero (the fields are still set).
        pub fn parse_meminfo(&mut self, text: &str) -> Result<(), HwError> {
            self.ram_total_kb = meminfo_field(text, "MemTotal");
            self.ram_avail_kb = meminfo_field(text, "MemAvailable");
            if self.ram_avail_kb == 0 {
                self.ram_avail_kb = meminfo_field(text, "MemFree");
            }
            if self.ram_total_kb == 0 {
                Err(HwError::NoMemTotal)
            } else {
                Ok(())
            }
        }

        /// Fills CPU fields from `/proc/cpuinfo` text. Feature flags accumulate across calls.
        pub fn parse_cpuinfo(&mut self, text: &str) {
            let mut threads: u32 = 0;
            for line in text.split('\n') {
                if line.starts_with("processor") {
                    threads = threads.saturating_add(1);
                } else if line.starts_with("flags") {
                    self.avx2 |= line_has_word(line, "avx2");
                    self.avx512 |= line_has_word(line, "avx512f");
                } else if line.starts_with("Features") {
                    self.neon |= line_has_word(line, "asimd") | line_has_word(line, "neon");
                }
            }
            self.cpu_threads = threads.max(1);
        }

        /// Records a GPU by PCI vendor id.
        ///
        /// # Errors
        /// [`HwError::TooManyGpus`] when [`MAX_GPUS`] are already recorded.
        pub fn add_gpu(&mut self, vendor_id: u32) -> Result<(), HwError> {
            if self.gpu_vendor.len() >= MAX_GPUS {
                return Err(HwError::TooManyGpus);
            }
            self.gpu_vendor.push(vendor_id);
            Ok(())
        }

        /// Reads the real machine from `/proc` and `/sys`. Missing files leave fields at zero.
        #[must_use]
        pub fn probe() -> Hw {
            let mut hw = Hw::default();
            if let Some(t) = read_text("/proc/meminfo") {
                let _ = hw.parse_meminfo(&t);
            }
            if let Some(t) = read_text("/proc/cpuinfo") {
                hw.parse_cpuinfo(&t);
            }
            hw.cpu_threads = hw.cpu_threads.max(1);
            for i in 0..16 {
                let path = format!("/sys/class/drm/card{i}/device/vendor");
                if let Some(t) = read_text(&path) {
                    let _ = hw.add_gpu(parse_leading_hex(&t));
                }
            }
            hw
        }

        /// Derives the run plan.
        #[must_use]
        pub fn plan(&self) -> Plan {
            let avail_mb = self.ram_avail_kb / 1024;
            let threads = if self.cpu_threads > 1 {
                self.cpu_threads - 1
            } else {
                1
            };
            let ctx = if avail_mb >= 8192 {
                4096
            } else if avail_mb >= 2048 {
                2048
            } else {
                512
            };
            let gpu_capable = self
                .gpu_vendor
                .iter()
                .any(|&v| matches!(v, vendor::NVIDIA | vendor::AMD | vendor::INTEL));
            Plan {
                threads,
                ctx,
                model_budget_mb: avail_mb.saturating_mul(6) / 10,
                backend: if gpu_capable {
                    "gpu-capable (cpu until Vulkan/CUDA build)"
                } else {
                    "cpu"
                },
            }
        }

        /// Two-line description printed at boot.
        #[must_use]
        pub fn format(&self, plan: &Plan) -> String {
            let mut out = String::new();
            // Writing to a String cannot fail.
            let _ = write!(
                out,
                "MOULI hardware: {} CPU threads{}{}{} | RAM {}/{} MB available | GPUs: {}",
                self.cpu_threads,
                if self.avx2 { " avx2" } else { "" },
                if self.avx512 { " avx512" } else { "" },
                if self.neon { " neon" } else { "" },
                self.ram_avail_kb / 1024,
                self.ram_total_kb / 1024,
                self.gpu_vendor.len()
            );
            for (i, &v) in self.gpu_vendor.iter().enumerate() {
                let _ = write!(out, "{}{}", if i > 0 { "," } else { " (" }, vendor_name(v));
            }
            let _ = write!(
                out,
                "{}\nMOULI plan: threads={} ctx={} model-budget={}MB backend={}",
                if self.gpu_vendor.is_empty() { "" } else { ")" },
                plan.threads,
                plan.ctx,
                plan.model_budget_mb,
                plan.backend
            );
            out
        }
    }

    /// Human name of a PCI vendor id.
    #[must_use]
    pub fn vendor_name(v: u32) -> &'static str {
        match v {
            vendor::NVIDIA => "NVIDIA",
            vendor::AMD => "AMD",
            vendor::INTEL => "Intel",
            vendor::VIRTIO => "virtio",
            vendor::QEMU_STD => "QEMU-std",
            _ => "other",
        }
    }

    fn read_text(path: &str) -> Option<String> {
        let bytes = fs::read(path).ok()?;
        if bytes.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const MEMINFO: &str =
            "MemTotal:       16303428 kB\nMemFree:         1000000 kB\nMemAvailable:   12000000 kB\n";
        const CPU_X86: &str = "processor\t: 0\nflags\t\t: fpu sse2 avx avx2 fma\nprocessor\t: 1\nflags\t\t: fpu sse2 avx avx2 fma\nprocessor\t: 2\nflags\t: avx512f\nprocessor\t: 3\n";
        const CPU_ARM: &str = "processor\t: 0\nFeatures\t: fp asimd evtstrm\nprocessor\t: 1\n";

        #[test]
        fn meminfo_and_cpu_and_plan() {
            let mut hw = Hw::default();
            assert_eq!(hw.parse_meminfo(MEMINFO), Ok(()));
            assert_eq!((hw.ram_total_kb, hw.ram_avail_kb), (16_303_428, 12_000_000));
            hw.parse_cpuinfo(CPU_X86);
            assert!(hw.cpu_threads == 4 && hw.avx2 && hw.avx512 && !hw.neon);
            hw.add_gpu(vendor::NVIDIA).unwrap();
            let plan = hw.plan();
            assert_eq!(plan.threads, 3);
            assert_eq!(plan.ctx, 4096);
            assert_eq!(plan.model_budget_mb, 12_000_000 / 1024 * 6 / 10);
            assert!(plan.backend.contains("gpu"));
            let text = hw.format(&plan);
            assert!(text.contains("NVIDIA") && text.contains("threads=3"));
        }

        #[test]
        fn small_machine_and_edge_cases() {
            let mut hw = Hw::default();
            hw.parse_cpuinfo(CPU_ARM);
            assert!(hw.neon && hw.cpu_threads == 2);
            hw.ram_avail_kb = 1024 * 1024;
            let plan = hw.plan();
            assert!(plan.ctx == 512 && plan.threads == 1 && plan.backend == "cpu");

            let mut hw = Hw::default();
            hw.parse_cpuinfo("flags : avx2x\n");
            assert!(!hw.avx2);
            assert_eq!(hw.cpu_threads, 1);
        }

        #[test]
        fn missing_memtotal_is_an_error_and_free_is_fallback() {
            let mut hw = Hw::default();
            assert_eq!(
                hw.parse_meminfo("MemFree: 5 kB\n"),
                Err(HwError::NoMemTotal)
            );
            assert_eq!(hw.ram_avail_kb, 5);
            assert_eq!(hw.parse_meminfo("MemTotal: 10 kB\nMemFree: 7 kB\n"), Ok(()));
            assert_eq!(hw.ram_avail_kb, 7);
        }

        #[test]
        fn gpu_limit_and_names() {
            let mut hw = Hw::default();
            for _ in 0..MAX_GPUS {
                hw.add_gpu(vendor::INTEL).unwrap();
            }
            assert_eq!(hw.add_gpu(vendor::AMD), Err(HwError::TooManyGpus));
            assert_eq!(vendor_name(0x1af4), "virtio");
            assert_eq!(vendor_name(0xffff), "other");
        }

        #[test]
        fn number_parsers_match_c_semantics() {
            assert_eq!(parse_leading_u64("  42 kB"), 42);
            assert_eq!(parse_leading_u64("+7"), 7);
            assert_eq!(parse_leading_u64("abc"), 0);
            assert_eq!(parse_leading_hex("0x10de\n"), 0x10de);
            assert_eq!(parse_leading_hex("8086"), 0x8086);
        }

        #[test]
        fn probe_runs_on_this_machine() {
            let hw = Hw::probe();
            assert!(hw.cpu_threads >= 1);
        }
    }
}

pub mod prompt {
    //! Asking questions on the terminal: a line of input, and a line read with the echo turned off
    //! (for keys and passwords).

    use std::io::{self, BufRead, Write};

    /// Writes `prompt`, then reads one line (trimmed). `Ok(None)` at end of input.
    ///
    /// # Errors
    /// An I/O error from the reader or writer.
    pub fn ask<R: BufRead, W: Write>(
        input: &mut R,
        out: &mut W,
        prompt: &str,
    ) -> io::Result<Option<String>> {
        write!(out, "{prompt}")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim().to_string()))
    }

    /// Read a line with the terminal's echo turned off, so the key is not shown on screen.
    /// Like [`ask`], but with the terminal's echo turned off while the answer is typed.
    ///
    /// # Errors
    /// An I/O error from the reader or writer.
    pub fn ask_secret<R: BufRead, W: Write>(
        input: &mut R,
        out: &mut W,
        prompt: &str,
    ) -> io::Result<Option<String>> {
        #[cfg(target_os = "linux")]
        let _guard = EchoOff::new();
        let r = ask(input, out, prompt);
        writeln!(out)?;
        r
    }

    /// Turns terminal echo off for stdin until dropped. Silently does nothing when stdin is not a
    /// terminal (tests, pipes).
    #[cfg(target_os = "linux")]
    struct EchoOff(Option<nix::sys::termios::Termios>);

    #[cfg(target_os = "linux")]
    impl EchoOff {
        fn new() -> Self {
            use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
            let stdin = io::stdin();
            let Ok(orig) = tcgetattr(&stdin) else {
                return Self(None);
            };
            let mut quiet = orig.clone();
            quiet.local_flags.remove(LocalFlags::ECHO);
            quiet.local_flags.insert(LocalFlags::ECHONL);
            let _ = tcsetattr(&stdin, SetArg::TCSANOW, &quiet);
            Self(Some(orig))
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for EchoOff {
        fn drop(&mut self) {
            if let Some(orig) = &self.0 {
                let _ = nix::sys::termios::tcsetattr(
                    io::stdin(),
                    nix::sys::termios::SetArg::TCSANOW,
                    orig,
                );
            }
        }
    }
}

pub mod net {
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
                    "MOULI network: {iface} {addr}, dns {}",
                    if dns.is_empty() {
                        "none".to_string()
                    } else {
                        dns.join(" ")
                    }
                ),
                Self::NoLease(n) => format!(
                    "MOULI network: {} found but no cable or DHCP answer - offline",
                    n.join(", ")
                ),
                Self::NoAdapter => "MOULI network: no wired adapter found - offline".into(),
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
        let dns =
            parse_nameservers(&std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default());
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
            let root = std::env::temp_dir().join(format!("moulid-net-{}", std::process::id()));
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
                "MOULI network: eth0 10.0.2.15/24, dns 10.0.2.3"
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
}

pub mod wifi {
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
            if fs::read_to_string(&soft).is_ok_and(|s| s.trim() == "1")
                && fs::write(&soft, "0").is_ok()
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
                            "a Wi-Fi password has 8 to 63 characters (letters, digits, symbols)"
                                .into(),
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
        use crate::prompt::{ask, ask_secret};
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
                let Some(p) = ask_secret(input, out, "  Wi-Fi password (typing is hidden) > ")?
                else {
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
                        Ok(()) => {
                            writeln!(out, "  Saved: MOULI will join this network next time.")?
                        }
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
            let dir =
                std::env::temp_dir().join(format!("moulid-wifi-{tag}-{}", std::process::id()));
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
}

pub mod wifi_saved {
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
            let d = std::env::temp_dir()
                .join(format!("mouli-wifi-saved-{name}-{}", std::process::id()));
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
}

pub mod known_networks {
    //! Remembered Wi-Fi networks held entirely in memory.
    //!
    //! The caller owns all file or network I/O. This module validates entries,
    //! parses and renders a tab-separated text format, and redacts secrets from
    //! diagnostics. SSIDs are arbitrary bytes on the wire, so they are represented
    //! as raw bytes here; passphrases are printable ASCII only.

    use std::fmt;

    const MAX_ENTRIES: usize = 64;
    const MAX_SSID_BYTES: usize = 32;
    const MIN_PASSPHRASE_BYTES: usize = 8;
    const MAX_PASSPHRASE_BYTES: usize = 63;

    const ERR_WRONG_FIELD_COUNT: &str = "wrong field count";
    const ERR_BAD_HEX: &str = "bad hex";
    const ERR_SSID_LENGTH: &str = "SSID must be 1 to 32 bytes";
    const ERR_PASSPHRASE_LENGTH: &str = "passphrase must be 8 to 63 printable ASCII characters";
    const ERR_PASSPHRASE_PRINTABLE: &str = "passphrase must be printable ASCII";
    const ERR_LAST_USED: &str = "last_used must be an unsigned 64-bit decimal integer";

    /// A remembered network.
    ///
    /// `ssid` is 1..=32 arbitrary bytes. `passphrase` is `None` for an open
    /// network; when present it is 8..=63 printable ASCII characters. `last_used`
    /// is an opaque counter where larger means more recently used.
    pub struct Entry {
        /// Network SSID as raw bytes, 1..=32 bytes long.
        pub ssid: Vec<u8>,
        /// Passphrase, or `None` for an open network.
        pub passphrase: Option<Secret>,
        /// Opaque last-used counter; larger means more recently used.
        pub last_used: u64,
    }

    impl fmt::Debug for Entry {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("Entry")
                .field("ssid_hex", &encode_hex(&self.ssid))
                .field("passphrase", &self.passphrase)
                .field("last_used", &self.last_used)
                .finish()
        }
    }

    /// A validated passphrase that never exposes itself through `Debug`.
    ///
    /// The inner string is overwritten with zero bytes when dropped. There is no
    /// `Display` implementation; callers must opt in to exposure explicitly.
    pub struct Secret(String);

    impl Secret {
        /// Creates a secret from `text` after validating printable-ASCII bounds.
        ///
        /// # Errors
        /// [`BookError::InvalidPassphrase`] when the length or characters are out of range.
        pub fn new(text: String) -> Result<Secret, BookError> {
            let bytes = text.into_bytes();
            validate_passphrase_bytes(&bytes).map_err(|_| BookError::InvalidPassphrase)?;
            let text = String::from_utf8(bytes).map_err(|_| BookError::InvalidPassphrase)?;
            Ok(Secret(text))
        }

        /// Borrows the passphrase for explicit use by the caller.
        #[must_use]
        pub fn expose(&self) -> &str {
            &self.0
        }
    }

    impl Drop for Secret {
        fn drop(&mut self) {
            let mut bytes = std::mem::take(&mut self.0).into_bytes();
            bytes.fill(0);
        }
    }

    impl fmt::Debug for Secret {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("Secret(***)")
        }
    }

    /// Errors produced by validating an entry or secret.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BookError {
        /// The SSID was empty or longer than 32 bytes.
        InvalidSsid,
        /// The passphrase was outside 8..=63 printable ASCII bytes.
        InvalidPassphrase,
    }

    impl fmt::Display for BookError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                BookError::InvalidSsid => formatter.write_str("SSID must be 1 to 32 bytes"),
                BookError::InvalidPassphrase => {
                    formatter.write_str("passphrase must be 8 to 63 printable ASCII characters")
                }
            }
        }
    }

    impl std::error::Error for BookError {}

    /// A parse failure on a single input line.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct LineError {
        /// One-based line number in the parsed text.
        pub line: usize,
        /// Static description of why the line was skipped.
        pub message: &'static str,
    }

    impl fmt::Display for LineError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "line {}: {}", self.line, self.message)
        }
    }

    impl std::error::Error for LineError {}

    /// A bounded set of remembered networks.
    pub struct Book {
        entries: Vec<Entry>,
    }

    impl Book {
        /// Creates an empty book.
        #[must_use]
        pub fn new() -> Self {
            Self {
                entries: Vec::new(),
            }
        }

        /// Returns the number of stored entries.
        #[must_use]
        pub fn len(&self) -> usize {
            self.entries.len()
        }

        /// Returns true when no entries are stored.
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.entries.is_empty()
        }

        /// Returns all entries in storage order.
        #[must_use]
        pub fn entries(&self) -> &[Entry] {
            &self.entries
        }

        /// Validates and inserts or replaces an entry by SSID.
        ///
        /// If the book is already at capacity after insertion, the entry with the
        /// smallest `last_used` is evicted.
        ///
        /// # Errors
        /// [`BookError`] when the SSID or passphrase is invalid.
        pub fn upsert(&mut self, entry: Entry) -> Result<(), BookError> {
            validate_entry(&entry)?;
            self.insert_entry(entry);
            Ok(())
        }

        /// Removes the entry with `ssid`, returning true if one was present.
        pub fn forget(&mut self, ssid: &[u8]) -> bool {
            if let Some(position) = self
                .entries
                .iter()
                .position(|entry| entry.ssid.as_slice() == ssid)
            {
                let _ = self.entries.remove(position);
                true
            } else {
                false
            }
        }

        /// Returns the best known network visible right now.
        ///
        /// The visible list contains `(ssid, signal_dbm)` pairs. Stronger signal
        /// wins; equal signal is broken by the remembered entry's higher
        /// `last_used`. Unknown SSIDs are ignored.
        #[must_use]
        pub fn best_match(&self, visible: &[(Vec<u8>, i32)]) -> Option<&Entry> {
            let mut best: Option<&Entry> = None;
            let mut best_signal = 0_i32;

            for visible_network in visible {
                let (ssid, signal) = visible_network;
                let Some(entry) = self
                    .entries
                    .iter()
                    .find(|candidate| candidate.ssid.as_slice() == ssid.as_slice())
                else {
                    continue;
                };

                let replace = match best {
                    None => true,
                    Some(current) => {
                        *signal > best_signal
                            || (*signal == best_signal && entry.last_used > current.last_used)
                    }
                };

                if replace {
                    best = Some(entry);
                    best_signal = *signal;
                }
            }

            best
        }

        /// Parses remembered networks, one per line.
        ///
        /// Each line is `hex(ssid) TAB hex(passphrase) TAB last_used`, with `-` as
        /// the passphrase field for an open network. Hex is lowercase on output and
        /// either case is accepted on input. Malformed lines are skipped and
        /// reported with one-based line numbers. A later valid line for the same
        /// SSID replaces the earlier one.
        #[must_use]
        pub fn parse(text: &str) -> (Book, Vec<LineError>) {
            let mut book = Book::new();
            let mut errors = Vec::new();

            for (index, line) in text.lines().enumerate() {
                let line_number = index + 1;
                match entry_from_line(line) {
                    Ok(entry) => book.insert_entry(entry),
                    Err(message) => errors.push(LineError {
                        line: line_number,
                        message,
                    }),
                }
            }

            (book, errors)
        }

        /// Renders the book, one network per line, sorted by `last_used` descending.
        ///
        /// Output ends with a trailing newline when at least one entry is present.
        #[must_use]
        pub fn render(&self) -> String {
            let mut entries: Vec<&Entry> = self.entries.iter().collect();
            entries.sort_by(|left, right| {
                right
                    .last_used
                    .cmp(&left.last_used)
                    .then_with(|| left.ssid.cmp(&right.ssid))
            });

            let mut output = String::new();
            for entry in entries {
                output.push_str(&encode_hex(&entry.ssid));
                output.push('\t');
                match &entry.passphrase {
                    Some(secret) => output.push_str(&encode_hex(secret.expose().as_bytes())),
                    None => output.push('-'),
                }
                output.push('\t');
                output.push_str(&entry.last_used.to_string());
                output.push('\n');
            }
            output
        }

        fn insert_entry(&mut self, entry: Entry) {
            if let Some(position) = self
                .entries
                .iter()
                .position(|existing| existing.ssid == entry.ssid)
            {
                if let Some(slot) = self.entries.get_mut(position) {
                    *slot = entry;
                }
                return;
            }

            self.entries.push(entry);
            if self.entries.len() > MAX_ENTRIES {
                let lowest = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, candidate)| candidate.last_used)
                    .map(|(position, _)| position);
                if let Some(position) = lowest {
                    let _ = self.entries.remove(position);
                }
            }
        }
    }

    impl Default for Book {
        fn default() -> Self {
            Self::new()
        }
    }

    impl fmt::Debug for Book {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("Book")
                .field("len", &self.entries.len())
                .finish()
        }
    }

    fn validate_entry(entry: &Entry) -> Result<(), BookError> {
        if entry.ssid.is_empty() || entry.ssid.len() > MAX_SSID_BYTES {
            return Err(BookError::InvalidSsid);
        }
        if let Some(secret) = &entry.passphrase {
            validate_passphrase_bytes(secret.expose().as_bytes())
                .map_err(|_| BookError::InvalidPassphrase)?;
        }
        Ok(())
    }

    fn validate_passphrase_bytes(bytes: &[u8]) -> Result<(), &'static str> {
        if !(MIN_PASSPHRASE_BYTES..=MAX_PASSPHRASE_BYTES).contains(&bytes.len()) {
            return Err(ERR_PASSPHRASE_LENGTH);
        }
        if bytes.iter().any(|byte| !(0x20_u8..=0x7e_u8).contains(byte)) {
            return Err(ERR_PASSPHRASE_PRINTABLE);
        }
        Ok(())
    }

    fn entry_from_line(line: &str) -> Result<Entry, &'static str> {
        let fields: Vec<&str> = line.split('\t').collect();
        let [ssid_field, passphrase_field, last_used_field] = fields.as_slice() else {
            return Err(ERR_WRONG_FIELD_COUNT);
        };

        let ssid = decode_hex(ssid_field)?;
        if ssid.is_empty() || ssid.len() > MAX_SSID_BYTES {
            return Err(ERR_SSID_LENGTH);
        }

        let passphrase = if *passphrase_field == "-" {
            None
        } else {
            let bytes = decode_hex(passphrase_field)?;
            let text = secret_text_from_bytes(bytes)?;
            Some(Secret(text))
        };

        let last_used = parse_u64(last_used_field).ok_or(ERR_LAST_USED)?;
        Ok(Entry {
            ssid,
            passphrase,
            last_used,
        })
    }

    fn secret_text_from_bytes(bytes: Vec<u8>) -> Result<String, &'static str> {
        validate_passphrase_bytes(&bytes)?;
        String::from_utf8(bytes).map_err(|_| ERR_PASSPHRASE_PRINTABLE)
    }

    fn decode_hex(text: &str) -> Result<Vec<u8>, &'static str> {
        let (chunks, rest) = text.as_bytes().as_chunks::<2>();
        if !rest.is_empty() {
            return Err(ERR_BAD_HEX);
        }

        let mut bytes = Vec::with_capacity(text.len() / 2);
        for [high, low] in chunks {
            let high = hex_value(*high).ok_or(ERR_BAD_HEX)?;
            let low = hex_value(*low).ok_or(ERR_BAD_HEX)?;
            bytes.push((high << 4) | low);
        }
        Ok(bytes)
    }

    fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    fn encode_hex(bytes: &[u8]) -> String {
        fn hex_char(nibble: u8) -> char {
            match nibble {
                0..=9 => char::from(b'0' + nibble),
                _ => char::from(b'a' + nibble - 10),
            }
        }

        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(hex_char(byte >> 4));
            output.push(hex_char(byte & 0x0f));
        }
        output
    }

    fn parse_u64(text: &str) -> Option<u64> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<u64>().ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn open_entry(ssid: &[u8], last_used: u64) -> Entry {
            Entry {
                ssid: ssid.to_vec(),
                passphrase: None,
                last_used,
            }
        }

        fn secured_entry(ssid: &[u8], passphrase: &str, last_used: u64) -> Entry {
            Entry {
                ssid: ssid.to_vec(),
                passphrase: Some(Secret::new(passphrase.to_owned()).unwrap()),
                last_used,
            }
        }

        fn one_byte_entry(ssid: u8, passphrase: Option<&str>, last_used: u64) -> Entry {
            Entry {
                ssid: vec![ssid],
                passphrase: passphrase.map(|text| Secret::new(text.to_owned()).unwrap()),
                last_used,
            }
        }

        fn assert_parse_error(text: &str, line: usize, message: &'static str) {
            let (book, errors) = Book::parse(text);
            assert_eq!(book.len(), 0);
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].line, line);
            assert_eq!(errors[0].message, message);
        }

        fn assert_second_line_error(text: &str, message: &'static str) {
            let (book, errors) = Book::parse(text);
            assert_eq!(book.len(), 1);
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].line, 2);
            assert_eq!(errors[0].message, message);
        }

        #[test]
        fn round_trip_preserves_entries() {
            let mut book = Book::new();
            book.upsert(secured_entry(&[0xde, 0xad, 0xbe, 0xef], "p@ss w0rd", 7))
                .unwrap();
            book.upsert(open_entry(b"open", 9)).unwrap();

            let text = book.render();
            let (parsed, errors) = Book::parse(&text);

            assert!(errors.is_empty());
            assert_eq!(parsed.render(), text);
            assert_eq!(parsed.entries().len(), 2);
        }

        #[test]
        fn render_sorts_by_last_used_descending_and_ends_with_newline() {
            let mut book = Book::new();
            book.upsert(open_entry(b"old", 1)).unwrap();
            book.upsert(open_entry(b"new", 3)).unwrap();
            book.upsert(open_entry(b"mid", 2)).unwrap();

            let text = book.render();
            assert!(text.ends_with('\n'));
            let fields: Vec<&str> = text.lines().collect();
            assert_eq!(fields.len(), 3);
            assert_eq!(fields[0].rsplit('\t').next(), Some("3"));
            assert_eq!(fields[1].rsplit('\t').next(), Some("2"));
            assert_eq!(fields[2].rsplit('\t').next(), Some("1"));
        }

        #[test]
        fn render_of_empty_book_is_empty() {
            assert_eq!(Book::new().render(), "");
        }

        #[test]
        fn parse_accepts_uppercase_hex_and_open_networks() {
            let (book, errors) =
                Book::parse("AABB\t-\t18446744073709551615\n00\t2D2D2D2D2D2D2D2D\t1\n");
            assert!(errors.is_empty());
            assert_eq!(book.len(), 2);
            assert_eq!(book.entries()[0].ssid, vec![0xaa, 0xbb]);
            assert_eq!(book.entries()[0].last_used, u64::MAX);
        }

        #[test]
        fn parse_wrong_field_count_reports_line() {
            assert_parse_error("\n", 1, ERR_WRONG_FIELD_COUNT);
            assert_parse_error("00\t-\t1\textra\n", 1, ERR_WRONG_FIELD_COUNT);
            assert_parse_error("00\t1\n", 1, ERR_WRONG_FIELD_COUNT);
            assert_second_line_error("aa\t-\t1\nextra\t-\t2\textra\n", ERR_WRONG_FIELD_COUNT);
        }

        #[test]
        fn parse_reports_bad_hex_and_line_numbers() {
            assert_parse_error("gg\t-\t1\n", 1, ERR_BAD_HEX);
            assert_parse_error("0\t-\t1\n", 1, ERR_BAD_HEX);
            assert_parse_error("00\tgg\t1\n", 1, ERR_BAD_HEX);
            assert_parse_error("00\t0\t1\n", 1, ERR_BAD_HEX);
            assert_second_line_error("aa\t-\t1\nzz\t-\t2\n", ERR_BAD_HEX);
        }

        #[test]
        fn parse_reports_invalid_ssid_lengths() {
            assert_parse_error("\t-\t1\n", 1, ERR_SSID_LENGTH);
            assert_parse_error(&format!("{}\t-\t1\n", "42".repeat(33)), 1, ERR_SSID_LENGTH);
        }

        #[test]
        fn parse_reports_invalid_passphrases() {
            assert_parse_error("00\t61616161616161\t1\n", 1, ERR_PASSPHRASE_LENGTH);
            assert_parse_error(
                &format!("00\t{}\t1\n", "61".repeat(64)),
                1,
                ERR_PASSPHRASE_LENGTH,
            );
            assert_parse_error("00\t616161616161617f\t1\n", 1, ERR_PASSPHRASE_PRINTABLE);
            assert_parse_error("00\t0101616161616161\t1\n", 1, ERR_PASSPHRASE_PRINTABLE);
        }

        #[test]
        fn parse_reports_bad_last_used_numbers() {
            assert_parse_error("00\t-\t\n", 1, ERR_LAST_USED);
            assert_parse_error("00\t-\t+1\n", 1, ERR_LAST_USED);
            assert_parse_error("00\t-\t-1\n", 1, ERR_LAST_USED);
            assert_parse_error("00\t-\t 1\n", 1, ERR_LAST_USED);
            assert_parse_error("00\t-\tone\n", 1, ERR_LAST_USED);
            assert_parse_error("00\t-\t18446744073709551616\n", 1, ERR_LAST_USED);
        }

        #[test]
        fn parse_skips_bad_lines_and_keeps_good_ones() {
            let text = "00\t-\t1\nbad\t-\tnope\nff\t6161616161616161\t3\n";
            let (book, errors) = Book::parse(text);
            assert_eq!(book.len(), 2);
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].line, 2);
            assert_eq!(errors[0].message, ERR_BAD_HEX);
        }

        #[test]
        fn parse_duplicate_ssid_keeps_later_entry() {
            let text = "6161\t-\t1\n6161\t6262626262626262\t2\n";
            let (book, errors) = Book::parse(text);
            assert!(errors.is_empty());
            assert_eq!(book.len(), 1);
            assert_eq!(book.entries()[0].last_used, 2);
            assert_eq!(
                book.entries()[0].passphrase.as_ref().map(Secret::expose),
                Some("bbbbbbbb")
            );
        }

        #[test]
        fn upsert_validates_and_replaces_same_ssid() {
            let mut book = Book::new();
            book.upsert(open_entry(b"net", 1)).unwrap();
            book.upsert(secured_entry(b"net", "eightchar", 2)).unwrap();
            assert_eq!(book.len(), 1);
            assert_eq!(book.entries()[0].last_used, 2);

            assert_eq!(book.upsert(open_entry(&[], 3)), Err(BookError::InvalidSsid));
            assert_eq!(
                book.upsert(open_entry(&[0_u8; 33], 3)),
                Err(BookError::InvalidSsid)
            );
        }

        #[test]
        fn passphrase_boundaries_and_printable_ascii() {
            assert!(Secret::new("1234567".to_owned()).is_err());
            assert!(Secret::new("12345678".to_owned()).is_ok());
            assert!(Secret::new("a".repeat(63)).is_ok());
            assert!(Secret::new("a".repeat(64)).is_err());
            assert!(Secret::new("        ".to_owned()).is_ok());
            assert!(Secret::new("1234567\x7f".to_owned()).is_err());
            assert!(Secret::new("1234567\n".to_owned()).is_err());
            assert!(Secret::new("hélloooo".to_owned()).is_err());
        }

        #[test]
        fn ssid_boundaries_are_enforced() {
            assert!(Book::new().upsert(open_entry(&[], 1)).is_err());
            assert!(Book::new().upsert(open_entry(b"a", 1)).is_ok());
            assert!(Book::new().upsert(open_entry(&[b'a'; 32], 1)).is_ok());
            assert!(Book::new().upsert(open_entry(&[b'a'; 33], 1)).is_err());
        }

        #[test]
        fn eviction_at_65_entries_removes_smallest_last_used() {
            let mut book = Book::new();
            for value in 1..=64_u8 {
                book.upsert(open_entry(&[value], u64::from(value))).unwrap();
            }

            book.upsert(open_entry(&[65], 65)).unwrap();

            assert_eq!(book.len(), 64);
            assert!(!book
                .entries()
                .iter()
                .any(|entry| entry.ssid.as_slice() == [1]));
            assert!(book
                .entries()
                .iter()
                .any(|entry| entry.ssid.as_slice() == [65]));
            assert!(!book.forget(&[1]));
            assert!(book.forget(&[2]));
        }

        #[test]
        fn best_match_prefers_strongest_signal() {
            let mut book = Book::new();
            book.upsert(one_byte_entry(b'a', Some("aaaaaaaa"), 1))
                .unwrap();
            book.upsert(one_byte_entry(b'b', Some("bbbbbbbb"), 100))
                .unwrap();

            let visible = vec![(vec![b'a'], -30), (vec![b'b'], -90)];
            let best = book.best_match(&visible).map(|entry| entry.ssid.as_slice());
            assert_eq!(best, Some(&b"a"[..]));
        }

        #[test]
        fn best_match_breaks_signal_ties_by_higher_last_used() {
            let mut book = Book::new();
            book.upsert(one_byte_entry(b'a', Some("aaaaaaaa"), 5))
                .unwrap();
            book.upsert(one_byte_entry(b'b', Some("bbbbbbbb"), 9))
                .unwrap();

            let visible = vec![(vec![b'a'], -50), (vec![b'b'], -50)];
            let best = book.best_match(&visible).map(|entry| entry.ssid.as_slice());
            assert_eq!(best, Some(&b"b"[..]));
        }

        #[test]
        fn best_match_ignores_unknown_networks_and_handles_empty() {
            let mut book = Book::new();
            book.upsert(one_byte_entry(b'k', None, 10)).unwrap();

            assert!(book.best_match(&[]).is_none());
            assert!(book.best_match(&[(vec![b'u'], -10)]).is_none());

            let visible = vec![(vec![b'u'], -10), (vec![b'k'], -80)];
            assert_eq!(
                book.best_match(&visible).map(|entry| entry.ssid.as_slice()),
                Some(&b"k"[..])
            );
        }

        #[test]
        fn debug_output_never_reveals_passphrase() {
            let passphrase = "correct horse battery staple";
            let entry = Entry {
                ssid: vec![b'a'],
                passphrase: Some(Secret::new(passphrase.to_owned()).unwrap()),
                last_used: 42,
            };

            let entry_debug = format!("{entry:?}");
            assert!(!entry_debug.contains(passphrase));
            assert!(entry_debug.contains("Secret(***)"));

            let mut book = Book::new();
            book.upsert(entry).unwrap();
            let book_debug = format!("{book:?}");
            assert!(!book_debug.contains(passphrase));

            let secret_debug = format!("{:?}", Secret::new(passphrase.to_owned()).unwrap());
            assert_eq!(secret_debug, "Secret(***)");
        }

        #[test]
        fn non_utf8_ssid_round_trips() {
            let ssid = vec![0xff, 0x00, 0x7f, 0x41];
            let mut book = Book::new();
            book.upsert(Entry {
                ssid: ssid.clone(),
                passphrase: Some(Secret::new("p@ss w0rd".to_owned()).unwrap()),
                last_used: 123,
            })
            .unwrap();

            let rendered = book.render();
            let (parsed, errors) = Book::parse(&rendered);

            assert!(errors.is_empty());
            assert_eq!(parsed.render(), rendered);
            let parsed_entry = parsed
                .entries()
                .iter()
                .find(|entry| entry.ssid == ssid)
                .unwrap();
            assert_eq!(
                parsed_entry.passphrase.as_ref().map(Secret::expose),
                Some("p@ss w0rd")
            );
            assert_eq!(parsed_entry.last_used, 123);
        }

        #[test]
        fn forget_returns_false_for_missing_ssid() {
            let mut book = Book::new();
            assert!(!book.forget(b"m"));
            book.upsert(open_entry(b"m", 1)).unwrap();
            assert!(book.forget(b"m"));
            assert!(book.is_empty());
        }
    }
}

pub mod bluetooth {
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
            std::fs::remove_dir(state_dir).map_err(|_| {
                format!("{} is not empty; keys stay in memory", state_dir.display())
            })?;
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
    pub fn start_services<T: Tools>(
        t: &mut T,
        rfkill_dir: &Path,
        sys: &Path,
    ) -> Result<(), String> {
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
        use crate::prompt::ask;
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
}

pub mod civil_time {
    //! Date/time helpers for UTC proleptic Gregorian civil time.
    //!
    //! This module intentionally does no I/O and never reads the clock. It provides
    //! a small `DateTime` value, conversions to and from Unix seconds, strict
    //! RFC 3339 / RTC file parsing, X.509 time decoding, and simple formatting.

    /// A UTC civil date and time in the proleptic Gregorian calendar.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DateTime {
        /// Year in the proleptic Gregorian calendar.
        pub year: i32,
        /// Month of year, 1 through 12.
        pub month: u8,
        /// Day of month, 1 through the length of the month.
        pub day: u8,
        /// Hour of day, 0 through 23.
        pub hour: u8,
        /// Minute of hour, 0 through 59.
        pub minute: u8,
        /// Second of minute, 0 through 59; leap seconds are not supported.
        pub second: u8,
    }

    /// Errors returned by parsing and decoding helpers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TimeError {
        /// The input has the wrong length for the expected format.
        InvalidLength,
        /// The input contains characters outside the grammar at a position.
        InvalidFormat,
        /// The input is structurally well formed but not a valid date/time.
        InvalidValue,
        /// The DER tag is not UTCTime or GeneralizedTime.
        UnsupportedTag,
        /// The time uses a form other than uppercase `Z` Zulu form.
        UnsupportedForm,
        /// The value cannot be represented on this platform or in Unix seconds.
        OutOfRange,
    }

    impl std::fmt::Display for TimeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::InvalidLength => write!(f, "invalid length"),
                Self::InvalidFormat => write!(f, "invalid format"),
                Self::InvalidValue => write!(f, "invalid date or time"),
                Self::UnsupportedTag => write!(f, "unsupported DER tag"),
                Self::UnsupportedForm => write!(f, "unsupported time form"),
                Self::OutOfRange => write!(f, "value out of range"),
            }
        }
    }

    impl std::error::Error for TimeError {}

    const PLAUSIBLE_START: i64 = 1_735_689_600;
    const PLAUSIBLE_END: i64 = 4_102_444_800;

    impl DateTime {
        /// Creates a `DateTime` from Unix seconds.
        ///
        /// Returns `None` outside the inclusive year range 1970 through 9999.
        #[must_use]
        pub fn from_unix(secs: i64) -> Option<Self> {
            let days = secs.div_euclid(86_400);
            let rem = secs.rem_euclid(86_400);
            let (year, month, day) = civil_from_days(days)?;
            if !(1970..=9999).contains(&year) {
                return None;
            }
            let hour = u8::try_from(rem / 3_600).ok()?;
            let minute = u8::try_from((rem / 60) % 60).ok()?;
            let second = u8::try_from(rem % 60).ok()?;
            Some(Self {
                year: i32::try_from(year).ok()?,
                month,
                day,
                hour,
                minute,
                second,
            })
        }

        /// Converts this value to Unix seconds.
        ///
        /// Returns `None` unless the value is valid and inside the inclusive year
        /// range 1970 through 9999.
        #[must_use]
        pub fn to_unix(&self) -> Option<i64> {
            if !self.is_valid() || !(1970..=9999).contains(&self.year) {
                return None;
            }
            unix_seconds(self)
        }

        /// Checks month lengths, leap years, and time-of-day ranges.
        #[must_use]
        pub fn is_valid(&self) -> bool {
            let month_length = match self.month {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 => {
                    if is_leap_year(self.year) {
                        29
                    } else {
                        28
                    }
                }
                _ => return false,
            };
            (1..=month_length).contains(&self.day)
                && self.hour < 24
                && self.minute < 60
                && self.second <= 59
        }

        /// Parses exactly `YYYY-MM-DDTHH:MM:SSZ`; `t` and `z` may be lowercase.
        ///
        /// Offsets, fractional seconds, missing fields, and any other trailing or
        /// leading data are rejected.
        ///
        /// # Errors
        /// A [`TimeError`] describing why the text is not an exact UTC timestamp.
        pub fn parse_rfc3339_utc(s: &str) -> Result<Self, TimeError> {
            let bytes = s.as_bytes();
            if bytes.len() != 20 {
                return Err(TimeError::InvalidLength);
            }
            if !matches!(bytes.get(4), Some(&b'-'))
                || !matches!(bytes.get(7), Some(&b'-'))
                || !matches!(bytes.get(13), Some(&b':'))
                || !matches!(bytes.get(16), Some(&b':'))
            {
                return Err(TimeError::InvalidFormat);
            }
            let marker = *bytes.get(10).ok_or(TimeError::InvalidLength)?;
            if marker != b'T' && marker != b't' {
                return Err(TimeError::InvalidFormat);
            }
            let zed = *bytes.get(19).ok_or(TimeError::InvalidLength)?;
            if zed != b'Z' && zed != b'z' {
                return Err(TimeError::UnsupportedForm);
            }
            let (year, month, day) = parse_date(bytes.get(0..10).ok_or(TimeError::InvalidLength)?)
                .ok_or(TimeError::InvalidFormat)?;
            let (hour, minute, second) =
                parse_time(bytes.get(11..19).ok_or(TimeError::InvalidLength)?)
                    .ok_or(TimeError::InvalidFormat)?;
            let value = Self {
                year,
                month,
                day,
                hour,
                minute,
                second,
            };
            if value.is_valid() {
                Ok(value)
            } else {
                Err(TimeError::InvalidValue)
            }
        }

        /// Parses the separate contents of RTC `date` and `time` files.
        ///
        /// Trailing carriage returns and line feeds are trimmed from both inputs.
        /// The date must be `YYYY-MM-DD` and the time must be `HH:MM:SS`.
        ///
        /// # Errors
        /// A [`TimeError`] when either file's text is malformed or not a real time.
        pub fn from_rtc_files(date: &str, time: &str) -> Result<Self, TimeError> {
            let date = date.trim_end_matches(['\n', '\r']);
            let time = time.trim_end_matches(['\n', '\r']);
            if date.len() != 10 || time.len() != 8 {
                return Err(TimeError::InvalidLength);
            }
            let (year, month, day) = parse_date(date.as_bytes()).ok_or(TimeError::InvalidFormat)?;
            let (hour, minute, second) =
                parse_time(time.as_bytes()).ok_or(TimeError::InvalidFormat)?;
            let value = Self {
                year,
                month,
                day,
                hour,
                minute,
                second,
            };
            if value.is_valid() {
                Ok(value)
            } else {
                Err(TimeError::InvalidValue)
            }
        }

        /// Formats as `YYYY-MM-DDTHH:MM:SSZ`.
        #[must_use]
        pub fn format_rfc3339(&self) -> String {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                self.year, self.month, self.day, self.hour, self.minute, self.second
            )
        }

        /// Formats as `DD Mon YYYY HH:MM:SS UTC`, for example `24 Nov 2026 12:34:56 UTC`.
        #[must_use]
        pub fn format_human(&self) -> String {
            format!(
                "{:02} {} {:04} {:02}:{:02}:{:02} UTC",
                self.day,
                month_abbrev(self.month),
                self.year,
                self.hour,
                self.minute,
                self.second
            )
        }
    }

    /// Decodes an X.509 UTCTime (`0x17`) or GeneralizedTime (`0x18`) value.
    ///
    /// Only the uppercase `Z` form is accepted. UTCTime years below 50 are decoded
    /// as 20YY and years 50 through 99 are decoded as 19YY.
    ///
    /// # Errors
    /// A [`TimeError`] for an unknown tag, wrong length or form, bad digits, or an
    /// impossible date.
    pub fn der_time_to_unix(tag: u8, content: &[u8]) -> Result<i64, TimeError> {
        let year_digits = match tag {
            0x17 => 2_usize,
            0x18 => 4_usize,
            _ => return Err(TimeError::UnsupportedTag),
        };
        let expected_len = year_digits + 11;
        if content.len() != expected_len {
            return Err(TimeError::InvalidLength);
        }
        if *content
            .get(expected_len - 1)
            .ok_or(TimeError::InvalidLength)?
            != b'Z'
        {
            return Err(TimeError::UnsupportedForm);
        }
        let year_raw = parse_digits(content, 0, year_digits).ok_or(TimeError::InvalidFormat)?;
        let year_i32 = i32::try_from(year_raw).map_err(|_| TimeError::OutOfRange)?;
        let year = if year_digits == 2 {
            if year_i32 >= 50 {
                1900 + year_i32
            } else {
                2000 + year_i32
            }
        } else {
            year_i32
        };
        let field = |offset: usize| -> Result<u8, TimeError> {
            let value =
                parse_digits(content, year_digits + offset, 2).ok_or(TimeError::InvalidFormat)?;
            u8::try_from(value).map_err(|_| TimeError::OutOfRange)
        };
        let value = DateTime {
            year,
            month: field(0)?,
            day: field(2)?,
            hour: field(4)?,
            minute: field(6)?,
            second: field(8)?,
        };
        if !value.is_valid() {
            return Err(TimeError::InvalidValue);
        }
        unix_seconds(&value).ok_or(TimeError::OutOfRange)
    }

    /// Returns true only after 2025-01-01T00:00:00Z and before 2100-01-01T00:00:00Z.
    #[must_use]
    pub fn plausible(unix: i64) -> bool {
        unix > PLAUSIBLE_START && unix < PLAUSIBLE_END
    }

    fn is_leap_year(year: i32) -> bool {
        (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
    }

    fn unix_seconds(dt: &DateTime) -> Option<i64> {
        let days = days_from_civil(i64::from(dt.year), i64::from(dt.month), i64::from(dt.day));
        let seconds = i64::from(dt.hour) * 3_600 + i64::from(dt.minute) * 60 + i64::from(dt.second);
        days.checked_mul(86_400)?.checked_add(seconds)
    }

    fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
        let adjusted_year = if month <= 2 { year - 1 } else { year };
        let era = (if adjusted_year >= 0 {
            adjusted_year
        } else {
            adjusted_year - 399
        }) / 400;
        let year_of_era = adjusted_year - era * 400;
        let month_prime = if month > 2 { month - 3 } else { month + 9 };
        let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    fn civil_from_days(z: i64) -> Option<(i64, u8, u8)> {
        let shifted = z.checked_add(719_468)?;
        let era = (if shifted >= 0 {
            shifted
        } else {
            shifted.checked_sub(146_096)?
        }) / 146_097;
        let day_of_era = shifted - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prime = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
        let month = if month_prime < 10 {
            month_prime + 3
        } else {
            month_prime - 9
        };
        let output_year = year + i64::from(month <= 2);
        Some((
            output_year,
            u8::try_from(month).ok()?,
            u8::try_from(day).ok()?,
        ))
    }

    fn parse_digits(bytes: &[u8], start: usize, len: usize) -> Option<u32> {
        let mut value = 0_u32;
        for offset in 0..len {
            let index = start.checked_add(offset)?;
            let byte = *bytes.get(index)?;
            if !byte.is_ascii_digit() {
                return None;
            }
            value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
        }
        Some(value)
    }

    fn parse_date(bytes: &[u8]) -> Option<(i32, u8, u8)> {
        if bytes.len() != 10 {
            return None;
        }
        if !matches!(bytes.get(4), Some(&b'-')) || !matches!(bytes.get(7), Some(&b'-')) {
            return None;
        }
        let year = i32::try_from(parse_digits(bytes, 0, 4)?).ok()?;
        let month = u8::try_from(parse_digits(bytes, 5, 2)?).ok()?;
        let day = u8::try_from(parse_digits(bytes, 8, 2)?).ok()?;
        Some((year, month, day))
    }

    fn parse_time(bytes: &[u8]) -> Option<(u8, u8, u8)> {
        if bytes.len() != 8 {
            return None;
        }
        if !matches!(bytes.get(2), Some(&b':')) || !matches!(bytes.get(5), Some(&b':')) {
            return None;
        }
        let hour = u8::try_from(parse_digits(bytes, 0, 2)?).ok()?;
        let minute = u8::try_from(parse_digits(bytes, 3, 2)?).ok()?;
        let second = u8::try_from(parse_digits(bytes, 6, 2)?).ok()?;
        Some((hour, minute, second))
    }

    fn month_abbrev(month: u8) -> &'static str {
        match month {
            1 => "Jan",
            2 => "Feb",
            3 => "Mar",
            4 => "Apr",
            5 => "May",
            6 => "Jun",
            7 => "Jul",
            8 => "Aug",
            9 => "Sep",
            10 => "Oct",
            11 => "Nov",
            12 => "Dec",
            _ => "???",
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn unix_epoch_boundaries() {
            let epoch = DateTime {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            };
            assert_eq!(DateTime::from_unix(0), Some(epoch));
            assert_eq!(epoch.to_unix(), Some(0));
            assert_eq!(DateTime::from_unix(-1), None);
            assert_eq!(
                DateTime {
                    year: 1969,
                    month: 12,
                    day: 31,
                    hour: 23,
                    minute: 59,
                    second: 59,
                }
                .to_unix(),
                None
            );
        }

        #[test]
        fn known_dates() {
            let y2k = DateTime {
                year: 2000,
                month: 2,
                day: 29,
                hour: 0,
                minute: 0,
                second: 0,
            };
            assert!(y2k.is_valid());
            assert_eq!(
                DateTime::parse_rfc3339_utc("2000-02-29T00:00:00Z").unwrap(),
                y2k
            );
            assert!(!DateTime {
                year: 2100,
                month: 2,
                day: 29,
                hour: 0,
                minute: 0,
                second: 0,
            }
            .is_valid());
            assert!(DateTime::parse_rfc3339_utc("2100-02-29T00:00:00Z").is_err());

            let end_2038 = DateTime::parse_rfc3339_utc("2038-01-19T03:14:08Z").unwrap();
            assert_eq!(end_2038.to_unix(), Some(2_147_483_648));
            assert_eq!(
                DateTime::from_unix(2_147_483_647).unwrap().format_rfc3339(),
                "2038-01-19T03:14:07Z"
            );

            let max = DateTime {
                year: 9999,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59,
                second: 59,
            };
            let max_unix = max.to_unix().unwrap();
            assert_eq!(DateTime::from_unix(max_unix), Some(max));
            assert_eq!(DateTime::from_unix(max_unix + 1), None);
        }

        #[test]
        fn round_trip_pseudo_random_values() {
            let max_secs = DateTime {
                year: 9999,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59,
                second: 59,
            }
            .to_unix()
            .unwrap();
            let span = u64::try_from(max_secs).unwrap() + 1;
            let mut x = 0x243f_6a88_85a3_08d3_u64;
            for _ in 0..1000 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let secs = i64::try_from(x % span).unwrap();
                let dt = DateTime::from_unix(secs).unwrap();
                assert!(dt.is_valid());
                assert_eq!(dt.to_unix(), Some(secs));
            }
        }

        #[test]
        fn rfc3339_accepts_expected_forms_only() {
            assert_eq!(
                DateTime::parse_rfc3339_utc("2026-11-24T12:34:56Z")
                    .unwrap()
                    .format_rfc3339(),
                "2026-11-24T12:34:56Z"
            );
            assert!(DateTime::parse_rfc3339_utc("2026-11-24t12:34:56z").is_ok());
            for bad in [
                "",
                "2026-11-24 12:34:56Z",
                "2026/11/24T12:34:56Z",
                "2026-11-24T12:34:56+00:00",
                "2026-11-24T12:34:56-01:00",
                "2026-11-24T12:34:56.000Z",
                "2026-11-24T12:34:56",
                "2026-11-24T12:34:56ZZ",
                " 2026-11-24T12:34:56Z",
                "2026-11-24T12:34:56Z ",
                "2026-13-24T00:00:00Z",
                "2026-00-24T00:00:00Z",
                "2026-11-00T00:00:00Z",
                "2026-11-24T24:00:00Z",
                "2026-11-24T12:60:00Z",
                "2026-11-24T12:34:60Z",
                "2026-02-30T00:00:00Z",
                "2026-11-24x12:34:56Z",
                "abcd-11-24T12:34:56Z",
            ] {
                assert!(DateTime::parse_rfc3339_utc(bad).is_err(), "case: {bad:?}");
            }
        }

        #[test]
        fn rtc_file_parsing() {
            let expected = DateTime {
                year: 2026,
                month: 11,
                day: 24,
                hour: 12,
                minute: 34,
                second: 56,
            };
            assert_eq!(
                DateTime::from_rtc_files("2026-11-24\n", "12:34:56\n").unwrap(),
                expected
            );
            assert_eq!(
                DateTime::from_rtc_files("2026-11-24\r\n", "12:34:56\r\n").unwrap(),
                expected
            );
            assert_eq!(
                DateTime::from_rtc_files("2026-11-24", "12:34:56").unwrap(),
                expected
            );
            assert!(DateTime::from_rtc_files("2026-11-24\n\n", "12:34:56\n\n").is_ok());
            for (date, time) in [
                ("", "12:34:56\n"),
                ("2026-11-24\n", ""),
                (" 2026-11-24\n", "12:34:56\n"),
                ("2026-11-24\n", " 12:34:56\n"),
                ("2026-11-24\n", "12:34:56Z\n"),
                ("2026-11-24T00:00:00Z\n", "12:34:56\n"),
                ("2026-13-24\n", "12:34:56\n"),
                ("2026-11-24\n", "24:00:00\n"),
                ("2026-11-24\n", "12:34:60\n"),
                ("20261124\n", "12:34:56\n"),
                ("2026-11-24\n", "123456\n"),
            ] {
                assert!(DateTime::from_rtc_files(date, time).is_err());
            }
        }

        #[test]
        fn formats_values() {
            let dt = DateTime {
                year: 2026,
                month: 11,
                day: 24,
                hour: 12,
                minute: 34,
                second: 56,
            };
            assert_eq!(dt.format_rfc3339(), "2026-11-24T12:34:56Z");
            assert_eq!(dt.format_human(), "24 Nov 2026 12:34:56 UTC");
            assert_eq!(
                DateTime {
                    year: 1970,
                    month: 1,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    second: 0,
                }
                .format_human(),
                "01 Jan 1970 00:00:00 UTC"
            );
        }

        #[test]
        fn der_utc_time_pivot() {
            let y49 = der_time_to_unix(0x17, b"490101000000Z").unwrap();
            let y50 = der_time_to_unix(0x17, b"500101000000Z").unwrap();
            let y99 = der_time_to_unix(0x17, b"990101000000Z").unwrap();
            let y00 = der_time_to_unix(0x17, b"000101000000Z").unwrap();
            assert!(y49 > 0);
            assert!(y50 < 0);
            assert!(y00 > y99);
            assert_eq!(y49, der_time_to_unix(0x18, b"20490101000000Z").unwrap());
            assert_eq!(y50, der_time_to_unix(0x18, b"19500101000000Z").unwrap());
        }

        #[test]
        fn der_rejects_malformed_inputs() {
            for (tag, content) in [
                (0x16, b"490101000000Z".as_slice()),
                (0x17, b"49010100000Z".as_slice()),
                (0x17, b"4901010000000Z".as_slice()),
                (0x17, b"490101000000z".as_slice()),
                (0x17, b"49A101000000Z".as_slice()),
                (0x17, b"49010100000+Z".as_slice()),
                (0x17, b"491301000000Z".as_slice()),
                (0x17, b"490230000000Z".as_slice()),
                (0x17, b"490101240000Z".as_slice()),
                (0x17, b"490101006000Z".as_slice()),
                (0x17, b"490101000060Z".as_slice()),
                (0x18, b"2049010100000Z".as_slice()),
                (0x18, b"204901010000000Z".as_slice()),
                (0x18, b"20490101000000z".as_slice()),
                (0x18, b"20491301000000Z".as_slice()),
                (0x18, b"21000229000000Z".as_slice()),
            ] {
                assert!(der_time_to_unix(tag, content).is_err());
            }
        }

        #[test]
        fn plausible_boundaries() {
            let start = 1_735_689_600;
            let end = 4_102_444_800;
            assert!(!plausible(0));
            assert!(!plausible(start));
            assert!(plausible(start + 1));
            assert!(plausible(end - 1));
            assert!(!plausible(end));
            assert!(!plausible(i64::MIN));
            assert!(!plausible(i64::MAX));
        }
    }
}

pub mod clocksync {
    //! Setting the clock from the network (SNTP).
    //!
    //! Without a correct clock, HTTPS certificates look expired or not yet valid, so hosted models
    //! fail. Many PCs also have a dead or wrong hardware clock. This module asks three time servers,
    //! takes the median answer ([`crate::sntp`]), and sets the system clock when it is off by more
    //! than a couple of seconds. Everything that decides is pure; only [`sync`] touches the network.

    use crate::civil_time::{plausible, DateTime};
    use crate::sntp::{self, SntpFix};
    use std::net::{ToSocketAddrs, UdpSocket};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Time servers asked, all at once.
    pub const SERVERS: [&str; 3] = [
        "time.cloudflare.com:123",
        "time.google.com:123",
        "pool.ntp.org:123",
    ];

    /// The clock is left alone when it is already this close (milliseconds).
    pub const TOLERANCE_MS: i64 = 2_000;

    /// How long to wait for the servers altogether.
    const OVERALL_TIMEOUT: Duration = Duration::from_secs(8);

    /// What to do about a measured offset.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Decision {
        /// The clock is close enough.
        Keep,
        /// Set the clock to this Unix time.
        Set(i64),
        /// The answer is not believable (before 2025 or after 2100); leave the clock.
        Implausible,
    }

    /// Decide from the median `offset_ms` (server time minus local time) and the local Unix time.
    #[must_use]
    pub fn decide(offset_ms: i64, local_unix_secs: i64) -> Decision {
        if offset_ms.abs() <= TOLERANCE_MS {
            return Decision::Keep;
        }
        // Round to the nearest second.
        let corrected =
            local_unix_secs.saturating_add((offset_ms + offset_ms.signum() * 500) / 1000);
        if plausible(corrected) {
            Decision::Set(corrected)
        } else {
            Decision::Implausible
        }
    }

    /// The argument for `date -u -s`, e.g. `2026-11-24 12:34:56`.
    #[must_use]
    pub fn date_argument(unix_secs: i64) -> Option<String> {
        let dt = DateTime::from_unix(unix_secs)?;
        let rfc = dt.format_rfc3339();
        // `2026-11-24T12:34:56Z` -> `2026-11-24 12:34:56`
        Some(rfc.trim_end_matches('Z').replacen('T', " ", 1))
    }

    fn unix_now() -> Option<(SystemTime, i64)> {
        let now = SystemTime::now();
        let secs = i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
        Some((now, secs))
    }

    /// Ask one server.
    ///
    /// # Errors
    /// A short message: name lookup, network or reply problems.
    pub fn query(server: &str) -> Result<SntpFix, String> {
        let addr = server
            .to_socket_addrs()
            .map_err(|e| format!("{server}: {e}"))?
            .find(std::net::SocketAddr::is_ipv4)
            .ok_or_else(|| format!("{server}: no address"))?;
        let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(|e| e.to_string())?;
        socket.connect(addr).map_err(|e| e.to_string())?;
        let sent = SystemTime::now();
        socket
            .send(&sntp::request(sent))
            .map_err(|e| format!("{server}: {e}"))?;
        let mut buf = [0_u8; 128];
        let n = socket
            .recv(&mut buf)
            .map_err(|e| format!("{server}: no answer ({e})"))?;
        let reply = buf.get(..n).unwrap_or_default();
        sntp::parse_reply(reply, sent, SystemTime::now()).map_err(|e| format!("{server}: {e}"))
    }

    fn set_system_clock(unix_secs: i64) -> Result<(), String> {
        let arg = date_argument(unix_secs).ok_or("cannot format the time")?;
        let ok = Command::new("date")
            .args(["-u", "-s", &arg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return Err("the `date` command refused".into());
        }
        // Keep the hardware clock right for the next boot; failure does not matter.
        let _ = Command::new("hwclock")
            .args(["-w", "-u"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }

    /// Ask the servers and, if the clock is wrong, set it. Returns a line for the console.
    ///
    /// # Errors
    /// A message when no usable answer came or the clock could not be set.
    pub fn sync() -> Result<String, String> {
        let (tx, rx) = mpsc::channel();
        for server in SERVERS {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(query(server));
            });
        }
        drop(tx);
        let deadline = std::time::Instant::now() + OVERALL_TIMEOUT;
        let mut fixes = Vec::new();
        let mut errors = Vec::new();
        while fixes.len() + errors.len() < SERVERS.len() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(Ok(f)) => fixes.push(f),
                Ok(Err(e)) => errors.push(e),
                Err(_) => break,
            }
        }
        let offset = sntp::median_offset(&fixes).ok_or_else(|| {
            format!(
                "only {} of {} time servers answered ({})",
                fixes.len(),
                SERVERS.len(),
                errors.first().map_or("timed out", String::as_str)
            )
        })?;
        let (_, local) = unix_now().ok_or("the clock is before 1970")?;
        match decide(offset, local) {
            Decision::Keep => Ok(format!("clock is right (within {} ms)", offset.abs())),
            Decision::Implausible => {
                Err("the servers gave an impossible time; clock left alone".into())
            }
            Decision::Set(t) => {
                set_system_clock(t)?;
                let shown = DateTime::from_unix(t).map_or_else(String::new, |d| d.format_human());
                Ok(format!("clock set from the network: {shown}"))
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const NOW: i64 = 1_795_000_000; // November 2026

        #[test]
        fn close_clocks_are_left_alone() {
            assert_eq!(decide(0, NOW), Decision::Keep);
            assert_eq!(decide(1_999, NOW), Decision::Keep);
            assert_eq!(decide(-2_000, NOW), Decision::Keep);
        }

        #[test]
        fn a_wrong_clock_is_corrected_to_the_nearest_second() {
            assert_eq!(decide(2_001, NOW), Decision::Set(NOW + 2));
            assert_eq!(decide(2_500, NOW), Decision::Set(NOW + 3));
            assert_eq!(decide(-2_500, NOW), Decision::Set(NOW - 3));
            // A clock stuck in 1970 jumps to the present.
            let offset_ms = (NOW - 5) * 1000;
            assert_eq!(decide(offset_ms, 5), Decision::Set(NOW));
        }

        #[test]
        fn impossible_answers_are_not_applied() {
            assert_eq!(decide(1_000_000, 100), Decision::Implausible);
            let far = 5_000_000_000_i64 * 1000; // year ~2128
            assert_eq!(decide(far, NOW), Decision::Implausible);
        }

        #[test]
        fn date_argument_is_what_busybox_date_wants() {
            assert_eq!(
                date_argument(1_700_000_000).as_deref(),
                Some("2023-11-14 22:13:20")
            );
            assert_eq!(date_argument(0).as_deref(), Some("1970-01-01 00:00:00"));
        }
    }
}

pub mod sntp {
    //! Pure packet logic for SNTPv4 (RFC 4330).
    //!
    //! The caller owns UDP send/receive and supplies all times as parameters.
    //! This module only builds request packets and validates/decodes replies.

    use std::error::Error;
    use std::fmt;
    use std::time::{SystemTime, UNIX_EPOCH};

    const NTP_UNIX_EPOCH_DELTA: i128 = 2_208_988_800;
    const MODULUS_2_32: i128 = 4_294_967_296;
    const HIGH_BIT: i128 = 2_147_483_648;
    const MAX_ROUND_TRIP_NS: i128 = 5_000_000_000;

    /// Decoded SNTP correction for a client clock.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SntpFix {
        /// Server time corrected for half the round trip, expressed as whole Unix seconds.
        pub unix_seconds: i64,
        /// Estimated clock offset in milliseconds: `((t2 - t1) + (t3 - t4)) / 2`.
        pub offset_ms: i64,
        /// Measured round-trip time in milliseconds.
        pub round_trip_ms: u32,
    }

    /// Errors returned while validating an SNTP reply packet.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SntpError {
        /// Reply buffer length was not at least 48 bytes.
        WrongLength,
        /// Reply mode was not server mode 4.
        InvalidMode(u8),
        /// Reply NTP version was below 3.
        UnsupportedVersion(u8),
        /// Leap indicator was 3, meaning the server is unsynchronised.
        LeapIndicatorUnsynchronized,
        /// Stratum was 0; the reference identifier contains the kiss-o'-death code.
        KissODeath(String),
        /// Stratum was greater than 15.
        StratumTooHigh(u8),
        /// Originate timestamp did not match the timestamp placed in the request.
        OriginateMismatch,
        /// Transmit timestamp was all zero.
        ZeroTransmitTimestamp,
        /// Round trip exceeded 5000 milliseconds.
        RoundTripTooLong,
        /// Round trip computed to a negative duration.
        NegativeRoundTrip,
    }

    impl fmt::Display for SntpError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::WrongLength => formatter.write_str("SNTP reply shorter than 48 bytes"),
                Self::InvalidMode(mode) => {
                    write!(formatter, "SNTP reply mode {mode} is not server mode 4")
                }
                Self::UnsupportedVersion(version) => {
                    write!(formatter, "SNTP version {version} is below 3")
                }
                Self::LeapIndicatorUnsynchronized => {
                    formatter.write_str("SNTP server reports it is unsynchronised")
                }
                Self::KissODeath(code) => write!(formatter, "SNTP kiss-o'-death code {code}"),
                Self::StratumTooHigh(stratum) => {
                    write!(formatter, "SNTP stratum {stratum} is above 15")
                }
                Self::OriginateMismatch => {
                    formatter.write_str("SNTP reply originate timestamp does not match request")
                }
                Self::ZeroTransmitTimestamp => {
                    formatter.write_str("SNTP reply has a zero transmit timestamp")
                }
                Self::RoundTripTooLong => formatter.write_str("SNTP round trip exceeds 5000 ms"),
                Self::NegativeRoundTrip => formatter.write_str("SNTP round trip is negative"),
            }
        }
    }

    impl Error for SntpError {}

    /// Builds a 48-byte SNTPv4 client request packet.
    ///
    /// LI is 0, VN is 4, Mode is 3, every other field is zero except the
    /// transmit timestamp, which is `transmit_time` encoded as an NTP 64-bit
    /// timestamp.
    #[must_use]
    pub fn request(transmit_time: SystemTime) -> [u8; 48] {
        let mut packet = [0u8; 48];
        packet[0] = 0x23; // LI = 0, VN = 4, Mode = 3
        let timestamp = unix_nanos_to_ntp64(system_time_to_unix_nanos(transmit_time));
        packet[40..48].copy_from_slice(&timestamp.to_be_bytes());
        packet
    }

    /// Parses and validates a 48-byte (or larger) SNTPv4 server reply.
    ///
    /// `sent` is the system time placed in the request's transmit timestamp and
    /// `now` is the system time at which the reply was received. Timestamps whose
    /// NTP seconds field has the high bit clear are interpreted as era 1, after
    /// 2036, and timestamps with the high bit set are interpreted as era 0.
    ///
    /// # Errors
    /// An [`SntpError`] naming the first check the reply failed.
    pub fn parse_reply(
        buf: &[u8],
        sent: SystemTime,
        now: SystemTime,
    ) -> Result<SntpFix, SntpError> {
        if buf.len() < 48 {
            return Err(SntpError::WrongLength);
        }

        let first = *buf.first().ok_or(SntpError::WrongLength)?;
        let leap = first >> 6;
        let version = (first >> 3) & 0x07;
        let mode = first & 0x07;
        let stratum = *buf.get(1).ok_or(SntpError::WrongLength)?;

        if mode != 4 {
            return Err(SntpError::InvalidMode(mode));
        }
        if version < 3 {
            return Err(SntpError::UnsupportedVersion(version));
        }
        if leap == 3 {
            return Err(SntpError::LeapIndicatorUnsynchronized);
        }
        if stratum == 0 {
            let code_bytes = buf.get(12..16).ok_or(SntpError::WrongLength)?;
            return Err(SntpError::KissODeath(
                String::from_utf8_lossy(code_bytes).into_owned(),
            ));
        }
        if stratum > 15 {
            return Err(SntpError::StratumTooHigh(stratum));
        }

        let originate = read_u64_be(buf, 24)?;
        let sent_ntp = unix_nanos_to_ntp64(system_time_to_unix_nanos(sent));
        if originate != sent_ntp {
            return Err(SntpError::OriginateMismatch);
        }

        let receive_ts = read_u64_be(buf, 32)?;
        let transmit_ts = read_u64_be(buf, 40)?;
        if transmit_ts == 0 {
            return Err(SntpError::ZeroTransmitTimestamp);
        }

        let t1 = system_time_to_unix_nanos(sent);
        let t4 = system_time_to_unix_nanos(now);
        let t2 = ntp64_to_unix_nanos(receive_ts);
        let t3 = ntp64_to_unix_nanos(transmit_ts);

        let round_trip_ns = (t4 - t1) - (t3 - t2);
        if round_trip_ns < 0 {
            return Err(SntpError::NegativeRoundTrip);
        }
        if round_trip_ns > MAX_ROUND_TRIP_NS {
            return Err(SntpError::RoundTripTooLong);
        }

        let server_now_ns = t3 + round_trip_ns / 2;
        let unix_seconds = floor_div(server_now_ns, 1_000_000_000) as i64;
        let offset_numerator = (t2 - t1) + (t3 - t4);
        let offset_ms = round_half_away(offset_numerator, 2_000_000) as i64;
        let round_trip_ms = u32::try_from(round_half_away(round_trip_ns, 1_000_000))
            .map_err(|_| SntpError::RoundTripTooLong)?;

        Ok(SntpFix {
            unix_seconds,
            offset_ms,
            round_trip_ms,
        })
    }

    /// Computes the median offset from at least three fixes after rejecting outliers.
    ///
    /// The median of all input offsets is found first. Fixes whose offsets are
    /// more than 1000 ms away from that median are dropped. If fewer than two
    /// fixes remain, `None` is returned; otherwise the median of the remaining
    /// offsets is returned.
    #[must_use]
    pub fn median_offset(fixes: &[SntpFix]) -> Option<i64> {
        if fixes.len() < 3 {
            return None;
        }

        let mut offsets: Vec<i64> = fixes.iter().map(|fix| fix.offset_ms).collect();
        offsets.sort_unstable();

        let median2 = two_times_median_sorted(&offsets)?;
        let kept: Vec<i64> = offsets
            .into_iter()
            .filter(|offset| (2_i128 * i128::from(*offset) - median2).abs() <= 2_000)
            .collect();

        if kept.len() < 2 {
            return None;
        }

        median_sorted(&kept)
    }

    fn system_time_to_unix_nanos(time: SystemTime) -> i128 {
        match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => {
                i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos())
            }
            Err(error) => {
                let duration = error.duration();
                -(i128::from(duration.as_secs()) * 1_000_000_000)
                    - i128::from(duration.subsec_nanos())
            }
        }
    }

    fn unix_nanos_to_ntp64(nanos: i128) -> u64 {
        let seconds = nanos.div_euclid(1_000_000_000);
        let subsec = nanos.rem_euclid(1_000_000_000);
        let ntp_seconds = (seconds + NTP_UNIX_EPOCH_DELTA).rem_euclid(MODULUS_2_32) as u64;
        let fraction = ((subsec as u128 * MODULUS_2_32 as u128) + 500_000_000) / 1_000_000_000;
        (ntp_seconds << 32) | (fraction as u64)
    }

    fn ntp64_to_unix_nanos(timestamp: u64) -> i128 {
        let raw_seconds = (timestamp >> 32) as u32;
        let fraction = i128::from(timestamp & 0xffff_ffff);
        let seconds = ntp_to_unix_secs(raw_seconds);
        let nanos = (fraction * 1_000_000_000 + (1_i128 << 31)) >> 32;
        seconds * 1_000_000_000 + nanos
    }

    fn ntp_to_unix_secs(raw_seconds: u32) -> i128 {
        let raw = i128::from(raw_seconds);
        if raw < HIGH_BIT {
            raw + MODULUS_2_32 - NTP_UNIX_EPOCH_DELTA
        } else {
            raw - NTP_UNIX_EPOCH_DELTA
        }
    }

    fn read_u64_be(buf: &[u8], start: usize) -> Result<u64, SntpError> {
        let end = start.checked_add(8).ok_or(SntpError::WrongLength)?;
        let bytes = buf.get(start..end).ok_or(SntpError::WrongLength)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(array))
    }

    fn floor_div(numerator: i128, denominator: i128) -> i128 {
        let quotient = numerator / denominator;
        let remainder = numerator % denominator;
        if remainder != 0 && ((remainder < 0) != (denominator < 0)) {
            quotient - 1
        } else {
            quotient
        }
    }

    fn round_half_away(numerator: i128, denominator: i128) -> i128 {
        if numerator >= 0 {
            (numerator + denominator / 2) / denominator
        } else {
            -((-numerator + denominator / 2) / denominator)
        }
    }

    fn two_times_median_sorted(values: &[i64]) -> Option<i128> {
        let len = values.len();
        if len == 0 {
            return None;
        }
        let mid = len / 2;
        if len.is_multiple_of(2) {
            let lower = i128::from(*values.get(mid.checked_sub(1)?)?);
            let upper = i128::from(*values.get(mid)?);
            Some(lower + upper)
        } else {
            Some(2 * i128::from(*values.get(mid)?))
        }
    }

    fn median_sorted(values: &[i64]) -> Option<i64> {
        let len = values.len();
        if len == 0 {
            return None;
        }
        let mid = len / 2;
        if len.is_multiple_of(2) {
            let lower = i128::from(*values.get(mid.checked_sub(1)?)?);
            let upper = i128::from(*values.get(mid)?);
            Some(round_half_away(lower + upper, 2) as i64)
        } else {
            values.get(mid).copied()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Duration;

        const NANOS_PER_MS: i128 = 1_000_000;

        fn fix(offset_ms: i64) -> SntpFix {
            SntpFix {
                unix_seconds: 0,
                offset_ms,
                round_trip_ms: 0,
            }
        }

        fn nanos(seconds: i64, subsec_nanos: u32) -> i128 {
            i128::from(seconds) * 1_000_000_000 + i128::from(subsec_nanos)
        }

        fn sent_for_case() -> SystemTime {
            UNIX_EPOCH + Duration::new(1_700_000_000, 250_000_000)
        }

        fn ntp_bytes(timestamp: u64) -> [u8; 8] {
            timestamp.to_be_bytes()
        }

        fn make_reply(
            sent: SystemTime,
            server_offset_ns: i128,
            receive_delay_ns: i128,
            processing_ns: i128,
        ) -> Vec<u8> {
            let sent_nanos = system_time_to_unix_nanos(sent);
            let t2 = sent_nanos + receive_delay_ns + server_offset_ns;
            let t3 = t2 + processing_ns;

            let mut packet = vec![0_u8; 48];
            packet[0] = 0x24; // LI = 0, VN = 4, Mode = 4
            packet[1] = 1; // stratum 1
            packet[24..32].copy_from_slice(&ntp_bytes(unix_nanos_to_ntp64(sent_nanos)));
            packet[32..40].copy_from_slice(&ntp_bytes(unix_nanos_to_ntp64(t2)));
            packet[40..48].copy_from_slice(&ntp_bytes(unix_nanos_to_ntp64(t3)));
            packet
        }

        fn base_case() -> (SystemTime, SystemTime, Vec<u8>) {
            let sent = sent_for_case();
            let now = sent + Duration::from_millis(100);
            let reply = make_reply(
                sent,
                50 * NANOS_PER_MS,
                20 * NANOS_PER_MS,
                10 * NANOS_PER_MS,
            );
            (sent, now, reply)
        }

        #[test]
        fn request_sets_only_expected_fields() {
            let sent = sent_for_case();
            let packet = request(sent);
            assert_eq!(packet[0], 0x23);
            assert!(packet[1..40].iter().all(|byte| *byte == 0));
            let expected = unix_nanos_to_ntp64(system_time_to_unix_nanos(sent));
            assert_eq!(
                u64::from_be_bytes(packet[40..48].try_into().unwrap()),
                expected
            );
        }

        #[test]
        fn parses_hand_built_valid_reply() {
            let (sent, now, reply) = base_case();
            let parsed = parse_reply(&reply, sent, now).unwrap();
            assert_eq!(parsed.unix_seconds, 1_700_000_000);
            assert_eq!(parsed.offset_ms, 25);
            assert_eq!(parsed.round_trip_ms, 90);
        }

        #[test]
        fn request_and_parse_wrap_into_ntp_era_one_after_2036() {
            let sent = UNIX_EPOCH + Duration::new(2_116_000_000, 123_000_000); // 2037
            let raw = (system_time_to_unix_nanos(sent).div_euclid(1_000_000_000)
                + NTP_UNIX_EPOCH_DELTA)
                .rem_euclid(MODULUS_2_32);
            assert!(raw < HIGH_BIT);

            let now = sent + Duration::from_millis(50);
            let reply = make_reply(
                sent,
                150 * NANOS_PER_MS,
                10 * NANOS_PER_MS,
                5 * NANOS_PER_MS,
            );
            let parsed = parse_reply(&reply, sent, now).unwrap();
            assert_eq!(parsed.unix_seconds, 2_116_000_000);
            assert_eq!(parsed.offset_ms, 138); // 137.5 rounds half away from zero
            assert_eq!(parsed.round_trip_ms, 45);
        }

        #[test]
        fn rejects_short_buffer() {
            let (sent, now, _) = base_case();
            let short = [0_u8; 47];
            assert_eq!(parse_reply(&short, sent, now), Err(SntpError::WrongLength));
        }

        #[test]
        fn rejects_mode_other_than_server() {
            let (sent, now, mut reply) = base_case();
            reply[0] = 0x23; // client mode
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::InvalidMode(3))
            );
        }

        #[test]
        fn rejects_version_below_three() {
            let (sent, now, mut reply) = base_case();
            reply[0] = 0x14; // LI = 0, VN = 2, Mode = 4
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::UnsupportedVersion(2))
            );
        }

        #[test]
        fn rejects_leap_indicator_three() {
            let (sent, now, mut reply) = base_case();
            reply[0] = 0xe4; // LI = 3, VN = 4, Mode = 4
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::LeapIndicatorUnsynchronized)
            );
        }

        #[test]
        fn rejects_kiss_o_death_and_returns_code() {
            let (sent, now, mut reply) = base_case();
            reply[1] = 0;
            reply[12..16].copy_from_slice(b"RATE");
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::KissODeath("RATE".to_string()))
            );

            reply[12..16].copy_from_slice(b"DENY");
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::KissODeath("DENY".to_string()))
            );
        }

        #[test]
        fn rejects_stratum_above_fifteen() {
            let (sent, now, mut reply) = base_case();
            reply[1] = 16;
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::StratumTooHigh(16))
            );
        }

        #[test]
        fn accepts_stratum_fifteen() {
            let (sent, now, mut reply) = base_case();
            reply[1] = 15;
            assert!(parse_reply(&reply, sent, now).is_ok());
        }

        #[test]
        fn rejects_originate_timestamp_mismatch_in_low_bit() {
            let (sent, now, mut reply) = base_case();
            reply[31] ^= 0x01; // same high 32 bits, different low 32 bits
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::OriginateMismatch)
            );
        }

        #[test]
        fn rejects_zero_transmit_timestamp() {
            let (sent, now, mut reply) = base_case();
            reply[40..48].fill(0);
            assert_eq!(
                parse_reply(&reply, sent, now),
                Err(SntpError::ZeroTransmitTimestamp)
            );
        }

        #[test]
        fn rejects_round_trip_over_5000_ms_but_accepts_exactly_5000_ms() {
            let sent = sent_for_case();
            let reply = make_reply(sent, 0, 0, 0);

            let too_late = sent + Duration::from_millis(5_001);
            assert_eq!(
                parse_reply(&reply, sent, too_late),
                Err(SntpError::RoundTripTooLong)
            );

            let exactly = sent + Duration::from_millis(5_000);
            let parsed = parse_reply(&reply, sent, exactly).unwrap();
            assert_eq!(parsed.round_trip_ms, 5_000);
        }

        #[test]
        fn rejects_negative_round_trip() {
            let sent = sent_for_case();
            let reply = make_reply(sent, 0, 100 * NANOS_PER_MS, 100 * NANOS_PER_MS);
            assert_eq!(
                parse_reply(&reply, sent, sent),
                Err(SntpError::NegativeRoundTrip)
            );
        }

        #[test]
        fn transmit_before_receive_still_follows_the_rfc_formula() {
            let (sent, now, mut reply) = base_case();
            let sent_nanos = system_time_to_unix_nanos(sent);
            let receive = unix_nanos_to_ntp64(sent_nanos + 500 * NANOS_PER_MS);
            let transmit = unix_nanos_to_ntp64(sent_nanos + 100 * NANOS_PER_MS);
            reply[32..40].copy_from_slice(&ntp_bytes(receive));
            reply[40..48].copy_from_slice(&ntp_bytes(transmit));
            let fix = parse_reply(&reply, sent, now).unwrap();
            assert_eq!(fix.round_trip_ms, 500);
            assert_eq!(fix.offset_ms, 250);
        }

        #[test]
        fn rounding_is_half_away_from_zero() {
            assert_eq!(round_half_away(1_500, 1_000), 2);
            assert_eq!(round_half_away(-1_500, 1_000), -2);
            assert_eq!(round_half_away(1_499, 1_000), 1);
            assert_eq!(round_half_away(-1_499, 1_000), -1);
            assert_eq!(round_half_away(2_000_000_000, 4_000_000_000), 1);
            assert_eq!(round_half_away(-2_000_000_000, 4_000_000_000), -1);
        }

        #[test]
        fn ntp_fraction_rounding_and_unix_second_flooring() {
            let exact_half_ms = nanos(1_700_000_000, 500_000);
            let timestamp = unix_nanos_to_ntp64(exact_half_ms);
            assert_eq!(ntp64_to_unix_nanos(timestamp), exact_half_ms);
            assert_eq!(floor_div(-1, 1_000_000_000), -1);
            assert_eq!(floor_div(1, 1_000_000_000), 0);
            assert_eq!(floor_div(-1_500_000_000, 1_000_000_000), -2);
        }

        #[test]
        fn median_offset_requires_at_least_three_fixes() {
            assert_eq!(median_offset(&[]), None);
            assert_eq!(median_offset(&[fix(1), fix(2)]), None);
        }

        #[test]
        fn median_offset_drops_distant_outlier() {
            let fixes = [fix(10), fix(20), fix(30), fix(40), fix(5_000)];
            assert_eq!(median_offset(&fixes), Some(25));

            let low_outlier = [fix(-5_000), fix(10), fix(20), fix(30), fix(40)];
            assert_eq!(median_offset(&low_outlier), Some(25));
        }

        #[test]
        fn median_offset_returns_none_when_fewer_than_two_survive() {
            let fixes = [fix(0), fix(5_000), fix(10_000)];
            assert_eq!(median_offset(&fixes), None);
        }

        #[test]
        fn median_offset_handles_even_survivor_count_with_half_up_rounding() {
            let fixes = [fix(0), fix(10), fix(20), fix(1_000)];
            assert_eq!(median_offset(&fixes), Some(15));
        }

        #[test]
        fn parsed_error_display_is_descriptive() {
            let error = SntpError::KissODeath("RATE".to_string());
            assert_eq!(error.to_string(), "SNTP kiss-o'-death code RATE");
            let error = SntpError::NegativeRoundTrip;
            assert_eq!(error.to_string(), "SNTP round trip is negative");
        }
    }
}

pub mod fslabel {
    //! Reading a volume label straight from the start of a block device, so the data partition can
    //! be found without `blkid` or udev (the image has neither).
    //!
    //! Supported: ext2/3/4 and FAT12/16/32 (the formats a person can make on any computer).

    /// Bytes of the device needed by [`read_label`].
    pub const PROBE_BYTES: usize = 2048;

    fn clean(bytes: &[u8]) -> Option<String> {
        let s = String::from_utf8_lossy(bytes);
        let s = s.trim_end_matches(['\0', ' ']);
        (!s.is_empty()).then(|| s.to_string())
    }

    /// The volume label from the first [`PROBE_BYTES`] of a device, or `None` when the filesystem
    /// is unknown or has no label.
    #[must_use]
    pub fn read_label(head: &[u8]) -> Option<String> {
        // ext2/3/4: superblock at 1024, magic 0xEF53 at +56, 16-byte label at +120.
        if head.len() >= 1160 && head[1080..1082] == [0x53, 0xEF] {
            return clean(&head[1144..1160]);
        }
        if head.len() >= 512 && head[510..512] == [0x55, 0xAA] {
            // FAT32: type string at 82, label at 71.
            if head[82..87] == *b"FAT32" {
                return clean(&head[71..82]);
            }
            // FAT12/16: type string at 54, label at 43.
            if head[54..57] == *b"FAT" && matches!(&head[57..59], b"12" | b"16") {
                return clean(&head[43..54]);
            }
        }
        None
    }

    /// First device in `names` whose label equals `label` (ignoring case, since FAT upper-cases
    /// labels). `probe` returns a device's label.
    pub fn find_labelled<F: FnMut(&str) -> Option<String>>(
        names: &[String],
        label: &str,
        mut probe: F,
    ) -> Option<String> {
        names
            .iter()
            .find(|n| probe(n).is_some_and(|l| l.eq_ignore_ascii_case(label)))
            .cloned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn ext4(label: &str) -> Vec<u8> {
            let mut b = vec![0u8; PROBE_BYTES];
            b[1080] = 0x53;
            b[1081] = 0xEF;
            b[1144..1144 + label.len()].copy_from_slice(label.as_bytes());
            b
        }

        fn fat(label: &str, kind: &str) -> Vec<u8> {
            let mut b = vec![0u8; PROBE_BYTES];
            b[510] = 0x55;
            b[511] = 0xAA;
            let mut l = format!("{label:<11}").into_bytes();
            l.truncate(11);
            if kind == "FAT32" {
                b[71..82].copy_from_slice(&l);
                b[82..90].copy_from_slice(b"FAT32   ");
            } else {
                b[43..54].copy_from_slice(&l);
                b[54..62].copy_from_slice(format!("{kind:<8}").as_bytes());
            }
            b
        }

        #[test]
        fn reads_ext4_and_fat_labels() {
            assert_eq!(
                read_label(&ext4("MOULI-DATA")).as_deref(),
                Some("MOULI-DATA")
            );
            assert_eq!(read_label(&ext4("")), None);
            assert_eq!(
                read_label(&fat("MOULI-DATA", "FAT32")).as_deref(),
                Some("MOULI-DATA")
            );
            assert_eq!(read_label(&fat("USB", "FAT16")).as_deref(), Some("USB"));
            assert_eq!(read_label(&fat("X", "FAT12")).as_deref(), Some("X"));
        }

        #[test]
        fn unknown_or_short_input_has_no_label() {
            assert_eq!(read_label(&[0u8; PROBE_BYTES]), None);
            assert_eq!(read_label(&[]), None);
            assert_eq!(read_label(&[0xEF; 100]), None);
            // an exFAT/NTFS boot sector is not recognised
            let mut b = vec![0u8; PROBE_BYTES];
            b[510] = 0x55;
            b[511] = 0xAA;
            b[3..11].copy_from_slice(b"EXFAT   ");
            assert_eq!(read_label(&b), None);
        }

        #[test]
        fn finds_by_label_ignoring_case() {
            let names: Vec<String> = ["sda1", "sdb", "sdc1"].map(String::from).to_vec();
            let found = find_labelled(&names, "MOULI-DATA", |n| {
                Some(if n == "sdb" { "mouli-data" } else { "OTHER" }.to_string())
            });
            assert_eq!(found.as_deref(), Some("sdb"));
            assert_eq!(find_labelled(&names, "MOULI-DATA", |_| None), None);
        }
    }
}

pub mod partition_table {
    //! In-memory MBR and GPT partition-table parsing and editing.
    //!
    //! This module operates exclusively on caller-provided byte slices.
    //! It supports 512-byte logical sectors, validates GPT header and partition
    //! entry CRC32 checksums, detects malformed and overlapping partitions,
    //! calculates aligned free space, and creates primary and backup GPT data.
    //!
    //! No filesystem access, external dependencies, or unsafe code are used.

    use std::error::Error;
    use std::fmt;

    /// Logical sector size in bytes.
    pub const SECTOR_SIZE: usize = 512;

    /// Number of sectors required for the primary GPT region.
    pub const GPT_PRIMARY_SECTORS: usize = 34;

    /// Number of GPT partition entries supported.
    pub const MAX_GPT_ENTRIES: u32 = 256;

    /// Maximum supported GPT entry size in bytes.
    pub const MAX_GPT_ENTRY_SIZE: u32 = 512;

    /// Alignment in sectors corresponding to 1 MiB.
    pub const PARTITION_ALIGNMENT: u64 = 2048;

    /// EFI System Partition type GUID in GPT mixed-endian byte order.
    pub const EFI_SYSTEM_GUID: [u8; 16] = [
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9,
        0x3b,
    ];

    /// Linux filesystem partition type GUID in GPT mixed-endian byte order.
    pub const LINUX_FILESYSTEM_GUID: [u8; 16] = [
        0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d,
        0xe4,
    ];

    const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
    const MBR_SIGNATURE: [u8; 2] = [0x55, 0xaa];
    const GPT_HEADER_MIN_SIZE: usize = 92;
    const MBR_ENTRY_OFFSET: usize = 446;
    const MBR_ENTRY_SIZE: usize = 16;

    /// Errors returned by partition-table operations.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PartError {
        /// The supplied buffer is too small.
        BufferTooSmall,
        /// The MBR signature is missing.
        InvalidMbrSignature,
        /// The GPT signature is missing.
        InvalidGptSignature,
        /// The protective MBR entry is missing.
        MissingProtectiveMbr,
        /// A GPT header contains invalid values.
        InvalidGptHeader,
        /// The GPT header checksum is invalid.
        HeaderCrcMismatch,
        /// The GPT partition-entry checksum is invalid.
        EntriesCrcMismatch,
        /// The GPT partition-entry array is outside the supplied buffer.
        EntriesOutOfBounds,
        /// The GPT partition-entry count or size is unsupported.
        InvalidEntryGeometry,
        /// A partition has invalid or out-of-range LBAs.
        InvalidPartitionRange,
        /// Two or more partitions overlap.
        OverlappingPartitions,
        /// No empty GPT partition-entry slot is available.
        NoFreeEntry,
        /// A partition type or unique GUID is invalid.
        InvalidGuid,
        /// The requested backup GPT location is invalid.
        InvalidBackupLocation,
        /// Arithmetic overflow occurred while calculating an offset or size.
        ArithmeticOverflow,
        /// A supplied partition name cannot be represented in the GPT name field.
        InvalidPartitionName,
    }

    impl fmt::Display for PartError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            let message = match self {
                Self::BufferTooSmall => "buffer is too small",
                Self::InvalidMbrSignature => "invalid MBR signature",
                Self::InvalidGptSignature => "invalid GPT signature",
                Self::MissingProtectiveMbr => "protective MBR entry is missing",
                Self::InvalidGptHeader => "invalid GPT header",
                Self::HeaderCrcMismatch => "GPT header CRC32 mismatch",
                Self::EntriesCrcMismatch => "GPT entries CRC32 mismatch",
                Self::EntriesOutOfBounds => "GPT entries are outside the supplied buffer",
                Self::InvalidEntryGeometry => "invalid GPT entry geometry",
                Self::InvalidPartitionRange => "invalid partition LBA range",
                Self::OverlappingPartitions => "overlapping partitions",
                Self::NoFreeEntry => "no free GPT entry slot",
                Self::InvalidGuid => "invalid partition GUID",
                Self::InvalidBackupLocation => "invalid backup GPT location",
                Self::ArithmeticOverflow => "partition-table arithmetic overflow",
                Self::InvalidPartitionName => "invalid partition name",
            };

            formatter.write_str(message)
        }
    }

    impl Error for PartError {}

    /// An MBR partition entry.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MbrEntry {
        /// Boot indicator: `0x80` for bootable, `0x00` otherwise.
        pub boot_flag: u8,
        /// MBR partition type.
        pub partition_type: u8,
        /// Starting LBA.
        pub start_lba: u32,
        /// Number of sectors.
        pub sector_count: u32,
    }

    /// A GPT partition entry.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GptEntry {
        /// Partition type GUID.
        pub type_guid: [u8; 16],
        /// Unique partition GUID.
        pub unique_guid: [u8; 16],
        /// First partition LBA.
        pub first_lba: u64,
        /// Last partition LBA, inclusive.
        pub last_lba: u64,
        /// Partition attribute flags.
        pub attributes: u64,
        /// Partition name decoded from UTF-16LE.
        pub name: String,
    }

    /// Parsed GPT disk information.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GptInfo {
        /// GPT revision.
        pub revision: u32,
        /// Header size in bytes.
        pub header_size: u32,
        /// Current header LBA.
        pub my_lba: u64,
        /// Alternate GPT header LBA.
        pub alternate_lba: u64,
        /// First usable partition LBA.
        pub first_usable_lba: u64,
        /// Last usable partition LBA.
        pub last_usable_lba: u64,
        /// Disk GUID.
        pub disk_guid: [u8; 16],
        /// Partition-entry array starting LBA.
        pub entries_lba: u64,
        /// Number of partition-entry slots.
        pub entry_count: u32,
        /// Size of each partition entry.
        pub entry_size: u32,
        /// Parsed non-empty partition entries.
        pub entries: Vec<GptEntry>,
    }

    /// Detected partition-table format.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Table {
        /// Legacy Master Boot Record.
        Mbr(Vec<MbrEntry>),
        /// GUID Partition Table.
        Gpt(GptInfo),
    }

    /// CRC32 IEEE lookup table, initialized at compile time.
    const fn build_crc32_table() -> [u32; 256] {
        let mut table = [0_u32; 256];
        let mut index = 0_usize;

        while index < 256 {
            let mut crc = index as u32;
            let mut bit = 0;

            while bit < 8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xedb8_8320;
                } else {
                    crc >>= 1;
                }

                bit += 1;
            }

            table[index] = crc;
            index += 1;
        }

        table
    }

    const CRC32_TABLE: [u32; 256] = build_crc32_table();

    /// Calculates the IEEE reflected CRC32 checksum.
    ///
    /// Uses polynomial `0xEDB88320`, initial value `0xFFFFFFFF`,
    /// and final XOR `0xFFFFFFFF`.
    #[must_use]
    pub fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffff_u32;

        for byte in data {
            let index = ((crc ^ u32::from(*byte)) & 0xff) as usize;
            crc = CRC32_TABLE[index] ^ (crc >> 8);
        }

        crc ^ 0xffff_ffff
    }

    fn read_u32(data: &[u8], offset: usize) -> Result<u32, PartError> {
        let end = offset.checked_add(4).ok_or(PartError::ArithmeticOverflow)?;

        let bytes = data.get(offset..end).ok_or(PartError::BufferTooSmall)?;
        let array: [u8; 4] = bytes.try_into().map_err(|_| PartError::BufferTooSmall)?;

        Ok(u32::from_le_bytes(array))
    }

    fn read_u64(data: &[u8], offset: usize) -> Result<u64, PartError> {
        let end = offset.checked_add(8).ok_or(PartError::ArithmeticOverflow)?;

        let bytes = data.get(offset..end).ok_or(PartError::BufferTooSmall)?;
        let array: [u8; 8] = bytes.try_into().map_err(|_| PartError::BufferTooSmall)?;

        Ok(u64::from_le_bytes(array))
    }

    fn write_u32(data: &mut [u8], offset: usize, value: u32) -> Result<(), PartError> {
        let end = offset.checked_add(4).ok_or(PartError::ArithmeticOverflow)?;

        let destination = data.get_mut(offset..end).ok_or(PartError::BufferTooSmall)?;

        destination.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn write_u64(data: &mut [u8], offset: usize, value: u64) -> Result<(), PartError> {
        let end = offset.checked_add(8).ok_or(PartError::ArithmeticOverflow)?;

        let destination = data.get_mut(offset..end).ok_or(PartError::BufferTooSmall)?;

        destination.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn read_guid(data: &[u8], offset: usize) -> Result<[u8; 16], PartError> {
        let end = offset
            .checked_add(16)
            .ok_or(PartError::ArithmeticOverflow)?;

        let bytes = data.get(offset..end).ok_or(PartError::BufferTooSmall)?;
        let mut guid = [0_u8; 16];
        guid.copy_from_slice(bytes);

        Ok(guid)
    }

    fn write_guid(data: &mut [u8], offset: usize, guid: &[u8; 16]) -> Result<(), PartError> {
        let end = offset
            .checked_add(16)
            .ok_or(PartError::ArithmeticOverflow)?;

        let destination = data.get_mut(offset..end).ok_or(PartError::BufferTooSmall)?;

        destination.copy_from_slice(guid);
        Ok(())
    }

    fn checked_byte_offset(lba: u64) -> Result<usize, PartError> {
        let bytes = lba
            .checked_mul(u64::try_from(SECTOR_SIZE).map_err(|_| PartError::ArithmeticOverflow)?)
            .ok_or(PartError::ArithmeticOverflow)?;

        usize::try_from(bytes).map_err(|_| PartError::ArithmeticOverflow)
    }

    fn checked_entry_bytes(count: u32, size: u32) -> Result<usize, PartError> {
        let bytes = count
            .checked_mul(size)
            .ok_or(PartError::ArithmeticOverflow)?;

        usize::try_from(bytes).map_err(|_| PartError::ArithmeticOverflow)
    }

    fn parse_mbr(data: &[u8]) -> Result<Vec<MbrEntry>, PartError> {
        if data.len() < SECTOR_SIZE {
            return Err(PartError::BufferTooSmall);
        }

        if data.get(510..512) != Some(MBR_SIGNATURE.as_slice()) {
            return Err(PartError::InvalidMbrSignature);
        }

        let mut entries = Vec::new();

        for index in 0..4_usize {
            let offset = MBR_ENTRY_OFFSET
                .checked_add(
                    index
                        .checked_mul(MBR_ENTRY_SIZE)
                        .ok_or(PartError::ArithmeticOverflow)?,
                )
                .ok_or(PartError::ArithmeticOverflow)?;

            let entry = data
                .get(offset..offset + MBR_ENTRY_SIZE)
                .ok_or(PartError::BufferTooSmall)?;

            let boot_flag = *entry.first().ok_or(PartError::BufferTooSmall)?;
            let partition_type = *entry.get(4).ok_or(PartError::BufferTooSmall)?;

            if partition_type == 0 {
                continue;
            }

            if boot_flag != 0 && boot_flag != 0x80 {
                return Err(PartError::InvalidPartitionRange);
            }

            let start_lba = u32::from_le_bytes(
                entry
                    .get(8..12)
                    .ok_or(PartError::BufferTooSmall)?
                    .try_into()
                    .map_err(|_| PartError::BufferTooSmall)?,
            );

            let sector_count = u32::from_le_bytes(
                entry
                    .get(12..16)
                    .ok_or(PartError::BufferTooSmall)?
                    .try_into()
                    .map_err(|_| PartError::BufferTooSmall)?,
            );

            if sector_count == 0 {
                return Err(PartError::InvalidPartitionRange);
            }

            start_lba
                .checked_add(sector_count)
                .ok_or(PartError::ArithmeticOverflow)?;

            entries.push(MbrEntry {
                boot_flag,
                partition_type,
                start_lba,
                sector_count,
            });
        }

        Ok(entries)
    }

    fn decode_gpt_name(bytes: &[u8]) -> String {
        let mut units = Vec::new();

        for chunk in bytes.as_chunks::<2>().0 {
            let unit = u16::from_le_bytes(*chunk);

            if unit == 0 {
                break;
            }

            units.push(unit);
        }

        String::from_utf16_lossy(&units)
    }

    fn parse_gpt(data: &[u8]) -> Result<GptInfo, PartError> {
        let minimum_size = GPT_PRIMARY_SECTORS
            .checked_mul(SECTOR_SIZE)
            .ok_or(PartError::ArithmeticOverflow)?;

        if data.len() < minimum_size {
            return Err(PartError::BufferTooSmall);
        }

        if data.get(SECTOR_SIZE..SECTOR_SIZE + 8) != Some(GPT_SIGNATURE.as_slice()) {
            return Err(PartError::InvalidGptSignature);
        }

        if data.get(510..512) != Some(MBR_SIGNATURE.as_slice()) {
            return Err(PartError::InvalidMbrSignature);
        }

        let protective_type = data.get(MBR_ENTRY_OFFSET + 4).copied();

        if protective_type != Some(0xee) {
            return Err(PartError::MissingProtectiveMbr);
        }

        let header = data
            .get(SECTOR_SIZE..SECTOR_SIZE * 2)
            .ok_or(PartError::BufferTooSmall)?;

        let revision = read_u32(header, 8)?;
        let header_size = read_u32(header, 12)?;
        let expected_header_crc = read_u32(header, 16)?;

        let header_size_usize =
            usize::try_from(header_size).map_err(|_| PartError::InvalidGptHeader)?;

        if !(GPT_HEADER_MIN_SIZE..=SECTOR_SIZE).contains(&header_size_usize) {
            return Err(PartError::InvalidGptHeader);
        }

        let mut header_copy = header
            .get(..header_size_usize)
            .ok_or(PartError::InvalidGptHeader)?
            .to_vec();

        let crc_field = header_copy
            .get_mut(16..20)
            .ok_or(PartError::InvalidGptHeader)?;

        crc_field.fill(0);

        if crc32(&header_copy) != expected_header_crc {
            return Err(PartError::HeaderCrcMismatch);
        }

        let my_lba = read_u64(header, 24)?;
        let alternate_lba = read_u64(header, 32)?;
        let first_usable_lba = read_u64(header, 40)?;
        let last_usable_lba = read_u64(header, 48)?;
        let disk_guid = read_guid(header, 56)?;
        let entries_lba = read_u64(header, 72)?;
        let entry_count = read_u32(header, 80)?;
        let entry_size = read_u32(header, 84)?;
        let expected_entries_crc = read_u32(header, 88)?;

        if revision < 0x0001_0000
            || my_lba != 1
            || alternate_lba <= my_lba
            || first_usable_lba > last_usable_lba
            || entries_lba < 2
            || entry_count == 0
            || entry_count > MAX_GPT_ENTRIES
            || !(128..=MAX_GPT_ENTRY_SIZE).contains(&entry_size)
            || !entry_size.is_multiple_of(8)
        {
            return Err(PartError::InvalidGptHeader);
        }

        let entries_length = checked_entry_bytes(entry_count, entry_size)?;
        let entries_offset = checked_byte_offset(entries_lba)?;
        let entries_end = entries_offset
            .checked_add(entries_length)
            .ok_or(PartError::ArithmeticOverflow)?;

        if entries_end > data.len() {
            return Err(PartError::EntriesOutOfBounds);
        }

        let entries_data = data
            .get(entries_offset..entries_end)
            .ok_or(PartError::EntriesOutOfBounds)?;

        if crc32(entries_data) != expected_entries_crc {
            return Err(PartError::EntriesCrcMismatch);
        }

        let entry_size_usize =
            usize::try_from(entry_size).map_err(|_| PartError::InvalidEntryGeometry)?;

        let mut entries = Vec::new();

        for index in 0..entry_count {
            let offset = usize::try_from(index)
                .map_err(|_| PartError::ArithmeticOverflow)?
                .checked_mul(entry_size_usize)
                .ok_or(PartError::ArithmeticOverflow)?;

            let entry_end = offset
                .checked_add(entry_size_usize)
                .ok_or(PartError::ArithmeticOverflow)?;

            let raw = entries_data
                .get(offset..entry_end)
                .ok_or(PartError::EntriesOutOfBounds)?;

            let type_guid = read_guid(raw, 0)?;

            if type_guid.iter().all(|byte| *byte == 0) {
                continue;
            }

            let unique_guid = read_guid(raw, 16)?;
            let first_lba = read_u64(raw, 32)?;
            let last_lba = read_u64(raw, 40)?;
            let attributes = read_u64(raw, 48)?;

            if first_lba > last_lba || first_lba < first_usable_lba || last_lba > last_usable_lba {
                return Err(PartError::InvalidPartitionRange);
            }

            let name_bytes = raw.get(56..128).ok_or(PartError::InvalidEntryGeometry)?;

            entries.push(GptEntry {
                type_guid,
                unique_guid,
                first_lba,
                last_lba,
                attributes,
                name: decode_gpt_name(name_bytes),
            });
        }

        let mut ranges: Vec<(u64, u64)> = entries
            .iter()
            .map(|entry| (entry.first_lba, entry.last_lba))
            .collect();

        ranges.sort_unstable();

        for pair in ranges.windows(2) {
            let first = pair.first().ok_or(PartError::InvalidPartitionRange)?;
            let second = pair.get(1).ok_or(PartError::InvalidPartitionRange)?;

            if second.0 <= first.1 {
                return Err(PartError::OverlappingPartitions);
            }
        }

        Ok(GptInfo {
            revision,
            header_size,
            my_lba,
            alternate_lba,
            first_usable_lba,
            last_usable_lba,
            disk_guid,
            entries_lba,
            entry_count,
            entry_size,
            entries,
        })
    }

    /// Detects and parses an MBR or primary GPT partition table.
    ///
    /// GPT detection requires the primary 34-sector region. The protective
    /// MBR and `EFI PART` header signature must both be present.
    /// Otherwise, a valid MBR is parsed.
    pub fn detect(first_sectors: &[u8]) -> Result<Table, PartError> {
        if first_sectors.len() >= SECTOR_SIZE * 2
            && first_sectors.get(SECTOR_SIZE..SECTOR_SIZE + 8) == Some(GPT_SIGNATURE.as_slice())
        {
            return parse_gpt(first_sectors).map(Table::Gpt);
        }

        parse_mbr(first_sectors).map(Table::Mbr)
    }

    /// Aligns an LBA upward to the next 1 MiB boundary.
    fn align_up(lba: u64) -> Option<u64> {
        let remainder = lba % PARTITION_ALIGNMENT;

        if remainder == 0 {
            Some(lba)
        } else {
            lba.checked_add(PARTITION_ALIGNMENT - remainder)
        }
    }

    fn find_largest_gap(mut ranges: Vec<(u64, u64)>, start: u64, end: u64) -> Option<(u64, u64)> {
        if start > end {
            return None;
        }

        ranges.sort_unstable();

        let mut cursor = start;
        let mut best: Option<(u64, u64)> = None;

        for (partition_start, partition_end) in ranges {
            if partition_end < cursor {
                continue;
            }

            if partition_start > end {
                break;
            }

            if partition_start > cursor {
                let gap_start = align_up(cursor)?;
                let gap_end = partition_start.min(end.saturating_add(1));

                if gap_start < gap_end {
                    let length = gap_end.checked_sub(gap_start)?;

                    if length >= PARTITION_ALIGNMENT
                        && best.is_none_or(|(_, best_length)| length > best_length)
                    {
                        best = Some((gap_start, length));
                    }
                }
            }

            cursor = cursor.max(partition_end.checked_add(1)?);

            if cursor > end {
                break;
            }
        }

        if cursor <= end {
            let gap_start = align_up(cursor)?;
            let gap_end = end.checked_add(1)?;

            if gap_start < gap_end {
                let length = gap_end.checked_sub(gap_start)?;

                if length >= PARTITION_ALIGNMENT
                    && best.is_none_or(|(_, best_length)| length > best_length)
                {
                    best = Some((gap_start, length));
                }
            }
        }

        best
    }

    /// Finds the largest unused, 1 MiB-aligned partition region.
    ///
    /// Returns `(start_lba, sector_count)`. Gaps shorter than 2048 sectors
    /// are ignored.
    ///
    /// For GPT, searches only the usable LBA range. For MBR, searches from
    /// LBA 2048 to the final disk sector.
    #[must_use]
    pub fn largest_free_gap(table: &Table, disk_sectors: u64) -> Option<(u64, u64)> {
        if disk_sectors == 0 {
            return None;
        }

        match table {
            Table::Mbr(entries) => {
                let ranges = entries
                    .iter()
                    .map(|entry| {
                        let end = u64::from(entry.start_lba)
                            .checked_add(u64::from(entry.sector_count))?
                            .checked_sub(1)?;

                        Some((u64::from(entry.start_lba), end))
                    })
                    .collect::<Option<Vec<_>>>()?;

                find_largest_gap(ranges, PARTITION_ALIGNMENT, disk_sectors.checked_sub(1)?)
            }
            Table::Gpt(info) => {
                let ranges = info
                    .entries
                    .iter()
                    .map(|entry| (entry.first_lba, entry.last_lba))
                    .collect();

                find_largest_gap(ranges, info.first_usable_lba, info.last_usable_lba)
            }
        }
    }

    /// Recomputes both CRC fields in a primary GPT region.
    fn update_gpt_crcs(data: &mut [u8]) -> Result<(), PartError> {
        let header = data
            .get(SECTOR_SIZE..SECTOR_SIZE * 2)
            .ok_or(PartError::BufferTooSmall)?;

        let entry_lba = read_u64(header, 72)?;
        let entry_count = read_u32(header, 80)?;
        let entry_size = read_u32(header, 84)?;

        let entries_length = checked_entry_bytes(entry_count, entry_size)?;
        let entries_offset = checked_byte_offset(entry_lba)?;
        let entries_end = entries_offset
            .checked_add(entries_length)
            .ok_or(PartError::ArithmeticOverflow)?;

        let entries = data
            .get(entries_offset..entries_end)
            .ok_or(PartError::EntriesOutOfBounds)?;

        let entries_crc = crc32(entries);

        let header = data
            .get_mut(SECTOR_SIZE..SECTOR_SIZE * 2)
            .ok_or(PartError::BufferTooSmall)?;

        write_u32(header, 88, entries_crc)?;
        write_u32(header, 16, 0)?;

        let header_size =
            usize::try_from(read_u32(header, 12)?).map_err(|_| PartError::InvalidGptHeader)?;

        if !(GPT_HEADER_MIN_SIZE..=SECTOR_SIZE).contains(&header_size) {
            return Err(PartError::InvalidGptHeader);
        }

        let checksum_data = header
            .get(..header_size)
            .ok_or(PartError::InvalidGptHeader)?;

        let header_crc = crc32(checksum_data);
        write_u32(header, 16, header_crc)?;

        Ok(())
    }

    /// Adds a GPT partition to the primary GPT region.
    ///
    /// `sectors` must contain the primary 34-sector region, beginning at LBA 0.
    /// The first empty entry slot is selected. Both GPT CRC fields are
    /// recomputed after the modification.
    ///
    /// The supplied partition must fit within the usable LBA range and must
    /// not overlap an existing partition. The backup GPT is not modified;
    /// use [`backup_gpt`] to generate its contents.
    pub fn add_gpt_entry(
        sectors: &mut [u8],
        disk_sectors: u64,
        first_lba: u64,
        last_lba: u64,
        type_guid: [u8; 16],
        unique_guid: [u8; 16],
        name: &str,
    ) -> Result<(), PartError> {
        if sectors.len() < GPT_PRIMARY_SECTORS * SECTOR_SIZE {
            return Err(PartError::BufferTooSmall);
        }

        if type_guid.iter().all(|byte| *byte == 0) || unique_guid.iter().all(|byte| *byte == 0) {
            return Err(PartError::InvalidGuid);
        }

        if first_lba > last_lba {
            return Err(PartError::InvalidPartitionRange);
        }

        let info = match detect(sectors)? {
            Table::Gpt(info) => info,
            Table::Mbr(_) => return Err(PartError::InvalidGptSignature),
        };

        if info.alternate_lba
            != disk_sectors
                .checked_sub(1)
                .ok_or(PartError::InvalidBackupLocation)?
            || first_lba < info.first_usable_lba
            || last_lba > info.last_usable_lba
        {
            return Err(PartError::InvalidPartitionRange);
        }

        if info
            .entries
            .iter()
            .any(|entry| entry.first_lba <= last_lba && first_lba <= entry.last_lba)
        {
            return Err(PartError::OverlappingPartitions);
        }

        if info
            .entries
            .iter()
            .any(|entry| entry.unique_guid == unique_guid)
        {
            return Err(PartError::InvalidGuid);
        }

        let encoded_name: Vec<u16> = name.encode_utf16().collect();

        let name_capacity = usize::try_from(info.entry_size)
            .map_err(|_| PartError::InvalidEntryGeometry)?
            .checked_sub(56)
            .ok_or(PartError::InvalidEntryGeometry)?
            / 2;

        if encoded_name.len() > name_capacity {
            return Err(PartError::InvalidPartitionName);
        }

        let entries_offset = checked_byte_offset(info.entries_lba)?;
        let entry_size =
            usize::try_from(info.entry_size).map_err(|_| PartError::InvalidEntryGeometry)?;

        let entries_length = checked_entry_bytes(info.entry_count, info.entry_size)?;
        let entries_end = entries_offset
            .checked_add(entries_length)
            .ok_or(PartError::ArithmeticOverflow)?;

        let entries = sectors
            .get(entries_offset..entries_end)
            .ok_or(PartError::EntriesOutOfBounds)?;

        let mut empty_slot = None;

        for index in 0..info.entry_count {
            let offset = usize::try_from(index)
                .map_err(|_| PartError::ArithmeticOverflow)?
                .checked_mul(entry_size)
                .ok_or(PartError::ArithmeticOverflow)?;

            let slot_end = offset
                .checked_add(entry_size)
                .ok_or(PartError::ArithmeticOverflow)?;

            let slot = entries
                .get(offset..slot_end)
                .ok_or(PartError::EntriesOutOfBounds)?;

            let slot_type = read_guid(slot, 0)?;

            if slot_type.iter().all(|byte| *byte == 0) {
                empty_slot = Some(offset);
                break;
            }
        }

        let slot_offset = empty_slot.ok_or(PartError::NoFreeEntry)?;

        let absolute_offset = entries_offset
            .checked_add(slot_offset)
            .ok_or(PartError::ArithmeticOverflow)?;

        let absolute_end = absolute_offset
            .checked_add(entry_size)
            .ok_or(PartError::ArithmeticOverflow)?;

        let slot = sectors
            .get_mut(absolute_offset..absolute_end)
            .ok_or(PartError::EntriesOutOfBounds)?;

        slot.fill(0);

        write_guid(slot, 0, &type_guid)?;
        write_guid(slot, 16, &unique_guid)?;
        write_u64(slot, 32, first_lba)?;
        write_u64(slot, 40, last_lba)?;

        for (index, unit) in encoded_name.iter().enumerate() {
            let offset = 56_usize
                .checked_add(index.checked_mul(2).ok_or(PartError::ArithmeticOverflow)?)
                .ok_or(PartError::ArithmeticOverflow)?;

            let end = offset.checked_add(2).ok_or(PartError::ArithmeticOverflow)?;

            let destination = slot
                .get_mut(offset..end)
                .ok_or(PartError::InvalidPartitionName)?;

            destination.copy_from_slice(&unit.to_le_bytes());
        }

        update_gpt_crcs(sectors)
    }

    /// Generates backup GPT partition entries and a backup GPT header.
    ///
    /// `primary` must contain the complete primary 34-sector region.
    /// The returned tuple contains:
    ///
    /// 1. The backup partition-entry array, with the same size and contents
    ///    as the primary array.
    /// 2. A 512-byte backup header sector.
    ///
    /// The backup header has its current and alternate LBAs swapped, and its
    /// partition-entry array begins at `disk_sectors - 33`.
    pub fn backup_gpt(primary: &[u8], disk_sectors: u64) -> Result<(Vec<u8>, Vec<u8>), PartError> {
        let info = match detect(primary)? {
            Table::Gpt(info) => info,
            Table::Mbr(_) => return Err(PartError::InvalidGptSignature),
        };

        if disk_sectors < 34
            || info.alternate_lba
                != disk_sectors
                    .checked_sub(1)
                    .ok_or(PartError::InvalidBackupLocation)?
        {
            return Err(PartError::InvalidBackupLocation);
        }

        let backup_entries_lba = disk_sectors
            .checked_sub(33)
            .ok_or(PartError::InvalidBackupLocation)?;

        if info.last_usable_lba >= backup_entries_lba
            || info.first_usable_lba <= 1
            || info.my_lba != 1
        {
            return Err(PartError::InvalidBackupLocation);
        }

        let entries_length = checked_entry_bytes(info.entry_count, info.entry_size)?;
        let entries_offset = checked_byte_offset(info.entries_lba)?;
        let entries_end = entries_offset
            .checked_add(entries_length)
            .ok_or(PartError::ArithmeticOverflow)?;

        let entries = primary
            .get(entries_offset..entries_end)
            .ok_or(PartError::EntriesOutOfBounds)?
            .to_vec();

        let mut header = primary
            .get(SECTOR_SIZE..SECTOR_SIZE * 2)
            .ok_or(PartError::BufferTooSmall)?
            .to_vec();

        write_u64(&mut header, 24, info.alternate_lba)?;
        write_u64(&mut header, 32, info.my_lba)?;
        write_u64(&mut header, 72, backup_entries_lba)?;
        write_u32(&mut header, 88, crc32(&entries))?;
        write_u32(&mut header, 16, 0)?;

        let header_size =
            usize::try_from(read_u32(&header, 12)?).map_err(|_| PartError::InvalidGptHeader)?;

        if !(GPT_HEADER_MIN_SIZE..=SECTOR_SIZE).contains(&header_size) {
            return Err(PartError::InvalidGptHeader);
        }

        let header_crc = crc32(
            header
                .get(..header_size)
                .ok_or(PartError::InvalidGptHeader)?,
        );

        write_u32(&mut header, 16, header_crc)?;

        Ok((entries, header))
    }

    /// Parses a canonical GUID string into GPT mixed-endian bytes.
    ///
    /// The accepted format is `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
    #[must_use]
    pub fn guid_from_str(value: &str) -> Option<[u8; 16]> {
        let compact: String = value.chars().filter(|ch| *ch != '-').collect();

        if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }

        let mut canonical = [0_u8; 16];

        for (index, byte) in canonical.iter_mut().enumerate() {
            let offset = index.checked_mul(2)?;
            let end = offset.checked_add(2)?;
            *byte = u8::from_str_radix(compact.get(offset..end)?, 16).ok()?;
        }

        let mut guid = canonical;
        guid.get_mut(0..4)?.reverse();
        guid.get_mut(4..6)?.reverse();
        guid.get_mut(6..8)?.reverse();

        Some(guid)
    }

    /// Formats GPT mixed-endian GUID bytes as a canonical GUID string.
    #[must_use]
    pub fn guid_to_string(guid: &[u8; 16]) -> String {
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            guid[3],
            guid[2],
            guid[1],
            guid[0],
            guid[5],
            guid[4],
            guid[7],
            guid[6],
            guid[8],
            guid[9],
            guid[10],
            guid[11],
            guid[12],
            guid[13],
            guid[14],
            guid[15],
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const DISK_SECTORS: u64 = 100_000;
        const ENTRY_COUNT: u32 = 128;
        const ENTRY_SIZE: u32 = 128;

        fn put_u32(data: &mut [u8], offset: usize, value: u32) {
            if let Some(destination) = data.get_mut(offset..offset + 4) {
                destination.copy_from_slice(&value.to_le_bytes());
            }
        }

        fn put_u64(data: &mut [u8], offset: usize, value: u64) {
            if let Some(destination) = data.get_mut(offset..offset + 8) {
                destination.copy_from_slice(&value.to_le_bytes());
            }
        }

        fn put_guid(data: &mut [u8], offset: usize, value: &[u8; 16]) {
            if let Some(destination) = data.get_mut(offset..offset + 16) {
                destination.copy_from_slice(value);
            }
        }

        /// Recomputes the header CRC (and optionally the entries CRC) after a test edit.
        fn refresh_crcs(data: &mut [u8], entries_too: bool) {
            if entries_too {
                let entries_crc = crc32(
                    data.get(2 * SECTOR_SIZE..2 * SECTOR_SIZE + 128 * 128)
                        .unwrap_or_default(),
                );
                put_u32(data, SECTOR_SIZE + 88, entries_crc);
            }

            let header_size =
                usize::try_from(read_u32(data, SECTOR_SIZE + 12).unwrap_or(92)).unwrap_or(92);
            let header_size = header_size.clamp(GPT_HEADER_MIN_SIZE, SECTOR_SIZE);

            let mut header = data
                .get(SECTOR_SIZE..SECTOR_SIZE + header_size)
                .unwrap_or_default()
                .to_vec();

            if let Some(field) = header.get_mut(16..20) {
                field.fill(0);
            }

            let checksum = crc32(&header);
            put_u32(data, SECTOR_SIZE + 16, checksum);
        }

        fn make_gpt() -> Vec<u8> {
            let mut data = vec![0_u8; GPT_PRIMARY_SECTORS * SECTOR_SIZE];

            data[510] = 0x55;
            data[511] = 0xaa;

            data[MBR_ENTRY_OFFSET + 4] = 0xee;
            data[MBR_ENTRY_OFFSET + 8..MBR_ENTRY_OFFSET + 12].copy_from_slice(&1_u32.to_le_bytes());
            data[MBR_ENTRY_OFFSET + 12..MBR_ENTRY_OFFSET + 16]
                .copy_from_slice(&99_999_u32.to_le_bytes());

            let header_offset = SECTOR_SIZE;

            data[header_offset..header_offset + 8].copy_from_slice(GPT_SIGNATURE);

            put_u32(&mut data, header_offset + 8, 0x0001_0000);
            put_u32(&mut data, header_offset + 12, 92);
            put_u64(&mut data, header_offset + 24, 1);
            put_u64(&mut data, header_offset + 32, DISK_SECTORS - 1);
            put_u64(&mut data, header_offset + 40, 2048);
            put_u64(&mut data, header_offset + 48, DISK_SECTORS - 34);

            put_guid(
                &mut data,
                header_offset + 56,
                &guid_from_str("12345678-1234-5678-9abc-def012345678").unwrap_or([0; 16]),
            );

            put_u64(&mut data, header_offset + 72, 2);
            put_u32(&mut data, header_offset + 80, ENTRY_COUNT);
            put_u32(&mut data, header_offset + 84, ENTRY_SIZE);

            let entries_offset = 2 * SECTOR_SIZE;

            for (index, (first, last, unique)) in [
                (2048_u64, 4095_u64, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
                (4096_u64, 8191_u64, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
            ]
            .iter()
            .enumerate()
            {
                let offset = entries_offset + index * 128;

                put_guid(&mut data, offset, &LINUX_FILESYSTEM_GUID);

                let guid = guid_from_str(unique).unwrap_or([0; 16]);
                put_guid(&mut data, offset + 16, &guid);

                put_u64(&mut data, offset + 32, *first);
                put_u64(&mut data, offset + 40, *last);

                let name = format!("Linux{}", index + 1);

                for (unit_index, unit) in name.encode_utf16().enumerate() {
                    let name_offset = offset + 56 + unit_index * 2;
                    data[name_offset..name_offset + 2].copy_from_slice(&unit.to_le_bytes());
                }
            }

            refresh_crcs(&mut data, true);

            data
        }

        fn valid_mbr() -> Vec<u8> {
            let mut data = vec![0_u8; SECTOR_SIZE];

            data[510] = 0x55;
            data[511] = 0xaa;

            for index in 0..4_usize {
                let offset = MBR_ENTRY_OFFSET + index * MBR_ENTRY_SIZE;

                data[offset] = if index == 0 { 0x80 } else { 0 };
                data[offset + 4] = u8::try_from(index + 1).unwrap_or(0);

                data[offset + 8..offset + 12].copy_from_slice(
                    &(2048_u32 * u32::try_from(index + 1).unwrap_or(0)).to_le_bytes(),
                );

                data[offset + 12..offset + 16].copy_from_slice(&2048_u32.to_le_bytes());
            }

            data
        }

        #[test]
        fn crc32_known_vectors() {
            assert_eq!(crc32(b""), 0);
            assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
            assert_eq!(
                crc32(b"The quick brown fox jumps over the lazy dog"),
                0x414f_a339
            );
        }

        #[test]
        fn detect_valid_gpt() {
            let data = make_gpt();

            let table = detect(&data);

            assert!(table.is_ok());

            let Ok(Table::Gpt(info)) = table else {
                return;
            };

            assert_eq!(info.my_lba, 1);
            assert_eq!(info.alternate_lba, DISK_SECTORS - 1);
            assert_eq!(info.first_usable_lba, 2048);
            assert_eq!(info.entries.len(), 2);
            assert_eq!(info.entries[0].name, "Linux1");
            assert_eq!(info.entries[1].name, "Linux2");
        }

        #[test]
        fn detect_valid_mbr_with_four_entries() {
            let data = valid_mbr();

            let table = detect(&data);

            assert!(matches!(table, Ok(Table::Mbr(entries)) if entries.len() == 4));
        }

        #[test]
        fn detect_rejects_bad_mbr_signature() {
            let mut data = valid_mbr();

            data[510] = 0;
            data[511] = 0;

            assert_eq!(detect(&data), Err(PartError::InvalidMbrSignature));
        }

        #[test]
        fn detect_rejects_truncated_buffers() {
            assert_eq!(detect(&[]), Err(PartError::BufferTooSmall));

            assert_eq!(
                detect(&vec![0; SECTOR_SIZE - 1]),
                Err(PartError::BufferTooSmall)
            );

            let data = make_gpt();

            assert_eq!(
                detect(data.get(..SECTOR_SIZE * 33).unwrap_or_default()),
                Err(PartError::BufferTooSmall)
            );
        }

        #[test]
        fn detect_rejects_corrupted_header_crc() {
            let mut data = make_gpt();

            data[SECTOR_SIZE + 16] ^= 0x01;

            assert_eq!(detect(&data), Err(PartError::HeaderCrcMismatch));
        }

        #[test]
        fn detect_rejects_corrupted_entries_crc() {
            let mut data = make_gpt();

            data[SECTOR_SIZE + 88] ^= 0x01;
            refresh_crcs(&mut data, false);

            assert_eq!(detect(&data), Err(PartError::EntriesCrcMismatch));
        }

        #[test]
        fn detect_rejects_corrupted_entry_data() {
            let mut data = make_gpt();

            data[2 * SECTOR_SIZE + 32] ^= 0x01;

            assert_eq!(detect(&data), Err(PartError::EntriesCrcMismatch));
        }

        #[test]
        fn detect_rejects_invalid_header_fields() {
            let cases = [
                (12_usize, 0_u32),
                (12, 513),
                (80, 0),
                (80, 257),
                (84, 127),
                (84, 513),
                (84, 130),
            ];

            for (offset, value) in cases {
                let mut data = make_gpt();

                put_u32(&mut data, SECTOR_SIZE + offset, value);
                refresh_crcs(&mut data, false);

                assert!(detect(&data).is_err(), "offset {offset}, value {value}");
            }
        }

        #[test]
        fn detect_rejects_invalid_lba_fields() {
            let cases = [
                (24_usize, 2_u64),
                (32, 1),
                (40, 9000),
                (48, 1000),
                (72, 1000),
            ];

            for (offset, value) in cases {
                let mut data = make_gpt();

                put_u64(&mut data, SECTOR_SIZE + offset, value);
                refresh_crcs(&mut data, false);

                assert!(detect(&data).is_err(), "offset {offset}");
            }
        }

        #[test]
        fn detect_rejects_overlapping_entries() {
            let mut data = make_gpt();

            put_u64(&mut data, 2 * SECTOR_SIZE + 128 + 32, 4000);
            refresh_crcs(&mut data, true);

            assert_eq!(detect(&data), Err(PartError::OverlappingPartitions));
        }

        #[test]
        fn detect_rejects_invalid_partition_range() {
            let mut data = make_gpt();

            put_u64(&mut data, 2 * SECTOR_SIZE + 32, 8192);
            refresh_crcs(&mut data, true);

            assert_eq!(detect(&data), Err(PartError::InvalidPartitionRange));
        }

        #[test]
        fn add_entry_and_reparse() {
            let mut data = make_gpt();

            let result = add_gpt_entry(
                &mut data,
                DISK_SECTORS,
                8192,
                12_287,
                EFI_SYSTEM_GUID,
                guid_from_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap_or([0; 16]),
                "EFI System",
            );

            assert!(result.is_ok());

            let parsed = detect(&data);

            assert!(matches!(
                parsed,
                Ok(Table::Gpt(info))
                    if info.entries.len() == 3
                        && info.entries.iter().any(|entry| entry.name == "EFI System")
            ));
        }

        #[test]
        fn add_entry_rejects_overlapping_partition() {
            let mut data = make_gpt();

            assert_eq!(
                add_gpt_entry(
                    &mut data,
                    DISK_SECTORS,
                    3000,
                    5000,
                    EFI_SYSTEM_GUID,
                    guid_from_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap_or([0; 16]),
                    "Overlap",
                ),
                Err(PartError::OverlappingPartitions)
            );
        }

        #[test]
        fn add_entry_rejects_invalid_guids_and_names() {
            let mut data = make_gpt();

            assert_eq!(
                add_gpt_entry(
                    &mut data,
                    DISK_SECTORS,
                    8192,
                    12_287,
                    [0; 16],
                    [1; 16],
                    "Invalid",
                ),
                Err(PartError::InvalidGuid)
            );

            assert_eq!(
                add_gpt_entry(
                    &mut data,
                    DISK_SECTORS,
                    8192,
                    12_287,
                    EFI_SYSTEM_GUID,
                    [0xcc; 16],
                    &"A".repeat(37),
                ),
                Err(PartError::InvalidPartitionName)
            );
        }

        #[test]
        fn add_entry_rejects_duplicate_unique_guid() {
            let mut data = make_gpt();

            let duplicate =
                guid_from_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap_or([0; 16]);

            assert_eq!(
                add_gpt_entry(
                    &mut data,
                    DISK_SECTORS,
                    8192,
                    12_287,
                    EFI_SYSTEM_GUID,
                    duplicate,
                    "Duplicate",
                ),
                Err(PartError::InvalidGuid)
            );
        }

        #[test]
        fn backup_header_consistency() {
            let primary = make_gpt();

            let result = backup_gpt(&primary, DISK_SECTORS);

            assert!(result.is_ok());

            let Ok((entries, header)) = result else {
                return;
            };

            assert_eq!(entries.len(), 128 * 128);
            assert_eq!(header.len(), SECTOR_SIZE);

            assert_eq!(read_u64(&header, 24), Ok(DISK_SECTORS - 1));
            assert_eq!(read_u64(&header, 32), Ok(1));
            assert_eq!(read_u64(&header, 72), Ok(DISK_SECTORS - 33));

            assert_eq!(read_u32(&header, 88), Ok(crc32(&entries)));

            let expected_crc = read_u32(&header, 16).unwrap_or(0);

            let mut copy = header.clone();
            put_u32(&mut copy, 16, 0);

            assert_eq!(crc32(copy.get(..92).unwrap_or_default()), expected_crc);
        }

        #[test]
        fn backup_rejects_invalid_disk_size() {
            let primary = make_gpt();

            assert_eq!(
                backup_gpt(&primary, 33),
                Err(PartError::InvalidBackupLocation)
            );
        }

        #[test]
        fn largest_gpt_free_gap() {
            let data = make_gpt();

            let Ok(Table::Gpt(info)) = detect(&data) else {
                return;
            };

            let gap = largest_free_gap(&Table::Gpt(info), DISK_SECTORS);

            // Free space runs from LBA 8192 to the last usable LBA (DISK_SECTORS - 34).
            assert_eq!(gap, Some((8192, DISK_SECTORS - 33 - 8192)));
        }

        #[test]
        fn largest_mbr_free_gap() {
            let data = valid_mbr();

            let Ok(Table::Mbr(entries)) = detect(&data) else {
                return;
            };

            let gap = largest_free_gap(&Table::Mbr(entries), 100_000);

            assert_eq!(gap, Some((10_240, 89_760)));
        }

        #[test]
        fn guid_linux_filesystem_round_trip() {
            let value = "0fc63daf-8483-4772-8e79-3d69d8477de4";

            let guid = guid_from_str(value);

            assert_eq!(guid, Some(LINUX_FILESYSTEM_GUID));

            if let Some(guid) = guid {
                assert_eq!(guid_to_string(&guid), value);
            }
        }

        #[test]
        fn guid_efi_system_round_trip() {
            let value = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";

            let guid = guid_from_str(value);

            assert_eq!(guid, Some(EFI_SYSTEM_GUID));

            if let Some(guid) = guid {
                assert_eq!(guid_to_string(&guid), value);
            }
        }

        #[test]
        fn guid_rejects_malformed_values() {
            assert_eq!(guid_from_str(""), None);
            assert_eq!(guid_from_str("invalid"), None);
            assert_eq!(guid_from_str("0fc63daf"), None);
            assert_eq!(guid_from_str("0fc63daf-8483-4772"), None);
            assert_eq!(guid_from_str("zzzzzzzz-8483-4772-8e79-3d69d8477de4"), None);
        }

        #[test]
        fn malformed_buffers_never_panic() {
            for length in 0..GPT_PRIMARY_SECTORS * SECTOR_SIZE {
                let data = vec![0_u8; length];

                assert!(detect(&data).is_err());
                assert!(backup_gpt(&data, DISK_SECTORS).is_err());
            }

            let mut data = vec![0_u8; GPT_PRIMARY_SECTORS * SECTOR_SIZE];

            assert!(add_gpt_entry(
                &mut data,
                DISK_SECTORS,
                2048,
                4095,
                EFI_SYSTEM_GUID,
                [1; 16],
                "Test",
            )
            .is_err());
        }

        #[test]
        fn free_gap_rejects_too_small_disk() {
            assert_eq!(largest_free_gap(&Table::Mbr(Vec::new()), 1000), None);
        }
    }
}

pub mod procparse {
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
            let input =
                r#"BOOT_IMAGE=/boot/bzImage quiet loglevel=3 mouli.name="my box" -- ignored"#;

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
}

pub mod hwprobe {
    //! Reads the machine's hardware facts from `/proc` and `/sys` and turns them into the plain-text
    //! hardware report ([`crate::hwreport`]) shown on the `i` key and saved to the data partition.
    //!
    //! Both roots are parameters so the whole thing can be tested against a fake tree.

    use crate::hwreport::{build_with_classes, Inputs};
    use crate::procparse::{is_whole_disk, parse_partitions, parse_pci_id, parse_uevent};
    use std::path::Path;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    fn dir_names(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// PCI `(vendor, device)` pairs and class codes from `<sys>/bus/pci/devices/*/uevent`.
    fn pci_devices(sys: &Path) -> (Vec<(u16, u16)>, Vec<u32>) {
        let base = sys.join("bus/pci/devices");
        dir_names(&base)
            .iter()
            .filter_map(|d| {
                let ev = parse_uevent(&read(&base.join(d).join("uevent")));
                let ids = parse_pci_id(ev.get("PCI_ID")?)?;
                let class = ev
                    .get("PCI_CLASS")
                    .and_then(|c| u32::from_str_radix(c.trim(), 16).ok())
                    .unwrap_or(0);
                Some((ids, class))
            })
            .unzip()
    }

    /// The report for the machine whose `/proc` and `/sys` are at `proc` and `sys`.
    #[must_use]
    pub fn report(proc: &Path, sys: &Path) -> String {
        let cpuinfo = read(&proc.join("cpuinfo"));
        let meminfo = read(&proc.join("meminfo"));
        let dmi = sys.join("class/dmi/id");
        let (vendor, product, bios) = (
            read(&dmi.join("sys_vendor")),
            read(&dmi.join("product_name")),
            read(&dmi.join("bios_version")),
        );
        let (pci, classes) = pci_devices(sys);
        let nets = dir_names(&sys.join("class/net"));
        let net_refs: Vec<&str> = nets.iter().map(String::as_str).collect();
        let disks: Vec<String> = parse_partitions(&read(&proc.join("partitions")))
            .into_iter()
            .filter(|p| is_whole_disk(&p.name))
            .map(|p| p.name)
            .collect();
        let disk_refs: Vec<&str> = disks.iter().map(String::as_str).collect();
        build_with_classes(
            &Inputs {
                cpuinfo: &cpuinfo,
                meminfo: &meminfo,
                dmi_vendor: &vendor,
                dmi_product: &product,
                dmi_bios: &bios,
                pci_ids: &pci,
                net_interfaces: &net_refs,
                block_devices: &disk_refs,
            },
            &classes,
        )
        .render()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn put(root: &Path, rel: &str, text: &str) {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }

        fn fake(name: &str) -> std::path::PathBuf {
            let d =
                std::env::temp_dir().join(format!("mouli-hwprobe-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        #[test]
        fn reads_a_fake_machine() {
            let root = fake("machine");
            let (proc, sys) = (root.join("proc"), root.join("sys"));
            put(
                &proc,
                "cpuinfo",
                "processor\t: 0\nmodel name\t: Test CPU @ 3.00GHz\nflags\t\t: fpu avx avx2\n",
            );
            put(
                &proc,
                "meminfo",
                "MemTotal:       8000000 kB\nMemAvailable:   6000000 kB\n",
            );
            put(
                &proc,
                "partitions",
                "major minor  #blocks  name\n\n   8        0  976762584 sda\n   8        1     524288 sda1\n",
            );
            put(&sys, "class/dmi/id/sys_vendor", "ACME\n");
            put(&sys, "class/dmi/id/product_name", "Laptop 9\n");
            put(&sys, "class/dmi/id/bios_version", "F.12\n");
            put(&sys, "class/net/eth0/type", "1\n");
            put(&sys, "class/net/lo/type", "772\n");
            put(
                &sys,
                "bus/pci/devices/0000:00:14.3/uevent",
                "DRIVER=iwlwifi\nPCI_CLASS=28000\nPCI_ID=8086:2725\n",
            );
            let text = report(&proc, &sys);
            assert!(text.contains("Hardware report"));
            assert!(text.contains("ACME") && text.contains("Laptop 9"), "{text}");
            assert!(text.contains("sda"), "{text}");
            assert!(
                !text.contains("sda1"),
                "partitions are not listed as disks: {text}"
            );
            assert!(text.contains("eth0"), "{text}");
            assert!(text.contains("8086:2725"), "{text}");
            assert!(text.lines().all(|l| l.chars().count() <= 60), "{text}");
        }

        #[test]
        fn an_empty_tree_still_gives_a_report() {
            let root = fake("empty");
            let text = report(&root.join("proc"), &root.join("sys"));
            assert!(text.starts_with("Hardware report"));
        }
    }
}

pub mod hwreport {
    //! hwreport turns Linux hardware text snapshots into a compact plain-text report.
    //!
    //! All inputs are supplied by the caller; this module never reads files and never
    //! performs I/O. The rendered report is intended to be photocopied, photographed,
    //! or pasted into a support request, so every rendered line is kept under 60 columns.

    /// Raw text and id snapshots supplied by the caller.
    pub struct Inputs<'a> {
        /// Contents of `/proc/cpuinfo`.
        pub cpuinfo: &'a str,
        /// Contents of `/proc/meminfo`.
        pub meminfo: &'a str,
        /// DMI vendor string, for example from product or sysfs DMI data.
        pub dmi_vendor: &'a str,
        /// DMI product name string.
        pub dmi_product: &'a str,
        /// DMI BIOS version string.
        pub dmi_bios: &'a str,
        /// PCI vendor/device pairs as `(vendor, device)`.
        pub pci_ids: &'a [(u16, u16)],
        /// Network interface names such as `eth0`, `wlan0`, or `lo`.
        pub net_interfaces: &'a [&'a str],
        /// Block device names such as `sda`, `nvme0n1`, or `/dev/sda`.
        pub block_devices: &'a [&'a str],
    }

    /// Hardware support class for a PCI device.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Class {
        /// Wireless network adapter.
        WiFi,
        /// Wired Ethernet adapter.
        Ethernet,
        /// Graphics processor.
        Gpu,
        /// Storage controller or adapter.
        Storage,
        /// Anything else, including fallback matches.
        Other,
    }

    /// Expected local software support for a device.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Support {
        /// The device is expected to work.
        Yes,
        /// The device is not expected to work yet, with a static reason.
        No(&'static str),
        /// No confident support statement is available.
        Unknown,
    }

    /// Parsed hardware report ready to render.
    pub struct Report {
        cpu: CpuReport,
        memory: MemoryReport,
        dmi: DmiReport,
        pci: Vec<PciReport>,
        net: NetReport,
        block_devices: Vec<String>,
        verdict: String,
    }

    impl Report {
        /// Render the report as plain text with no line wider than 60 columns.
        #[must_use]
        pub fn render(&self) -> String {
            let mut out = String::new();
            out.push_str("Hardware report\n");
            out.push_str("===============\n");

            out.push_str("CPU\n");
            emit_kv(&mut out, "  ", "Model", &self.cpu.model);
            emit_kv(
                &mut out,
                "  ",
                "Logical CPUs",
                &self.cpu.logical_cpus.to_string(),
            );
            emit_kv(&mut out, "  ", "AVX2", on_off(self.cpu.avx2));
            emit_kv(&mut out, "  ", "AVX512F", on_off(self.cpu.avx512f));
            emit_kv(&mut out, "  ", "AES-NI", on_off(self.cpu.aes));
            emit_kv(&mut out, "  ", "SSE4.2", on_off(self.cpu.sse4_2));

            out.push_str("Memory\n");
            emit_kv(&mut out, "  ", "MemTotal", &self.memory.total);
            emit_kv(&mut out, "  ", "MemAvailable", &self.memory.available);

            out.push_str("Firmware\n");
            emit_kv(&mut out, "  ", "Vendor", &self.dmi.vendor);
            emit_kv(&mut out, "  ", "Product", &self.dmi.product);
            emit_kv(&mut out, "  ", "BIOS", &self.dmi.bios);

            out.push_str("PCI devices\n");
            if self.pci.is_empty() {
                out.push_str("  none supplied\n");
            } else {
                for device in &self.pci {
                    push_wrapped(&mut out, "  ", &format_pci(device));
                }
            }

            out.push_str("Network interfaces\n");
            emit_kv(&mut out, "  ", "Wired", &join_or_none(&self.net.wired));
            emit_kv(
                &mut out,
                "  ",
                "Wireless",
                &join_or_none(&self.net.wireless),
            );
            emit_kv(&mut out, "  ", "Other", &join_or_none(&self.net.other));

            out.push_str("Block devices\n");
            if self.block_devices.is_empty() {
                out.push_str("  none supplied\n");
            } else {
                push_wrapped(&mut out, "  ", &self.block_devices.join(", "));
            }

            push_wrapped(&mut out, "", &format!("Verdict: {}", self.verdict));
            out
        }
    }

    const MAX_WIDTH: usize = 60;

    type KnownEntry = (u16, u16, &'static str, Class, Support);

    const NO_FIRMWARE: Support = Support::No("needs proprietary firmware");
    const NO_GPU: Support = Support::No("no GPU acceleration yet; runs on the CPU");

    const KNOWN: &[KnownEntry] = &[
        (
            0x8086,
            0x2723,
            "Intel Wi-Fi 6 AX201",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x8086,
            0x2725,
            "Intel Wi-Fi 6E AX210/AX211",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x8086,
            0x24f3,
            "Intel Wireless-AC 8265",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x8086,
            0x095a,
            "Intel Wireless-AC 7265",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x8086,
            0x08b1,
            "Intel Wireless 7260",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x8086,
            0x4232,
            "Intel PRO/Wireless 5100",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x8086,
            0x100e,
            "Intel 82540EM Gigabit Ethernet",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x8086,
            0x10d3,
            "Intel 82574L Gigabit Ethernet",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x8086,
            0x1502,
            "Intel 82579LM Gigabit Ethernet",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x8086,
            0x1533,
            "Intel I210 Gigabit Ethernet",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x8086,
            0x15b8,
            "Intel I219-V Ethernet",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x8086,
            0x15fa,
            "Intel I219-LM Ethernet",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x8086,
            0x10f5,
            "Intel 82599ES 10GbE",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x8086,
            0x9d03,
            "Intel SATA AHCI",
            Class::Storage,
            Support::Unknown,
        ),
        (
            0x8086,
            0x9c03,
            "Intel Lynx Point SATA AHCI",
            Class::Storage,
            Support::Unknown,
        ),
        (
            0x8086,
            0x2822,
            "Intel ICH9 SATA",
            Class::Storage,
            Support::Unknown,
        ),
        (
            0x10ec,
            0x8168,
            "Realtek RTL8168/8111 PCIe GbE",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x10ec,
            0x8139,
            "Realtek RTL8139 Fast Ethernet",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x10ec,
            0x8125,
            "Realtek RTL8125 2.5GbE",
            Class::Ethernet,
            Support::Yes,
        ),
        (
            0x10ec,
            0x8821,
            "Realtek RTL8821AE WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x10ec,
            0xb822,
            "Realtek RTL8822BE WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x10ec,
            0xb723,
            "Realtek RTL8723BE WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x10ec,
            0xc821,
            "Realtek RTL8821CE WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x14c3,
            0x7961,
            "MediaTek MT7921 Wi-Fi 6",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x14c3,
            0x7922,
            "MediaTek MT7922 Wi-Fi 6E",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x168c,
            0x0024,
            "Atheros AR5418/AR5008 WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x168c,
            0x0030,
            "Atheros AR938x WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x168c,
            0x0036,
            "Atheros AR9485 WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x168c,
            0x003e,
            "Qualcomm Atheros QCA6174 WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x168c,
            0x0042,
            "Qualcomm Atheros QCA9377 WiFi",
            Class::WiFi,
            Support::Yes,
        ),
        (
            0x14e4,
            0x432b,
            "Broadcom BCM4322 WiFi",
            Class::WiFi,
            NO_FIRMWARE,
        ),
        (
            0x14e4,
            0x4353,
            "Broadcom BCM43224 WiFi",
            Class::WiFi,
            NO_FIRMWARE,
        ),
        (
            0x14e4,
            0x43a0,
            "Broadcom BCM4360 WiFi",
            Class::WiFi,
            NO_FIRMWARE,
        ),
        (
            0x14e4,
            0x43a3,
            "Broadcom BCM43602 WiFi",
            Class::WiFi,
            NO_FIRMWARE,
        ),
        (
            0x14e4,
            0x43b1,
            "Broadcom BCM4352 WiFi",
            Class::WiFi,
            NO_FIRMWARE,
        ),
        (0x10de, 0x1380, "NVIDIA Maxwell-era GPU", Class::Gpu, NO_GPU),
        (0x10de, 0x1b81, "NVIDIA Pascal-era GPU", Class::Gpu, NO_GPU),
        (0x10de, 0x1b82, "NVIDIA Pascal-era GPU", Class::Gpu, NO_GPU),
        (
            0x10de,
            0x1c03,
            "NVIDIA Pascal mobile GPU",
            Class::Gpu,
            NO_GPU,
        ),
        (0x1002, 0x67df, "AMD Radeon Polaris GPU", Class::Gpu, NO_GPU),
        (0x1002, 0x67ef, "AMD Radeon Polaris GPU", Class::Gpu, NO_GPU),
        (0x1002, 0x687f, "AMD Radeon Vega GPU", Class::Gpu, NO_GPU),
        (0x1002, 0x731f, "AMD Radeon Navi GPU", Class::Gpu, NO_GPU),
    ];

    const KNOWN_VENDORS: &[(u16, &str)] = &[
        (0x8086, "Intel"),
        (0x10ec, "Realtek"),
        (0x14c3, "MediaTek"),
        (0x168c, "Atheros/Qualcomm"),
        (0x14e4, "Broadcom"),
        (0x10de, "NVIDIA"),
        (0x1002, "AMD"),
    ];

    const DMI_PLACEHOLDERS: &[&str] = &[
        "to be filled by o.e.m.",
        "default string",
        "system product name",
        "not specified",
    ];

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct CpuReport {
        model: String,
        logical_cpus: usize,
        avx2: bool,
        avx512f: bool,
        aes: bool,
        sse4_2: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct MemoryReport {
        total: String,
        available: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct DmiReport {
        vendor: String,
        product: String,
        bios: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct PciReport {
        vendor: u16,
        device: u16,
        name: String,
        class: Class,
        support: Support,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct NetReport {
        wired: Vec<String>,
        wireless: Vec<String>,
        other: Vec<String>,
    }

    /// Build a report from caller-supplied hardware text and identifiers.
    #[must_use]
    pub fn build(i: &Inputs) -> Report {
        build_with_classes(i, &[])
    }

    /// Like [`build`], with the PCI class code (`PCI_CLASS` in sysfs, for example `0x028000`) of each
    /// entry of `i.pci_ids`, in the same order. Devices missing from the built-in table are then
    /// described by what they are ("Intel wireless network adapter") instead of "unknown device".
    /// A shorter or empty slice is fine; the missing entries stay unknown.
    #[must_use]
    pub fn build_with_classes(i: &Inputs, classes: &[u32]) -> Report {
        let cpu = parse_cpuinfo(i.cpuinfo);
        let memory = parse_meminfo(i.meminfo);
        let dmi = DmiReport {
            vendor: clean_dmi(i.dmi_vendor),
            product: clean_dmi(i.dmi_product),
            bios: clean_dmi(i.dmi_bios),
        };
        let pci: Vec<PciReport> = i
            .pci_ids
            .iter()
            .enumerate()
            .map(|(n, &(v, d))| identify_pci(v, d, classes.get(n).copied()))
            .collect();
        let net = group_net_interfaces(i.net_interfaces);
        let block_devices = i
            .block_devices
            .iter()
            .filter_map(|name| {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .collect();
        let verdict = choose_verdict(&cpu, &pci);

        Report {
            cpu,
            memory,
            dmi,
            pci,
            net,
            block_devices,
            verdict,
        }
    }

    fn parse_cpuinfo(text: &str) -> CpuReport {
        let mut model = None;
        let mut logical_cpus = 0usize;
        let mut avx2 = false;
        let mut avx512f = false;
        let mut aes = false;
        let mut sse4_2 = false;

        for line in text.lines() {
            let Some((raw_key, value)) = line.split_once(':') else {
                continue;
            };
            let key = raw_key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "processor" => logical_cpus += 1,
                "model name" => {
                    if model.is_none() && !value.is_empty() {
                        model = Some(value.to_string());
                    }
                }
                "flags" | "features" => {
                    for token in value.split_whitespace() {
                        match token.to_ascii_lowercase().as_str() {
                            "avx2" => avx2 = true,
                            "avx512f" => avx512f = true,
                            "aes" => aes = true,
                            "sse4_2" => sse4_2 = true,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        CpuReport {
            model: model.unwrap_or_else(|| "unknown".to_string()),
            logical_cpus,
            avx2,
            avx512f,
            aes,
            sse4_2,
        }
    }

    fn parse_meminfo(text: &str) -> MemoryReport {
        let mut total_kib = None;
        let mut available_kib = None;

        for line in text.lines() {
            let Some((raw_key, value)) = line.split_once(':') else {
                continue;
            };
            let key = raw_key.trim();
            let parsed = value
                .split_whitespace()
                .next()
                .and_then(|first| first.parse::<u64>().ok());
            if key.eq_ignore_ascii_case("MemTotal") {
                total_kib = parsed;
            } else if key.eq_ignore_ascii_case("MemAvailable") {
                available_kib = parsed;
            }
        }

        MemoryReport {
            total: format_gib(total_kib),
            available: format_gib(available_kib),
        }
    }

    fn format_gib(kib: Option<u64>) -> String {
        match kib {
            Some(value) => format!("{:.1} GiB", value as f64 / 1_048_576.0),
            None => "unknown".to_string(),
        }
    }

    fn clean_dmi(value: &str) -> String {
        let trimmed = value.trim();
        let normalized = trimmed.to_ascii_lowercase();
        if trimmed.is_empty() || DMI_PLACEHOLDERS.contains(&normalized.as_str()) {
            "unknown".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn identify_pci(vendor: u16, device: u16, class_code: Option<u32>) -> PciReport {
        if let Some(&(_, _, name, class, support)) =
            KNOWN
                .iter()
                .find(|&&(known_vendor, known_device, _, _, _)| {
                    known_vendor == vendor && known_device == device
                })
        {
            return PciReport {
                vendor,
                device,
                name: name.to_string(),
                class,
                support,
            };
        }

        let vendor_label = vendor_name(vendor).unwrap_or("unknown vendor");
        if let Some((kind, class, support)) = class_code.and_then(generic_by_class) {
            return PciReport {
                vendor,
                device,
                name: format!("{vendor_label} {kind}"),
                class,
                support,
            };
        }

        if let Some(name) = vendor_name(vendor) {
            return PciReport {
                vendor,
                device,
                name: format!("{name} unknown device"),
                class: Class::Other,
                support: Support::Unknown,
            };
        }

        PciReport {
            vendor,
            device,
            name: "unknown vendor".to_string(),
            class: Class::Other,
            support: Support::Unknown,
        }
    }

    /// What a device is, from its PCI class code (`0xBBSSPP`: base class, subclass, interface).
    fn generic_by_class(code: u32) -> Option<(&'static str, Class, Support)> {
        let base = (code >> 16) & 0xff;
        let sub = (code >> 8) & 0xff;
        let (kind, class, support) = match (base, sub) {
            (0x01, 0x08) => ("NVMe storage controller", Class::Storage, Support::Unknown),
            (0x01, 0x06) => ("SATA storage controller", Class::Storage, Support::Unknown),
            (0x01, _) => ("storage controller", Class::Storage, Support::Unknown),
            (0x02, 0x00) => ("Ethernet adapter", Class::Ethernet, Support::Unknown),
            (0x02, 0x80) => ("wireless network adapter", Class::WiFi, Support::Unknown),
            (0x02, _) => ("network adapter", Class::Other, Support::Unknown),
            (0x03, _) => ("graphics adapter", Class::Gpu, NO_GPU),
            (0x04, _) => ("audio or video device", Class::Other, Support::Unknown),
            (0x05, _) => ("memory controller", Class::Other, Support::Unknown),
            (0x06, 0x00) => ("host bridge", Class::Other, Support::Unknown),
            (0x06, 0x01) => ("ISA bridge", Class::Other, Support::Unknown),
            (0x06, _) => ("PCI bridge", Class::Other, Support::Unknown),
            (0x07, _) => ("communication controller", Class::Other, Support::Unknown),
            (0x08, _) => ("system peripheral", Class::Other, Support::Unknown),
            (0x0c, 0x03) => ("USB controller", Class::Other, Support::Unknown),
            (0x0c, 0x05) => ("SMBus controller", Class::Other, Support::Unknown),
            (0x0c, _) => ("serial bus controller", Class::Other, Support::Unknown),
            (0x0d, _) => ("wireless controller", Class::Other, Support::Unknown),
            (0x10, _) => ("encryption controller", Class::Other, Support::Unknown),
            (0x11, _) => (
                "signal processing controller",
                Class::Other,
                Support::Unknown,
            ),
            _ => return None,
        };
        Some((kind, class, support))
    }

    fn vendor_name(vendor: u16) -> Option<&'static str> {
        KNOWN_VENDORS
            .iter()
            .find(|&&(known_vendor, _)| known_vendor == vendor)
            .map(|&(_, name)| name)
    }

    fn group_net_interfaces(interfaces: &[&str]) -> NetReport {
        let mut report = NetReport {
            wired: Vec::new(),
            wireless: Vec::new(),
            other: Vec::new(),
        };

        for interface in interfaces {
            let trimmed = interface.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("wlan") || lower.starts_with("wl") {
                report.wireless.push(trimmed.to_string());
            } else if lower.starts_with("eth") || lower.starts_with("en") {
                report.wired.push(trimmed.to_string());
            } else {
                report.other.push(trimmed.to_string());
            }
        }

        report
    }

    fn choose_verdict(cpu: &CpuReport, pci: &[PciReport]) -> String {
        if !cpu.avx2 {
            return "Too old: needs AVX2".to_string();
        }
        let has_unsupported_wifi = pci
            .iter()
            .any(|device| device.class == Class::WiFi && matches!(device.support, Support::No(_)));
        if has_unsupported_wifi {
            return "Ready, but no Wi-Fi support for your adapter".to_string();
        }
        "Ready".to_string()
    }

    fn on_off(value: bool) -> &'static str {
        if value {
            "yes"
        } else {
            "NO"
        }
    }

    fn class_label(class: Class) -> &'static str {
        match class {
            Class::WiFi => "WiFi",
            Class::Ethernet => "Ethernet",
            Class::Gpu => "GPU",
            Class::Storage => "Storage",
            Class::Other => "Other",
        }
    }

    fn support_label(support: Support) -> String {
        match support {
            Support::Yes => "supported".to_string(),
            Support::No(reason) => format!("not supported: {reason}"),
            Support::Unknown => "support unknown".to_string(),
        }
    }

    fn format_pci(device: &PciReport) -> String {
        let raw = format!("{:04x}:{:04x}", device.vendor, device.device);
        format!(
            "{raw} {} [{}; {}]",
            device.name,
            class_label(device.class),
            support_label(device.support)
        )
    }

    fn join_or_none(values: &[String]) -> String {
        if values.is_empty() {
            "none".to_string()
        } else {
            values.join(", ")
        }
    }

    fn flush_line(out: &mut String, indent: &str, line: &mut String) {
        if line.is_empty() {
            return;
        }
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
        line.clear();
    }

    fn push_wrapped(out: &mut String, indent: &str, text: &str) {
        let indent_width = indent.chars().count();
        let max_line = MAX_WIDTH.saturating_sub(indent_width).max(1);
        let mut line = String::new();

        for word in text.split_whitespace() {
            let word_width = word.chars().count();
            if word_width > max_line {
                flush_line(out, indent, &mut line);
                let chars: Vec<char> = word.chars().collect();
                for chunk in chars.chunks(max_line) {
                    out.push_str(indent);
                    out.extend(chunk);
                    out.push('\n');
                }
                continue;
            }

            let added = if line.is_empty() {
                word_width
            } else {
                word_width + 1
            };
            if line.chars().count() + added > max_line {
                flush_line(out, indent, &mut line);
                line.push_str(word);
            } else {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
        }

        flush_line(out, indent, &mut line);
    }

    fn emit_kv(out: &mut String, indent: &str, key: &str, value: &str) {
        let text = format!("{key}: {value}");
        push_wrapped(out, indent, &text);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const INTEL_CPUINFO: &str = "processor : 0\nmodel name : 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz\nflags : fpu vme de pse tsc msr pae mce cx8 apic sep mtrr sse sse2 ssse3 sse4_1 sse4_2 aes avx avx2 avx512f\nprocessor : 1\nmodel name : 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz\nflags : fpu sse4_2 aes avx2\n";

        const INTEL_MEMINFO: &str =
            "MemTotal:       16384000 kB\nMemAvailable:    8388608 kB\nBuffers:          123456 kB\n";

        const AMD_NO_AVX2_CPUINFO: &str = "processor : 0\nmodel name : AMD FX(tm)-6300 Six-Core Processor\nflags : fpu mmx sse sse2 sse4_1 sse4_2 aes\n";

        fn empty_inputs<'a>() -> Inputs<'a> {
            Inputs {
                cpuinfo: "",
                meminfo: "",
                dmi_vendor: "",
                dmi_product: "",
                dmi_bios: "",
                pci_ids: &[],
                net_interfaces: &[],
                block_devices: &[],
            }
        }

        /// Rendered text with line breaks and runs of spaces collapsed, so assertions
        /// do not depend on where wrapping happened.
        fn flat(text: &str) -> String {
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        }

        fn assert_render_width(inputs: &Inputs<'_>) {
            let rendered = build(inputs).render();
            for line in rendered.lines() {
                assert!(line.chars().count() <= 60, "line too wide: {line}");
            }
        }

        #[test]
        fn realistic_intel_laptop_sample_renders() {
            let inputs = Inputs {
                cpuinfo: INTEL_CPUINFO,
                meminfo: INTEL_MEMINFO,
                dmi_vendor: "  Dell Inc. ",
                dmi_product: "XPS 13 9310",
                dmi_bios: "1.21.0",
                pci_ids: &[(0x8086, 0x2725), (0x8086, 0x15b8), (0x10de, 0x1b82)],
                net_interfaces: &["lo", "eno1", "wlp3s0", "docker0"],
                block_devices: &["nvme0n1", "sda"],
            };
            let rendered = build(&inputs).render();
            assert!(rendered.contains("Logical CPUs: 2"));
            assert!(rendered.contains("AVX2: yes"));
            assert!(rendered.contains("AVX512F: yes"));
            assert!(rendered.contains("SSE4.2: yes"));
            assert!(rendered.contains("MemTotal: 15.6 GiB"));
            assert!(rendered.contains("Wired: eno1"));
            assert!(rendered.contains("Wireless: wlp3s0"));
            assert!(rendered.contains("Verdict: Ready"));
            assert_render_width(&inputs);
        }

        #[test]
        fn broadcom_wifi_sample_reports_no_wifi_support() {
            let avx2_cpuinfo = "processor : 0\nmodel name : Intel(R) Core(TM) i5-7200U CPU @ 2.50GHz\nflags : fpu sse sse2 ssse3 sse4_1 sse4_2 aes avx avx2\n";
            let inputs = Inputs {
                cpuinfo: avx2_cpuinfo,
                meminfo: "MemTotal: 8192000 kB\nMemAvailable: 4096000 kB\n",
                dmi_vendor: "HP",
                dmi_product: "EliteBook 840 G3",
                dmi_bios: "N75 Ver. 01.46",
                pci_ids: &[(0x14e4, 0x43b1), (0x8086, 0x1533)],
                net_interfaces: &["eth0", "wlan0"],
                block_devices: &["sda"],
            };
            let rendered = flat(&build(&inputs).render());
            assert!(rendered.contains("needs proprietary firmware"));
            assert!(rendered.contains("Verdict: Ready, but no Wi-Fi support for your adapter"));
            assert_render_width(&inputs);
        }

        #[test]
        fn missing_avx2_is_too_old() {
            let inputs = Inputs {
                cpuinfo: AMD_NO_AVX2_CPUINFO,
                meminfo: "MemTotal: 8388608 kB\n",
                dmi_vendor: "ASUS",
                dmi_product: "Custom build",
                dmi_bios: "4103",
                pci_ids: &[(0x168c, 0x0036)],
                net_interfaces: &["enp3s0", "wlp2s0"],
                block_devices: &["sdb"],
            };
            let rendered = build(&inputs).render();
            assert!(rendered.contains("AVX2: NO"));
            assert!(rendered.contains("Verdict: Too old: needs AVX2"));
            assert_render_width(&inputs);
        }

        #[test]
        fn empty_strings_everywhere_do_not_panic() {
            let inputs = empty_inputs();
            let rendered = build(&inputs).render();
            assert!(rendered.contains("Model: unknown"));
            assert!(rendered.contains("Logical CPUs: 0"));
            assert!(rendered.contains("MemTotal: unknown"));
            assert!(!rendered.contains("0000:0000"));
            assert_render_width(&inputs);
        }

        #[test]
        fn placeholder_dmi_strings_become_unknown() {
            let inputs = Inputs {
                cpuinfo: INTEL_CPUINFO,
                meminfo: INTEL_MEMINFO,
                dmi_vendor: "To be filled by O.E.M.",
                dmi_product: "Default string",
                dmi_bios: "Not Specified",
                pci_ids: &[(0x1af4, 0x1000)],
                net_interfaces: &["enp0s3"],
                block_devices: &[],
            };
            let rendered = build(&inputs).render();
            assert!(rendered.contains("Vendor: unknown"));
            assert!(rendered.contains("Product: unknown"));
            assert!(rendered.contains("BIOS: unknown"));
            assert!(flat(&rendered).contains("1af4:1000 unknown vendor"));
            assert_render_width(&inputs);
        }

        #[test]
        fn known_vendor_unknown_device_falls_back_to_other() {
            let inputs = Inputs {
                cpuinfo: INTEL_CPUINFO,
                meminfo: INTEL_MEMINFO,
                dmi_vendor: "Lenovo",
                dmi_product: "ThinkPad",
                dmi_bios: "1.0",
                pci_ids: &[(0x8086, 0xffff), (0x14c3, 0xffff)],
                net_interfaces: &[],
                block_devices: &["/dev/nvme0n1"],
            };
            let rendered = flat(&build(&inputs).render());
            assert!(rendered.contains("8086:ffff Intel unknown device [Other; support unknown]"));
            assert!(rendered.contains("14c3:ffff MediaTek unknown device [Other; support unknown]"));
            assert!(rendered.contains("/dev/nvme0n1"));
            assert_render_width(&inputs);
        }

        #[test]
        fn render_width_never_exceeds_60_columns() {
            let long_model = "model name : Very Long Processor Brand String That Keeps Going And Going And Going And Going Past The Normal Column Limit For Wrapped Output\n";
            let cpuinfo = format!("processor : 0\n{long_model}flags : avx2\n");
            let inputs = Inputs {
                cpuinfo: &cpuinfo,
                meminfo: "MemTotal: 99999999 kB\nMemAvailable: 99999999 kB\n",
                dmi_vendor:
                    "A Manufacturer With A Reasonably Long Name That Must Be Wrapped Cleanly",
                dmi_product:
                    "A Product Name With Plenty Of Words To Exercise The Wrapping Code Path",
                dmi_bios: "BIOS Version String With Extra Tokens",
                pci_ids: &[(0x8086, 0x2725), (0x14e4, 0x43b1), (0xffff, 0xeeee)],
                net_interfaces: &[
                    "eno1234567890verylongname",
                    "wlp9876543210verylongname",
                    "veth1234567890",
                ],
                block_devices: &[
                    "nvme0n1p1-with-a-very-long-extra-name",
                    "/dev/disk/by-id/very-long-identifier",
                ],
            };
            assert_render_width(&inputs);
        }

        #[test]
        fn unlisted_devices_are_named_by_class_code() {
            let ids = [
                (0x8086, 0x5914),
                (0x8086, 0xabcd),
                (0x10ec, 0xffff),
                (0x10de, 0xffff),
            ];
            let classes = [0x060000, 0x028000, 0x020000, 0x030000];
            let report = build_with_classes(
                &Inputs {
                    cpuinfo: "processor : 0\nflags : avx2\n",
                    meminfo: "",
                    dmi_vendor: "",
                    dmi_product: "",
                    dmi_bios: "",
                    pci_ids: &ids,
                    net_interfaces: &[],
                    block_devices: &[],
                },
                &classes,
            );
            let text = report.render();
            assert!(text.contains("8086:5914 Intel host bridge"), "{text}");
            assert!(
                text.contains("Intel wireless network adapter [WiFi;"),
                "{text}"
            );
            assert!(text.contains("Realtek Ethernet adapter"), "{text}");
            assert!(
                text.contains("NVIDIA graphics adapter [GPU; not supported"),
                "{text}"
            );
            assert!(text.lines().all(|l| l.chars().count() <= 60), "{text}");
        }

        #[test]
        fn a_short_class_list_leaves_the_rest_unknown() {
            let ids = [(0x8086, 0x5914), (0x8086, 0xabcd)];
            let report = build_with_classes(
                &Inputs {
                    cpuinfo: "processor : 0\nflags : avx2\n",
                    meminfo: "",
                    dmi_vendor: "",
                    dmi_product: "",
                    dmi_bios: "",
                    pci_ids: &ids,
                    net_interfaces: &[],
                    block_devices: &[],
                },
                &[0x060000],
            );
            let text = report.render();
            assert!(text.contains("Intel host bridge"), "{text}");
            assert!(text.contains("8086:abcd Intel unknown device"), "{text}");
        }
    }
}

pub mod system {
    //! PID 1 duties: early mounts, hostname, power-off. Linux only; elsewhere
    //! these are no-ops so the daemon runs in development mode.

    /// Mount proc, sys, dev, tmp and run, create log dirs, set the hostname.
    /// Returns human-readable warnings for steps that failed.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn early_setup() -> Vec<String> {
        use nix::mount::{mount, MsFlags};
        // The kernel starts init with no PATH, so child programs (ip, llama-server) would not be found.
        std::env::set_var(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );
        let mut warn = Vec::new();
        for (src, dst, fs) in [
            ("proc", "/proc", "proc"),
            ("sysfs", "/sys", "sysfs"),
            ("devtmpfs", "/dev", "devtmpfs"),
            ("tmpfs", "/tmp", "tmpfs"),
            ("tmpfs", "/run", "tmpfs"),
        ] {
            let _ = std::fs::create_dir_all(dst);
            match mount(Some(src), dst, Some(fs), MsFlags::empty(), None::<&str>) {
                Ok(()) | Err(nix::errno::Errno::EBUSY) => {}
                Err(e) => warn.push(format!("mount {dst} failed: {e}")),
            }
        }
        if let Err(e) = std::fs::create_dir_all("/var/log") {
            warn.push(format!("mkdir /var/log failed: {e}"));
        }
        if let Err(e) = nix::unistd::sethostname("mouli") {
            warn.push(format!("sethostname failed: {e}"));
        }
        if let Err(e) = console_stdio() {
            warn.push(format!("console setup failed: {e}"));
        }
        if !loopback_up() {
            warn.push("loopback up failed (no `ip` or `ifconfig`)".into());
        }
        warn
    }

    /// Point stdin, stdout and stderr at `/dev/console`, so the chat works even when the kernel
    /// started init without a console (initramfs before devtmpfs is mounted).
    #[cfg(target_os = "linux")]
    fn console_stdio() -> nix::Result<()> {
        use nix::fcntl::{open, OFlag};
        use nix::sys::stat::Mode;
        use nix::unistd::dup2;
        let fd = open(
            "/dev/console",
            OFlag::O_RDWR | OFlag::O_NOCTTY,
            Mode::empty(),
        )?;
        for target in 0..=2 {
            dup2(std::os::fd::AsRawFd::as_raw_fd(&fd), target)?;
        }
        Ok(())
    }

    /// Bring the loopback interface up with the system's `ip` or `ifconfig` (the inference server
    /// listens on 127.0.0.1). Returns whether one of them succeeded.
    #[cfg(target_os = "linux")]
    fn loopback_up() -> bool {
        use std::process::{Command, Stdio};
        let attempts: [(&str, &[&str]); 2] = [
            ("ip", &["link", "set", "lo", "up"]),
            ("ifconfig", &["lo", "up"]),
        ];
        attempts.iter().any(|(cmd, args)| {
            Command::new(cmd)
                .args(*args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    }

    /// Development-mode stand-in.
    #[cfg(not(target_os = "linux"))]
    #[must_use]
    pub fn early_setup() -> Vec<String> {
        vec!["early setup is Linux-only; skipped".into()]
    }

    /// Flush filesystems and power off. Only returns on failure.
    #[cfg(target_os = "linux")]
    pub fn power_off() -> String {
        nix::unistd::sync();
        match nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_POWER_OFF) {
            Ok(never) => match never {},
            Err(e) => format!("power off failed: {e}"),
        }
    }

    /// Development-mode stand-in.
    #[cfg(not(target_os = "linux"))]
    pub fn power_off() -> String {
        "power off is Linux-only".into()
    }

    /// Flush filesystems and restart. Only returns on failure.
    #[cfg(target_os = "linux")]
    pub fn reboot() -> String {
        nix::unistd::sync();
        match nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT) {
            Ok(never) => match never {},
            Err(e) => format!("restart failed: {e}"),
        }
    }

    /// Development-mode stand-in.
    #[cfg(not(target_os = "linux"))]
    pub fn reboot() -> String {
        "restart is Linux-only".into()
    }

    /// Kernel command line, or empty when unavailable.
    #[must_use]
    pub fn kernel_cmdline() -> String {
        std::fs::read_to_string("/proc/cmdline").unwrap_or_default()
    }

    /// Whether this process is PID 1.
    #[must_use]
    pub fn is_pid1() -> bool {
        std::process::id() == 1
    }
}
