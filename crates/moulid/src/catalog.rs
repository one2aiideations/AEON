//! The list of models MOULI can download.
//!
//! The list is plain text, one model per line, fields separated by `|`:
//!
//! ```text
//! id|file name|https address|size in bytes|sha256|licence|memory needed in MB|note
//! ```
//!
//! Every entry must carry the exact size and the SHA-256 of the file, so that what is downloaded
//! can be checked; an entry without them is refused. Addresses must be `https` and point at a
//! host this module allows (Hugging Face), so a bad or tampered list cannot send the downloader
//! somewhere else. The built-in list ships in the image; a file at [`CATALOG_FILE`] replaces it.

use std::collections::BTreeSet;

/// Where an updated list can be placed.
pub const CATALOG_FILE: &str = "/etc/mouli/models.catalog";
/// The most entries accepted.
pub const MAX_ENTRIES: usize = 64;
/// The longest list file accepted, in bytes.
pub const MAX_LIST_BYTES: usize = 64 * 1024;
/// The largest model accepted: 16 GiB.
pub const MAX_MODEL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// The smallest model accepted: 1 MB (a smaller file is not a real model).
pub const MIN_MODEL_BYTES: u64 = 1_000_000;

/// One downloadable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Short name, for example `qwen2.5-1.5b`.
    pub id: String,
    /// File name it is saved under.
    pub file: String,
    /// Where it comes from.
    pub url: String,
    /// Exact size in bytes.
    pub size: u64,
    /// SHA-256 of the file, 64 lower-case hex characters.
    pub sha256: String,
    /// Licence, shown before downloading.
    pub licence: String,
    /// Memory the model needs to run, in MB (rough).
    pub min_ram_mb: u64,
    /// A short note.
    pub note: String,
}

/// The list built into the image. Sizes and hashes were taken from the files themselves.
pub const BUILT_IN: &str = "\
# id|file|url|size|sha256|licence|memory MB|note
qwen2.5-1.5b|qwen2.5-1.5b-instruct-q4_k_m.gguf|https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf|1117320736|6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e|Apache-2.0|1800|Good first model: fast and small
qwen2.5-3b|qwen2.5-3b-instruct-q4_k_m.gguf|https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf|2104932768|626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d|Qwen Research (non-commercial)|3000|Better answers; check the licence before commercial use
llama3.2-1b|Llama-3.2-1B-Instruct-Q4_K_M.gguf|https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/main/Llama-3.2-1B-Instruct-Q4_K_M.gguf|807694464|6f85a640a97cf2bf5b8e764087b1e83da0fdb51d7c9fab7d0fece9385611df83|Llama 3.2 Community|1200|Smallest; for very low memory
llama3.2-3b|Llama-3.2-3B-Instruct-Q4_K_M.gguf|https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf|2019377696|6c1a2b41161032677be168d354123594c0e6e67d2b9227c84f296ad037c728ff|Llama 3.2 Community|2800|Good all-rounder
";

/// Whether `host` is a server the downloader may contact: Hugging Face and its content hosts.
/// The same rule is applied to every redirect.
#[must_use]
pub fn allowed_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    ["huggingface.co", "hf.co"]
        .iter()
        .any(|d| h == *d || h.ends_with(&format!(".{d}")))
}

/// Whether `s` is a SHA-256 in lower-case hex.
#[must_use]
pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// A safe file name: letters, digits, `.`, `-`, `_`; no leading dot; ends with `.gguf`.
#[must_use]
pub fn valid_file_name(s: &str) -> bool {
    s.len() <= 100
        && s.len() > ".gguf".len()
        && s.ends_with(".gguf")
        && !s.starts_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

fn valid_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && s.chars().all(|c| !c.is_control())
}

/// The host part of an `https://` address, if it is one.
#[must_use]
pub fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host_port = rest.split(['/', '?', '#']).next()?;
    if host_port.contains('@') || host_port.is_empty() {
        return None;
    }
    let host = host_port.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Reads a list.
