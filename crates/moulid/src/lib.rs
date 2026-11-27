//! moulid: MOULI init and AI service supervisor.
//!
//! As PID 1 it mounts the base filesystems, probes the hardware, starts the
//! local inference server with a hardware-aware plan, respawns it, and
//! offers a console chat. With `mouli.selftest=1` on the kernel command line
//! (or `--selftest`) it sends one prompt, prints the reply and powers off.
//!
//! The inference server is still an external HTTP process (llama-server
//! compatible `/health` and `/completion`); it is to be replaced by `mouli-llm`
//! once that can run a full model.
//!
//! Wired networking ([`net`]) is brought up with DHCP from the local network; nothing contacts the
//! internet by itself.
//!
//! A filesystem labelled `MOULI-DATA` (see [`persist`]) is mounted read-write to keep settings
//! and chat history across reboots; without one nothing is saved.
//!
//! A hosted model ([`hosted`]) can be chosen instead of a local one: the person enters the
//! provider, model and API key, agrees that messages leave the machine, and messages are washed
//! of secrets first. Nothing is sent without that consent.
//!
//! Ported from the retired C `userspace/aeond`.
#![forbid(unsafe_code)]

// The system-related modules live in `mouli-utility`; they stay reachable here under the same names.
pub use mouli_utility::{
    bluetooth, civil_time, clocksync, fslabel, hwprobe, hwreport, known_networks, net,
    partition_table, procparse, sntp, system, wifi, wifi_saved,
};
pub mod args;
pub mod backoff;
pub mod banner;
pub mod browse;
pub mod catalog;
pub mod context;
pub mod datagraph;
pub mod download_plan;
pub mod download_ui;
pub mod fetch;
pub mod gguf_info;
pub mod gui;
pub mod hosted;
pub mod http;
pub mod layout;
pub mod lineedit;
pub mod models;
pub mod persist;
pub mod reply;
#[cfg(target_os = "linux")]
pub mod shutdown;
pub mod storage;
pub mod sysinfo;
pub mod textwrap;
pub mod toolloop;
pub mod ui;
pub mod updatecheck;
pub mod verify;

use std::path::{Path, PathBuf};

/// Version string printed at boot.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Last `n` lines of `text`, for showing the end of a log on the console.
#[must_use]
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// True when the kernel command line contains `word` as a whole token.
#[must_use]
pub fn cmdline_has(cmdline: &str, word: &str) -> bool {
    cmdline.split_whitespace().any(|t| t == word)
}

/// First `*.gguf` file in `dir` (sorted by name for determinism).
#[must_use]
pub fn find_model(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "gguf"))
        .collect();
    found.sort();
    found.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tails_a_log() {
        assert_eq!(tail_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(tail_lines("a\nb", 10), "a\nb");
        assert_eq!(tail_lines("", 3), "");
    }

    #[test]
    fn cmdline_tokens() {
        let c = "console=ttyS0 mouli.selftest=1 quiet\n";
        assert!(cmdline_has(c, "mouli.selftest=1"));
        assert!(!cmdline_has(c, "mouli.selftest"));
    }

    #[test]
    fn finds_first_gguf() {
        let d = std::env::temp_dir().join(format!("moulid-models-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("b.gguf"), b"x").unwrap();
        std::fs::write(d.join("a.gguf"), b"x").unwrap();
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        assert_eq!(find_model(&d).unwrap().file_name().unwrap(), "a.gguf");
        std::fs::remove_dir_all(&d).unwrap();
        assert!(find_model(&d).is_none());
    }
}
