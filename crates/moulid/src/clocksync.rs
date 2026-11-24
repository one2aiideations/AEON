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
    let corrected = local_unix_secs.saturating_add((offset_ms + offset_ms.signum() * 500) / 1000);
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
