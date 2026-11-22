//! PID 1 duties: early mounts, hostname, power-off. Linux only; elsewhere
//! these are no-ops so the daemon runs in development mode.

/// Mount proc, sys, dev, tmp and run, create log dirs, set the hostname.
/// Returns human-readable warnings for steps that failed.
#[cfg(target_os = "linux")]
#[must_use]
pub fn early_setup() -> Vec<String> {
    use nix::mount::{mount, MsFlags};
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
    if let Err(e) = nix::unistd::sethostname("aeon") {
        warn.push(format!("sethostname failed: {e}"));
    }
    warn
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