///
/// # Errors
/// A message naming the line and the problem. One bad line refuses the whole list, so a damaged
/// list is never half used.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    if text.len() > MAX_LIST_BYTES {
        return Err("the model list is too large".into());
    }
    let mut out: Vec<Entry> = Vec::new();
    let mut ids = BTreeSet::new();
    let mut files = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |m: &str| format!("model list line {}: {m}", n + 1);
        let f: Vec<&str> = line.split('|').collect();
        if f.len() != 8 {
            return Err(at("expected 8 fields"));
        }
        let (id, file, url, size, sha, licence, ram, note) =
            (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
        if !valid_id(id) {
            return Err(at("bad id"));
        }
        if !valid_file_name(file) {
            return Err(at("bad file name"));
        }
        match https_host(url) {
            Some(h) if allowed_host(&h) => {}
            Some(_) => return Err(at("address is not on an allowed server")),
            None => return Err(at("address must be https")),
        }
        if url.len() > 400 || url.chars().any(|c| c.is_control() || c == ' ') {
            return Err(at("bad address"));
        }
        let size: u64 = size.parse().map_err(|_| at("bad size"))?;
        if !(MIN_MODEL_BYTES..=MAX_MODEL_BYTES).contains(&size) {
            return Err(at("size out of range"));
        }
        if !valid_sha256(sha) {
            return Err(at("sha256 must be 64 lower-case hex characters"));
        }
        if !valid_text(licence, 60) || !valid_text(note, 120) {
            return Err(at("bad licence or note"));
        }
        let min_ram_mb: u64 = ram.parse().map_err(|_| at("bad memory figure"))?;
        if !(64..=1_000_000).contains(&min_ram_mb) {
            return Err(at("memory figure out of range"));
        }
        if !ids.insert(id.to_string()) || !files.insert(file.to_ascii_lowercase()) {
            return Err(at("duplicate id or file name"));
        }
        if out.len() >= MAX_ENTRIES {
            return Err(at("too many entries"));
        }
        out.push(Entry {
            id: id.into(),
            file: file.into(),
            url: url.into(),
            size,
            sha256: sha.into(),
            licence: licence.into(),
            min_ram_mb,
            note: note.into(),
        });
    }
    if out.is_empty() {
        return Err("the model list is empty".into());
    }
    Ok(out)
}

