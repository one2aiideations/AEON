//! Finding models on the machine: the baked-in folder plus every disk or USB partition.
//!
//! Partitions are mounted read-only and scanned a few levels deep for `.gguf` files. Each file
//! is described from its GGUF header so the picker can show architecture, size and whether it
//! fits in memory.

use aeon_gguf::parse_header_only;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How deep below a mount point to look.
pub const SCAN_DEPTH: usize = 3;
/// How much of a file to read to understand its header.
const HEADER_BYTES: u64 = 64 << 20;

/// A model file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Full path.
    pub path: PathBuf,
    /// File size in bytes.
    pub size: u64,
    /// `general.name` or the file name.
    pub name: String,
    /// `general.architecture` when the header was readable.
    pub arch: Option<String>,
    /// Trained context length when present.
    pub context: Option<u32>,
}

impl Found {
    /// Size in whole megabytes, rounded up.
    #[must_use]
    pub fn size_mb(&self) -> u64 {
        self.size.div_ceil(1 << 20)
    }

    /// Whether the file fits the memory budget (megabytes).
    #[must_use]
    pub fn fits(&self, budget_mb: u64) -> bool {
        self.size_mb() <= budget_mb
    }
}

/// Describe one file, or `None` when it is not a readable GGUF.
#[must_use]
pub fn describe(path: &Path) -> Option<Found> {
    let size = std::fs::metadata(path).ok()?.len();
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(HEADER_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    if head.get(..4) != Some(b"GGUF") {
        return None;
    }
    let fallback = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let (name, arch, context) = match parse_header_only(&head).and_then(|g| g.summary()) {
        Ok(s) => (
            s.name.unwrap_or(fallback),
            Some(s.architecture),
            s.context_length,
        ),
        Err(_) => (fallback, None, None),
    };
    Some(Found {
        path: path.to_path_buf(),
        size,
        name,
        arch,
        context,
    })
}

/// Find GGUF files under `dir`, at most `depth` levels down, sorted by path. Hidden entries and
/// unreadable directories are skipped.
#[must_use]
pub fn scan_dir(dir: &Path, depth: usize) -> Vec<Found> {
    let mut out = Vec::new();
    walk(dir, depth, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Found>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(Result::ok) {
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, out);
            }
        } else if ft.is_file()
            && p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("gguf"))
        {
            if let Some(f) = describe(&p) {
                out.push(f);
            }
        }
    }
}

/// Partition names worth trying to mount, from the text of `/proc/partitions`. Skips RAM disks,
/// loop and zram devices, and whole disks that have partitions.
#[must_use]
pub fn candidate_partitions(proc_partitions: &str) -> Vec<String> {
    let names: Vec<&str> = proc_partitions
        .lines()
        .skip(2)
        .filter_map(|l| l.split_whitespace().nth(3))
        .filter(|n| {
            !["ram", "loop", "zram", "fd"]
                .iter()
                .any(|p| n.starts_with(p))
        })
        .collect();
    let mut out: Vec<String> = names
        .iter()
        .filter(|n| {
            !names.iter().any(|m| {
                m.len() > n.len() && m.starts_with(**n) && is_partition_suffix(&m[n.len()..])
            })
        })
        .map(|n| (*n).to_string())
        .collect();
    out.extend(
        names
            .iter()
            .filter(|n| n.starts_with("sr"))
            .map(|n| (*n).to_string()),
    );
    out.sort();
    out.dedup();
    out
}

