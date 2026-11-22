//! Hardware probe and run plan for aeond.
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

/// PCI vendor ids AEON knows by name.
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
    /// CPU supports NEON/ASIMD (kept for parity; AEON targets x86_64).
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
            "AEON hardware: {} CPU threads{}{}{} | RAM {}/{} MB available | GPUs: {}",
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
            "{}\nAEON plan: threads={} ctx={} model-budget={}MB backend={}",
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