/// The list to use: the file at `path` when it is there and valid, else the built-in list.
/// The second value says when the file was present but refused, so it can be reported.
#[must_use]
pub fn load(path: &std::path::Path) -> (Vec<Entry>, Option<String>) {
    let built_in = || parse(BUILT_IN).unwrap_or_default();
    match std::fs::read_to_string(path) {
        Ok(text) => match parse(&text) {
            Ok(list) => (list, None),
            Err(e) => (
                built_in(),
                Some(format!("ignoring {}: {e}", path.display())),
            ),
        },
        Err(_) => (built_in(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e";

    fn good() -> Vec<String> {
        vec![
            "m1".into(),
            "model.gguf".into(),
            "https://huggingface.co/o/r/resolve/main/model.gguf".into(),
            "5000000".into(),
            SHA.into(),
            "MIT".into(),
            "1000".into(),
            "a note".into(),
        ]
    }

    fn with(i: usize, v: &str) -> String {
        let mut g = good();
        g[i] = v.into();
        g.join("|")
    }

    #[test]
    fn the_built_in_list_is_valid_and_has_the_four_models() {
        let l = parse(BUILT_IN).unwrap();
        assert_eq!(l.len(), 4);
        assert!(l
            .iter()
            .all(|e| valid_sha256(&e.sha256) && e.size > 700_000_000));
        assert!(l.iter().any(|e| e.id == "qwen2.5-1.5b"));
    }

    #[test]
    fn a_good_line_is_read_field_by_field() {
        let l = parse(&good().join("|")).unwrap();
        assert_eq!(l[0].id, "m1");
        assert_eq!(l[0].size, 5_000_000);
        assert_eq!(l[0].min_ram_mb, 1000);
    }

    #[test]
    fn every_kind_of_bad_line_refuses_the_list() {
        for (i, bad) in [
            (0, "Bad Id"),
            (0, ""),
            (0, "x".repeat(33).as_str().to_string().as_str()),
            (1, "../model.gguf"),
            (1, "model.bin"),
            (1, ".hidden.gguf"),
            (1, "a b.gguf"),
            (1, ".gguf"),
            (2, "http://huggingface.co/x/model.gguf"),
            (2, "https://evil.example.com/model.gguf"),
            (2, "https://huggingface.co.evil.com/model.gguf"),
            (2, "https://user@huggingface.co/model.gguf"),
            (2, "ftp://huggingface.co/model.gguf"),
            (2, "https://huggingface.co/a b"),
            (3, "abc"),
            (3, "999"),
            (3, "99999999999999"),
            (3, "-5"),
            (
                4,
                "6A1A2EB6D15622BF3C96857206351BA97E1AF16C30D7A74EE38970E434E9407E",
            ),
            (4, "6a1a"),
            (4, ""),
            (5, ""),
            (5, "bad\x07licence"),
            (6, "10"),
            (6, "x"),
            (7, ""),
        ] {
            assert!(parse(&with(i, bad)).is_err(), "field {i}: {bad:?}");
        }
    }

    #[test]
    fn a_line_with_the_wrong_number_of_fields_is_refused() {
        assert!(parse("a|b|c").is_err());
        assert!(parse(&format!("{}|extra", good().join("|"))).is_err());
    }

    #[test]
    fn duplicates_and_empty_lists_are_refused() {
        let one = good().join("|");
        assert!(parse(&format!("{one}\n{one}")).is_err());
        let mut other = good();
        other[0] = "m2".into();
        assert!(
            parse(&format!("{one}\n{}", other.join("|"))).is_err(),
            "same file name"
        );
        assert!(parse("").is_err());
        assert!(parse("# only a comment\n\n").is_err());
    }

    #[test]
    fn comments_and_blank_lines_are_skipped_and_size_is_bounded() {
        let one = good().join("|");
        assert_eq!(parse(&format!("# c\n\n{one}\n")).unwrap().len(), 1);
        assert!(parse(&"x".repeat(MAX_LIST_BYTES + 1)).is_err());
        let mut many = String::new();
        for i in 0..(MAX_ENTRIES + 1) {
            let mut g = good();
            g[0] = format!("m{i}");
            g[1] = format!("model{i}.gguf");
            many.push_str(&g.join("|"));
            many.push('\n');
        }
        assert!(parse(&many).is_err());
    }

    #[test]
    fn only_hugging_face_hosts_are_allowed() {
        for ok in [
            "huggingface.co",
            "cdn-lfs.huggingface.co",
            "us.aws.cdn.hf.co",
            "hf.co",
            "HuggingFace.co",
        ] {
            assert!(allowed_host(ok), "{ok}");
        }
        for bad in [
            "evil.com",
            "huggingface.co.evil.com",
            "nothf.co",
            "xhuggingface.co",
            "",
            "hf.co.attacker.net",
        ] {
            assert!(!allowed_host(bad), "{bad}");
        }
    }

    #[test]
    fn the_host_is_taken_from_an_https_address() {
        assert_eq!(
            https_host("https://huggingface.co/a/b").as_deref(),
            Some("huggingface.co")
        );
        assert_eq!(https_host("https://HF.co:443/x").as_deref(), Some("hf.co"));
        assert_eq!(https_host("http://hf.co/x"), None);
        assert_eq!(https_host("https://a@hf.co/x"), None);
        assert_eq!(https_host("https:///x"), None);
    }

    #[test]
    fn a_missing_file_gives_the_built_in_list_and_a_bad_one_says_so() {
        let dir = std::env::temp_dir().join(format!("moulid-cat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (l, why) = load(&dir.join("none"));
        assert_eq!(l.len(), 4);
        assert!(why.is_none());
        std::fs::write(dir.join("bad"), "garbage").unwrap();
        let (l, why) = load(&dir.join("bad"));
        assert_eq!(l.len(), 4, "falls back to the built-in list");
        assert!(why.unwrap().contains("ignoring"));
        std::fs::write(dir.join("ok"), good().join("|")).unwrap();
        let (l, _) = load(&dir.join("ok"));
        assert_eq!(l.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
