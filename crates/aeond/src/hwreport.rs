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
    let cpu = parse_cpuinfo(i.cpuinfo);
    let memory = parse_meminfo(i.meminfo);
    let dmi = DmiReport {
        vendor: clean_dmi(i.dmi_vendor),
        product: clean_dmi(i.dmi_product),
        bios: clean_dmi(i.dmi_bios),
    };
    let pci: Vec<PciReport> = i.pci_ids.iter().map(|&(v, d)| identify_pci(v, d)).collect();
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

fn identify_pci(vendor: u16, device: u16) -> PciReport {
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
        "{raw} {} ({}) [{}; {}]",
        device.name,
        class_label(device.class),
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
        assert!(
            rendered.contains("8086:ffff Intel unknown device (Other) [Other; support unknown]")
        );
        assert!(
            rendered.contains("14c3:ffff MediaTek unknown device (Other) [Other; support unknown]")
        );
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
            dmi_vendor: "A Manufacturer With A Reasonably Long Name That Must Be Wrapped Cleanly",
            dmi_product: "A Product Name With Plenty Of Words To Exercise The Wrapping Code Path",
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
}
