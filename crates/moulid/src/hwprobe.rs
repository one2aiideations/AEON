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
        let d = std::env::temp_dir().join(format!("mouli-hwprobe-{name}-{}", std::process::id()));
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