fn is_partition_suffix(s: &str) -> bool {
    let s = s.strip_prefix('p').unwrap_or(s);
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `dev` (for example `/dev/sda1`) already appears in the text of `/proc/mounts`.
#[must_use]
pub fn is_mounted(proc_mounts: &str, dev: &str) -> bool {
    proc_mounts
        .lines()
        .any(|l| l.split_whitespace().next() == Some(dev))
}

/// Filesystem types tried, in order, when mounting a partition.
pub const FS_TYPES: [&str; 7] = ["ext4", "vfat", "exfat", "ntfs3", "iso9660", "btrfs", "xfs"];

/// Mount every candidate partition read-only under `/mnt/<name>` and return the mount points
/// that succeeded. Linux only; elsewhere returns nothing.
#[cfg(target_os = "linux")]
pub fn mount_partitions() -> Vec<PathBuf> {
    use nix::mount::{mount, MsFlags};
    let table = std::fs::read_to_string("/proc/partitions").unwrap_or_default();
    let active = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut mounted = Vec::new();
    for name in candidate_partitions(&table) {
        let dev = format!("/dev/{name}");
        let target = PathBuf::from(format!("/mnt/{name}"));
        if is_mounted(&active, &dev) {
            mounted.push(target);
            continue;
        }
        if std::fs::create_dir_all(&target).is_err() {
            continue;
        }
        let ok = FS_TYPES.iter().any(|fs| {
            mount(
                Some(dev.as_str()),
                &target,
                Some(*fs),
                MsFlags::MS_RDONLY,
                None::<&str>,
            )
            .is_ok()
        });
        if ok {
            mounted.push(target);
        }
    }
    mounted
}

/// Development-mode stand-in.
#[cfg(not(target_os = "linux"))]
pub fn mount_partitions() -> Vec<PathBuf> {
    Vec::new()
}

/// All models: the baked-in folder first, then everything on mounted partitions.
pub fn discover(builtin: &Path, mount: bool) -> Vec<Found> {
    let mut all = scan_dir(builtin, SCAN_DEPTH);
    if mount {
        for m in mount_partitions() {
            all.extend(scan_dir(&m, SCAN_DEPTH));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    all.retain(|f| seen.insert(f.path.clone()));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeon_gguf::{GgmlType, GgufWriter, Value};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aeond-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tiny_gguf(name: Option<&str>) -> Vec<u8> {
        let mut w = GgufWriter::new();
        w.add_metadata("general.architecture", Value::String("llama".into()));
        if let Some(n) = name {
            w.add_metadata("general.name", Value::String(n.into()));
        }
        w.add_metadata("llama.context_length", Value::U32(2048));
        w.add_tensor("t", &[4], GgmlType::F32, vec![0; 16]).unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn describes_a_real_header() {
        let d = tmp("describe");
        std::fs::write(d.join("m.gguf"), tiny_gguf(Some("Tiny Test"))).unwrap();
        let f = describe(&d.join("m.gguf")).unwrap();
        assert_eq!(
            (f.name.as_str(), f.arch.as_deref(), f.context),
            ("Tiny Test", Some("llama"), Some(2048))
        );
        assert!(f.size > 16 && f.fits(1) && !f.fits(0));
        std::fs::write(d.join("noname.gguf"), tiny_gguf(None)).unwrap();
        assert_eq!(describe(&d.join("noname.gguf")).unwrap().name, "noname");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rejects_non_gguf_and_keeps_unreadable_headers() {
        let d = tmp("reject");
        std::fs::write(d.join("a.gguf"), b"not a model").unwrap();
        assert!(describe(&d.join("a.gguf")).is_none());
        assert!(describe(&d.join("missing.gguf")).is_none());
        std::fs::write(d.join("trunc.gguf"), b"GGUF\x03\x00\x00\x00").unwrap();
        let f = describe(&d.join("trunc.gguf")).unwrap();
        assert!(
            f.arch.is_none() && f.name == "trunc",
            "listed with the file name when the header is unreadable"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn scan_depth_hidden_and_case() {
        let d = tmp("scan");
        let g = tiny_gguf(None);
        std::fs::create_dir_all(d.join("a/b/c/d")).unwrap();
        std::fs::create_dir_all(d.join(".trash")).unwrap();
        std::fs::write(d.join("top.gguf"), &g).unwrap();
        std::fs::write(d.join("a/b/c/deep.GGUF"), &g).unwrap();
        std::fs::write(d.join("a/b/c/d/toodeep.gguf"), &g).unwrap();
        std::fs::write(d.join(".trash/hidden.gguf"), &g).unwrap();
        std::fs::write(d.join("a/readme.txt"), b"x").unwrap();
        let names: Vec<_> = scan_dir(&d, SCAN_DEPTH)
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["deep.GGUF", "top.gguf"]);
        assert!(scan_dir(&d.join("nope"), 3).is_empty());
        let all = discover(&d, false);
        assert_eq!(all.len(), 2);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn mounted_detection() {
        let m = "/dev/sda1 /mnt/sda1 vfat ro 0 0\nproc /proc proc rw 0 0\n";
        assert!(is_mounted(m, "/dev/sda1") && !is_mounted(m, "/dev/sda"));
        assert!(!is_mounted("", "/dev/sda1"));
    }

    #[test]
    fn partition_candidates() {
        let t = "major minor  #blocks  name\n\n   8        0  976762584 sda\n   8        1     524288 sda1\n   8        2  976237568 sda2\n 259        0  500107608 nvme0n1\n 259        1     524288 nvme0n1p1\n   8       16   15564800 sdb\n   7        0      65536 loop0\n   1        0       4096 ram0\n  11        0    1048575 sr0\n 252        0    1000000 zram0\n";
        assert_eq!(
            candidate_partitions(t),
            ["nvme0n1p1", "sda1", "sda2", "sdb", "sr0"]
        );
        assert!(candidate_partitions("").is_empty());
    }
}
