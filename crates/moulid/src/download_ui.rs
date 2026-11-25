//! The download screen: pick a model from the list, confirm, watch the progress.
//!
//! Models copied onto a disk or USB stick by hand keep working exactly as before; a downloaded
//! model is saved in the same `models` folder of the MOULI-DATA partition, so both kinds appear
//! together in the model list.

use crate::catalog::{https_host, Entry};
use crate::fetch::{download, free_bytes, Fetcher, Outcome, Policy, Status};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// A size in the units people read: `1.1 GB`, `807 MB`.
#[must_use]
pub fn human_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes / 1_000_000)
    }
}

/// Seconds as `1m12s`.
#[must_use]
pub fn human_time(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// The progress line, for example `  42%  468 of 1117 MB  8.9 MB/s  about 1m12s left`.
#[must_use]
pub fn progress_line(s: &Status) -> String {
    let pct = s
        .progress
        .percent
        .map_or(String::new(), |p| format!("{p:>3}%  "));
    let eta = s
        .progress
        .eta_secs
        .map_or(String::new(), |e| format!("  about {} left", human_time(e)));
    format!(
        "  {pct}{} of {} MB  {:.1} MB/s{eta}   ",
        s.done / 1_000_000,
        s.total / 1_000_000,
        s.progress.bytes_per_sec as f64 / 1e6
    )
}

/// The catalogue as numbered lines. `budget_mb` is the memory available for model weights.
#[must_use]
pub fn render_list(entries: &[Entry], budget_mb: u64) -> String {
    let mut s = String::from("\n  Download a model\n  ----------------\n");
    for (i, e) in entries.iter().enumerate() {
        let fit = if e.min_ram_mb <= budget_mb * 3 / 2 {
            "fits in memory"
        } else {
            "may be too big for this machine"
        };
        s.push_str(&format!(
            "  {:>2}) {}  {}  [{}; {}]\n      {}\n",
            i + 1,
            e.id,
            human_size(e.size),
            e.licence,
            fit,
            e.note
        ));
    }
    s.push_str(
        "\n  Models you copy onto a disk or USB stick yourself still appear in the main list.\n",
    );
    s
}

/// The whole screen. Returns the path of the model when one was downloaded (or was already
/// there), so the caller can refresh the model list.
///
/// # Errors
/// I/O errors on the console.
pub fn flow<I: BufRead, O: Write, F: Fetcher>(
    input: &mut I,
    out: &mut O,
    fetcher: &F,
    policy: &Policy,
    entries: &[Entry],
    models_dir: &Path,
    budget_mb: u64,
) -> std::io::Result<Option<PathBuf>> {
    use crate::hosted::ask;
    if let Err(e) = std::fs::create_dir_all(models_dir) {
        writeln!(out, "\n  Cannot use the models folder: {e}")?;
        return Ok(None);
    }
    write!(out, "{}", render_list(entries, budget_mb))?;
    let Some(a) = ask(input, out, "\n  Number of the model (Enter to go back) > ")? else {
        return Ok(None);
    };
    let Some(e) = a
        .parse::<usize>()
        .ok()
        .and_then(|k| entries.get(k.wrapping_sub(1)))
    else {
        return Ok(None);
    };
    let host = https_host(&e.url).unwrap_or_default();
    writeln!(
        out,
        "\n  {} ({})\n  Licence: {}\n  This will connect to {host} and save the file in {}.",
        e.id,
        human_size(e.size),
        e.licence,
        models_dir.display()
    )?;
    let Some(yes) = ask(
        input,
        out,
        "  Type y and Enter to download, anything else to cancel > ",
    )?
    else {
        return Ok(None);
    };
    if !yes.eq_ignore_ascii_case("y") {
        writeln!(out, "  Cancelled.")?;
        return Ok(None);
    }
    writeln!(
        out,
        "  Downloading. This can take a while; it is safe to try again if it stops."
    )?;
    out.flush()?;
    let free = free_bytes(models_dir);
    let mut shown = false;
    let result = download(fetcher, e, models_dir, free, policy, &mut |s| {
        let _ = write!(out, "\r{}", progress_line(s));
        let _ = out.flush();
        shown = true;
    });
    if shown {
        writeln!(out)?;
    }
    match result {
        Ok(Outcome::Downloaded(p)) => {
            writeln!(
                out,
                "  Done. The model is checked and saved: {}",
                p.display()
            )?;
            writeln!(out, "  It now appears in the model list.")?;
            Ok(Some(p))
        }
        Ok(Outcome::AlreadyHad(p)) => {
            writeln!(
                out,
                "  You already have this model, and it is intact: {}",
                p.display()
            )?;
            Ok(Some(p))
        }
        Err(msg) => {
            writeln!(out, "  {msg}")?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;
    use crate::fetch::Reply;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    struct Serve(Vec<u8>);

    impl Fetcher for Serve {
        fn get(&self, _: &str, _: &[(String, String)]) -> Result<Reply, String> {
            Ok(Reply {
                status: 200,
                headers: vec![("Content-Length".into(), self.0.len().to_string())],
                body: Box::new(Cursor::new(self.0.clone())),
            })
        }
    }

    fn entry_for(d: &[u8]) -> Entry {
        Entry {
            id: "test-model".into(),
            file: "test.gguf".into(),
            url: "https://huggingface.co/o/r/resolve/main/test.gguf".into(),
            size: d.len() as u64,
            sha256: Sha256::digest(d)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            licence: "MIT".into(),
            min_ram_mb: 500,
            note: "for tests".into(),
        }
    }

    fn model() -> Vec<u8> {
        let mut v = b"GGUF".to_vec();
        v.extend((0..1_999_996u32).map(|i| (i % 251) as u8));
        v
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-dlui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn run(script: &str, d: &[u8], dir: &Path) -> (Option<PathBuf>, String) {
        let mut out = Vec::new();
        let mut input = Cursor::new(script.as_bytes().to_vec());
        let r = flow(
            &mut input,
            &mut out,
            &Serve(d.to_vec()),
            &Policy::production(),
            &[entry_for(d)],
            dir,
            4000,
        )
        .unwrap();
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn sizes_and_times_read_like_people_say_them() {
        assert_eq!(human_size(1_117_320_736), "1.1 GB");
        assert_eq!(human_size(807_694_464), "807 MB");
        assert_eq!(human_time(5), "5s");
        assert_eq!(human_time(72), "1m12s");
        assert_eq!(human_time(3_900), "1h05m");
    }

    #[test]
    fn the_list_shows_size_licence_and_whether_it_fits() {
        let l = catalog::parse(catalog::BUILT_IN).unwrap();
        let s = render_list(&l, 1000);
        assert!(s.contains("qwen2.5-1.5b") && s.contains("Apache-2.0"));
        assert!(s.contains("1.1 GB"));
        assert!(s.contains("may be too big"), "a 3B model on a 1 GB budget");
        assert!(s.contains("still appear in the main list"));
        assert!(render_list(&l, 16_000).matches("fits in memory").count() == 4);
    }

    #[test]
    fn the_progress_line_has_percent_speed_and_time_left() {
        let s = Status {
            done: 468_000_000,
            total: 1_117_000_000,
            progress: crate::download_plan::progress(468_000_000, Some(1_117_000_000), 52_000),
        };
        let l = progress_line(&s);
        assert!(l.contains("41%") || l.contains("42%"), "{l}");
        assert!(l.contains("468 of 1117 MB"), "{l}");
        assert!(l.contains("MB/s") && l.contains("left"), "{l}");
    }

    #[test]
    fn choosing_confirming_and_downloading_saves_a_checked_model() {
        let d = model();
        let dir = tmp("ok");
        let (r, text) = run("1\ny\n", &d, &dir);
        assert_eq!(r, Some(dir.join("test.gguf")));
        assert_eq!(std::fs::read(dir.join("test.gguf")).unwrap(), d);
        assert!(
            text.contains("huggingface.co"),
            "the server is named before connecting: {text}"
        );
        assert!(text.contains("Licence: MIT"));
        assert!(text.contains("Done."));
        // The same screen again finds the intact copy and does not download.
        let (again, text2) = run("1\ny\n", &d, &dir);
        assert!(again.is_some());
        assert!(text2.contains("already have this model"), "{text2}");
    }

    #[test]
    fn anything_but_y_cancels_and_nothing_is_written() {
        let d = model();
        for script in ["1\nn\n", "1\n\n", "1\nyes please\n", "1\n"] {
            let dir = tmp("cancel");
            let (r, text) = run(script, &d, &dir);
            assert!(r.is_none(), "{script:?}");
            assert!(
                !dir.join("test.gguf").exists() && !dir.join(".test.gguf.part").exists(),
                "{script:?}: {text}"
            );
        }
    }

    #[test]
    fn a_bad_number_or_enter_goes_back() {
        let d = model();
        for script in ["\n", "0\n", "2\n", "x\n", "-1\n", ""] {
            let (r, _) = run(script, &d, &tmp("back"));
            assert!(r.is_none(), "{script:?}");
        }
    }

    #[test]
    fn a_failed_download_explains_and_returns_nothing() {
        let d = model();
        let mut wrong = d.clone();
        wrong[10] ^= 1;
        let dir = tmp("bad");
        let mut out = Vec::new();
        let mut input = Cursor::new(b"1\ny\n".to_vec());
        let r = flow(
            &mut input,
            &mut out,
            &Serve(wrong),
            &Policy::production(),
            &[entry_for(&d)],
            &dir,
            4000,
        )
        .unwrap();
        assert!(r.is_none());
        assert!(String::from_utf8(out).unwrap().contains("checksum"));
        assert!(!dir.join("test.gguf").exists());
    }
}
