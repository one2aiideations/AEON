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
