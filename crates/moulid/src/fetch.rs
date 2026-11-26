//! Downloading a catalogue model: redirects, resume, verification.
//!
//! The planning rules live in [`crate::download_plan`] (ranges, size checks, hash verification);
//! this module does the work around them: it opens the connection through an [`Fetcher`], follows
//! redirects itself under a strict policy (https only, hosts must be allowed, at most
//! [`MAX_REDIRECTS`]), writes to a hidden `.part` file, resumes after a broken connection, and only
//! renames the file into place once its size and SHA-256 match the catalogue entry. Nothing the
//! server says about size or hash is trusted: both come from the [`Entry`].

use crate::catalog::{allowed_host, Entry};
use crate::download_plan::{
    check_response, check_space, progress, range_header, DownloadError, Hasher, Plan, Progress,
    ResumeAction, Verifier,
};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How many redirects are followed for one request.
pub const MAX_REDIRECTS: usize = 5;
/// How many times a broken connection is resumed automatically.
pub const MAX_RESUMES: u32 = 3;
/// Bytes read and written at a time.
pub const CHUNK: usize = 64 * 1024;
/// How often the progress callback is called while data flows.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// A response: status, headers and a body to read.
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// Headers, names as sent.
    pub headers: Vec<(String, String)>,
    /// The body, read as a stream.
    pub body: Box<dyn Read>,
}

impl Reply {
    /// The first header named `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Something that can fetch an address (the real network, or a fake in tests).
pub trait Fetcher {
    /// One request, no redirect following.
    ///
    /// # Errors
    /// A message when the connection or the response failed.
    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<Reply, String>;
}

/// The real network, over `mouli-http` (HTTPS with certificate checking).
pub struct HttpFetcher {
    client: mouli_http::Client,
}

impl HttpFetcher {
    /// A fetcher that trusts the certificates in the image's CA bundle.
    ///
    /// # Errors
    /// A message when the bundle cannot be loaded.
    pub fn system() -> Result<Self, String> {
        mouli_http::Client::system()
            .map(|client| Self { client })
            .map_err(|e| format!("cannot load the trusted certificates: {e}"))
    }

    /// A fetcher over `client` (tests use their own).
    #[must_use]
    pub fn new(client: mouli_http::Client) -> Self {
        Self { client }
    }
}

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<Reply, String> {
        let parsed = mouli_http::Url::parse(url).map_err(|e| format!("bad address: {e}"))?;
        let mut req = mouli_http::Request::get(parsed);
        for (n, v) in headers {
            req = req.header(n, v);
        }
        let r = self
            .client
            .request(&req)
            .map_err(|e| format!("connection failed: {e}"))?;
        let mouli_http::Response {
            status,
            headers,
            body,
            ..
        } = r;
        Ok(Reply {
            status,
            headers,
            body: Box::new(body),
        })
    }
}

/// What the downloader is allowed to contact.
#[derive(Clone, Copy)]
pub struct Policy {
    /// Which hosts may be contacted (checked for the first address and for every redirect).
    pub allow_host: fn(&str) -> bool,
    /// Whether plain `http` to loopback addresses is allowed (tests only).
    pub allow_loopback_http: bool,
}

impl Policy {
    /// The rules used on a real system: https only, Hugging Face hosts only.
    #[must_use]
    pub fn production() -> Self {
        Self {
            allow_host: allowed_host,
            allow_loopback_http: false,
        }
    }
}

/// SHA-256 for [`Verifier`].
pub struct Sha256Hasher(Sha256);

impl Sha256Hasher {
    /// A fresh hasher.
    #[must_use]
    pub fn boxed() -> Box<dyn Hasher> {
        Box::new(Self(Sha256::new()))
    }
}

impl Hasher for Sha256Hasher {
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish_hex(self: Box<Self>) -> String {
        self.0
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// How a download ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The model was downloaded and verified.
    Downloaded(PathBuf),
    /// A verified copy was already in place.
    AlreadyHad(PathBuf),
}

/// What the person sees while a download runs.
pub struct Status {
    /// Bytes on disk so far.
    pub done: u64,
    /// Total size.
    pub total: u64,
    /// Speed and time left.
    pub progress: Progress,
}

/// Free space to keep beyond the model itself: 5 percent plus 256 MB.
#[must_use]
pub fn reserve_for(size: u64) -> u64 {
    (size / 20).saturating_add(256 * 1024 * 1024)
}

/// Free bytes on the filesystem holding `path`.
#[cfg(target_os = "linux")]
#[must_use]
#[allow(clippy::useless_conversion)] // the field types differ between platforms
pub fn free_bytes(path: &Path) -> u64 {
    nix::sys::statvfs::statvfs(path).map_or(0, |s| {
        u64::from(s.blocks_available()).saturating_mul(u64::from(s.fragment_size()))
    })
}

/// Free bytes on the filesystem holding `path` (not measured off Linux).
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn free_bytes(_path: &Path) -> u64 {
    u64::MAX
}

fn resolve_location(base: &str, location: &str) -> Result<String, String> {
    if location.len() > 2000 || location.chars().any(|c| c.is_control() || c == ' ') {
        return Err("the server sent a bad redirect".into());
    }
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_string());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        let after = base.find("://").map_or(0, |i| i + 3);
        let end = base[after..].find('/').map_or(base.len(), |i| after + i);
        return Ok(format!("{}{location}", &base[..end]));
    }
    Err("the server sent a redirect that is not understood".into())
}

fn check_target(url: &str, policy: &Policy) -> Result<(), String> {
    let parsed =
        mouli_http::Url::parse(url).map_err(|_| "a bad address was refused".to_string())?;
    let plain_ok = policy.allow_loopback_http
        && parsed.scheme == mouli_http::Scheme::Http
        && parsed.is_loopback();
    if parsed.scheme != mouli_http::Scheme::Https && !plain_ok {
        return Err("a connection that is not encrypted was refused".into());
    }
    if !plain_ok && !(policy.allow_host)(&parsed.host) {
        return Err(format!(
            "the server {} is not on the allowed list",
            parsed.host
        ));
    }
    Ok(())
}

/// Fetches `url`, following up to [`MAX_REDIRECTS`] redirects under `policy`.
///
/// # Errors
/// A message when the address or a redirect is refused, there are too many redirects, or the
/// fetcher fails.
pub fn open<F: Fetcher>(
    f: &F,
    url: &str,
    headers: &[(String, String)],
    policy: &Policy,
) -> Result<Reply, String> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        check_target(&current, policy)?;
        let reply = f.get(&current, headers)?;
        if matches!(reply.status, 301 | 302 | 303 | 307 | 308) {
            let loc = reply
                .header("location")
                .ok_or("the server redirected without saying where")?
                .to_string();
            current = resolve_location(&current, &loc)?;
            continue;
        }
        return Ok(reply);
    }
    Err("too many redirects".into())
}

fn describe(e: &DownloadError) -> String {
    match e {
        DownloadError::NoSpace { needed, free } => format!(
            "Not enough free space: {} MB needed, {} MB free.",
            needed / 1_000_000,
            free / 1_000_000
        ),
        DownloadError::HttpStatus(404) => {
            "The server does not have this file any more (404).".into()
        }
        DownloadError::HttpStatus(s) => format!("The server answered with an error ({s})."),
        DownloadError::HashMismatch { .. } => {
            "The file did not match its checksum, so it was deleted. It may be damaged or changed."
                .into()
        }
        DownloadError::WrongSize => "The file is not the expected size, so it was deleted.".into(),
        other => format!("The download was refused: {other}."),
    }
}

fn hash_file(path: &Path, v: &mut Verifier) -> io::Result<()> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        v.feed(&buf[..n]);
    }
}

fn plan_for(e: &Entry, have: u64) -> Plan {
    Plan {
        url: e.url.clone(),
        expected_size: Some(e.size),
        expected_sha256: Some(e.sha256.clone()),
        already_have: have,
    }
}

/// Whether the file at `path` has the size and hash of `e`.
///
/// # Errors
/// I/O errors reading the file.
pub fn matches_entry(path: &Path, e: &Entry) -> io::Result<bool> {
    if fs::metadata(path)?.len() != e.size {
        return Ok(false);
    }
    let mut v = Verifier::new(Sha256Hasher::boxed());
    hash_file(path, &mut v)?;
    Ok(v.finish(&plan_for(e, e.size)).is_ok())
}

fn has_gguf_magic(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"GGUF"
}

/// Downloads `e` into `dir`.
///
/// The file is written as `.<name>.part` and renamed to `<name>` only after the size and SHA-256
/// match. A broken connection is resumed with a range request up to [`MAX_RESUMES`] times, and a
/// partial file from an earlier attempt is continued. `status` is called about four times a second.
///
/// # Errors
/// A message for the person: no space, a refused address, a server error, too many broken
/// connections, a size or checksum mismatch (the partial file is deleted), or an I/O failure.
pub fn download<F: Fetcher>(
    f: &F,
    e: &Entry,
    dir: &Path,
    free: u64,
    policy: &Policy,
    status: &mut dyn FnMut(&Status),
) -> Result<Outcome, String> {
    let final_path = dir.join(&e.file);
    let part = dir.join(format!(".{}.part", e.file));
    if final_path.exists() {
        return match matches_entry(&final_path, e) {
            Ok(true) => Ok(Outcome::AlreadyHad(final_path)),
            _ => Err(format!(
                "A different file named {} is already there. Remove it first, then try again.",
                e.file
            )),
        };
    }
    let io_err = |what: &str, err: io::Error| format!("{what}: {err}");
    let mut have = fs::metadata(&part).map_or(0, |m| m.len());
    if have > e.size {
        let _ = fs::remove_file(&part);
        have = 0;
    }
    check_space(e.size - have, free, reserve_for(e.size)).map_err(|x| describe(&x))?;

    let started = Instant::now();
    let base = have;
    let mut resumes = 0u32;
    'attempt: loop {
        let plan = plan_for(e, have);
        let mut headers = vec![("User-Agent".to_string(), "MOULI".to_string())];
        if let Some(r) = range_header(&plan) {
            headers.push(("Range".into(), r));
        }
        let mut reply = match open(f, &e.url, &headers, policy) {
            Ok(r) => r,
            Err(msg) => {
                if resumes < MAX_RESUMES && have > 0 {
                    resumes += 1;
                    continue 'attempt;
                }
                return Err(msg);
            }
        };
        let length = reply
            .header("content-length")
            .and_then(|v| v.trim().parse().ok());
        let action =
            match check_response(&plan, reply.status, reply.header("content-range"), length) {
                Ok(a) => a,
                Err(DownloadError::AlreadyComplete) if have == e.size => ResumeAction::Append {
                    from: have,
                    remaining: Some(0),
                },
                Err(x) => return Err(describe(&x)),
            };
        let mut verifier = Verifier::new(Sha256Hasher::boxed());
        let mut out = match action {
            ResumeAction::Append { .. } => {
                if have > 0 {
                    hash_file(&part, &mut verifier)
                        .map_err(|x| io_err("cannot read the partial file", x))?;
                }
                OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&part)
                    .map_err(|x| io_err("cannot write the model file", x))?
            }
            ResumeAction::Restart { .. } => {
                have = 0;
                File::create(&part).map_err(|x| io_err("cannot write the model file", x))?
            }
        };
        let mut buf = vec![0u8; CHUNK];
        let mut last_report = Instant::now();
        loop {
            let n = match reply.body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => {
                    // The connection broke: keep what was written and ask for the rest.
                    let _ = out.flush();
                    if resumes < MAX_RESUMES {
                        resumes += 1;
                        have = fs::metadata(&part).map_or(have, |m| m.len());
                        continue 'attempt;
                    }
                    return Err("The connection kept breaking. Try again; the download will continue where it stopped.".into());
                }
            };
            if have + n as u64 > e.size {
                drop(out);
                let _ = fs::remove_file(&part);
                return Err("The server sent more data than expected, so the download was stopped and deleted.".into());
            }
            out.write_all(&buf[..n])
                .map_err(|x| io_err("cannot write the model file (is the disk full?)", x))?;
            verifier.feed(&buf[..n]);
            have += n as u64;
            if last_report.elapsed() >= PROGRESS_EVERY {
                last_report = Instant::now();
                status(&Status {
                    done: have,
                    total: e.size,
                    progress: progress(have - base, Some(e.size - base), elapsed_ms(started)),
                });
            }
        }
        out.flush()
            .map_err(|x| io_err("cannot write the model file", x))?;
        if have < e.size {
            if resumes < MAX_RESUMES {
                resumes += 1;
                continue 'attempt;
            }
            return Err(
                "The server stopped sending before the file was complete. Try again to continue."
                    .into(),
            );
        }
        out.sync_all()
            .map_err(|x| io_err("cannot write the model file", x))?;
        drop(out);
        if let Err(x) = verifier.finish(&plan_for(e, have)) {
            let _ = fs::remove_file(&part);
            return Err(describe(&x));
        }
        if !has_gguf_magic(&part) {
            let _ = fs::remove_file(&part);
            return Err("The file is not a GGUF model, so it was deleted.".into());
        }
        fs::rename(&part, &final_path).map_err(|x| io_err("cannot finish the download", x))?;
        status(&Status {
            done: e.size,
            total: e.size,
            progress: progress(e.size - base, Some(e.size - base), elapsed_ms(started)),
        });
        return Ok(Outcome::Downloaded(final_path));
    }
}

fn elapsed_ms(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;

    fn data(n: usize) -> Vec<u8> {
        let mut v = b"GGUF".to_vec();
        v.extend((0..n.saturating_sub(4)).map(|i| (i * 31 % 251) as u8));
        v
    }

    fn sha(d: &[u8]) -> String {
        Sha256::digest(d)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn entry(d: &[u8]) -> Entry {
        Entry {
            id: "t".into(),
            file: "t.gguf".into(),
            url: "https://huggingface.co/o/r/resolve/main/t.gguf".into(),
            size: d.len() as u64,
            sha256: sha(d),
            licence: "MIT".into(),
            min_ram_mb: 100,
            note: "test".into(),
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-fetch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A reader that gives `n` bytes then fails like a dropped connection.
    struct Breaks {
        inner: Cursor<Vec<u8>>,
        left: usize,
    }

    impl Read for Breaks {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.left == 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset"));
            }
            let take = buf.len().min(self.left);
            let n = self.inner.read(&mut buf[..take])?;
            self.left -= n;
            Ok(n)
        }
    }

    type Handler = Box<dyn Fn(&str, &[(String, String)], usize) -> Result<Reply, String>>;
    type Call = (String, Vec<(String, String)>);

    struct Fake {
        handler: Handler,
        calls: RefCell<Vec<Call>>,
    }

    impl Fake {
        fn new(handler: Handler) -> Self {
            Self {
                handler,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetcher for Fake {
        fn get(&self, url: &str, headers: &[(String, String)]) -> Result<Reply, String> {
            let i = self.calls.borrow().len();
            self.calls.borrow_mut().push((url.into(), headers.to_vec()));
            (self.handler)(url, headers, i)
        }
    }

    fn reply(status: u16, headers: &[(&str, &str)], body: Box<dyn Read>) -> Reply {
        Reply {
            status,
            headers: headers
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            body,
        }
    }

    fn range_start(h: &[(String, String)]) -> Option<usize> {
        h.iter()
            .find(|(n, _)| n == "Range")
            .and_then(|(_, v)| v.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok())
    }

    /// A server that serves `d`, honouring Range.
    fn serving(d: Vec<u8>) -> Fake {
        Fake::new(Box::new(move |_url, h, _| {
            let total = d.len();
            match range_start(h) {
                Some(s) if s < total => Ok(reply(
                    206,
                    &[
                        ("Content-Range", &format!("bytes {s}-{}/{total}", total - 1)),
                        ("Content-Length", &(total - s).to_string()),
                    ],
                    Box::new(Cursor::new(d[s..].to_vec())),
                )),
                _ => Ok(reply(
                    200,
                    &[("Content-Length", &total.to_string())],
                    Box::new(Cursor::new(d.clone())),
                )),
            }
        }))
    }

    fn run(f: &Fake, e: &Entry, dir: &Path, free: u64) -> Result<Outcome, String> {
        download(f, e, dir, free, &Policy::production(), &mut |_| {})
    }

    const FREE: u64 = 10 * 1024 * 1024 * 1024;

    #[test]
    fn a_clean_download_is_verified_and_renamed_into_place() {
        let d = data(300_000);
        let dir = tmp("ok");
        let e = entry(&d);
        let f = serving(d.clone());
        let out = run(&f, &e, &dir, FREE).unwrap();
        assert_eq!(out, Outcome::Downloaded(dir.join("t.gguf")));
        assert_eq!(fs::read(dir.join("t.gguf")).unwrap(), d);
        assert!(
            !dir.join(".t.gguf.part").exists(),
            "no partial file is left"
        );
        let sent = &f.calls.borrow()[0].1;
        assert!(sent.iter().any(|(n, v)| n == "User-Agent" && v == "MOULI"));
        assert!(
            !sent
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("authorization")),
            "no credentials"
        );
        // Second time: the verified copy is recognised and nothing is fetched.
        let again = serving(d);
        assert_eq!(
            run(&again, &e, &dir, FREE).unwrap(),
            Outcome::AlreadyHad(dir.join("t.gguf"))
        );
        assert!(again.calls.borrow().is_empty());
    }

    #[test]
    fn an_existing_different_file_is_not_overwritten() {
        let d = data(100_000);
        let dir = tmp("exists");
        fs::write(dir.join("t.gguf"), b"my own file").unwrap();
        let f = serving(d.clone());
        let e = run(&f, &entry(&d), &dir, FREE).unwrap_err();
        assert!(e.contains("already there"), "{e}");
        assert_eq!(fs::read(dir.join("t.gguf")).unwrap(), b"my own file");
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn redirects_are_followed_with_the_range_kept_and_no_credentials() {
        let d = data(200_000);
        let dir = tmp("redir");
        let e = entry(&d);
        let inner = serving(d.clone());
        let f = Fake::new(Box::new(move |url, h, i| match i {
            0 => Ok(reply(
                302,
                &[("Location", "https://us.aws.cdn.hf.co/blob?sig=1")],
                Box::new(Cursor::new(vec![])),
            )),
            1 => Ok(reply(
                307,
                &[("Location", "/next/part")],
                Box::new(Cursor::new(vec![])),
            )),
            _ => inner.get(url, h),
        }));
        run(&f, &e, &dir, FREE).unwrap();
        let calls = f.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].0, "https://us.aws.cdn.hf.co/blob?sig=1");
        assert_eq!(
            calls[2].0, "https://us.aws.cdn.hf.co/next/part",
            "relative redirect keeps the host"
        );
        assert!(calls.iter().all(|(_, h)| !h
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("authorization"))));
    }

    #[test]
    fn hostile_redirects_are_refused() {
        let d = data(100_000);
        let e = entry(&d);
        for (target, why) in [
            ("http://huggingface.co/x", "not encrypted"),
            ("https://evil.example.com/x", "not on the allowed list"),
            ("ftp://huggingface.co/x", "not understood"),
            ("//evil.example.com/x", "not understood"),
            ("javascript:alert(1)", "not understood"),
            ("https://huggingface.co/a b", "bad redirect"),
        ] {
            let dir = tmp("hostile");
            let t = target.to_string();
            let f = Fake::new(Box::new(move |_, _, _| {
                Ok(reply(
                    302,
                    &[("Location", &t)],
                    Box::new(Cursor::new(vec![])),
                ))
            }));
            let err = run(&f, &e, &dir, FREE).unwrap_err();
            assert!(err.contains(why), "{target}: {err}");
            assert_eq!(
                f.calls.borrow().len(),
                1,
                "{target}: only the first request was made"
            );
            assert!(!dir.join("t.gguf").exists());
        }
    }

    #[test]
    fn an_address_outside_the_allowed_list_is_refused_before_connecting() {
        let d = data(100_000);
        let mut e = entry(&d);
        e.url = "https://evil.example.com/t.gguf".into();
        let f = serving(d);
        let err = run(&f, &e, &tmp("host"), FREE).unwrap_err();
        assert!(err.contains("not on the allowed list"), "{err}");
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn redirect_loops_and_missing_locations_stop() {
        let d = data(100_000);
        let e = entry(&d);
        let looping = Fake::new(Box::new(|_, _, _| {
            Ok(reply(
                302,
                &[("Location", "https://huggingface.co/again")],
                Box::new(Cursor::new(vec![])),
            ))
        }));
        let err = run(&looping, &e, &tmp("loop"), FREE).unwrap_err();
        assert!(err.contains("too many redirects"), "{err}");
        assert_eq!(looping.calls.borrow().len(), MAX_REDIRECTS + 1);
        let nowhere = Fake::new(Box::new(|_, _, _| {
            Ok(reply(302, &[], Box::new(Cursor::new(vec![]))))
        }));
        assert!(run(&nowhere, &e, &tmp("noloc"), FREE).is_err());
    }

    #[test]
    fn a_dropped_connection_is_resumed_with_a_range_request() {
        let d = data(400_000);
        let dir = tmp("resume");
        let e = entry(&d);
        let data2 = d.clone();
        let f = Fake::new(Box::new(move |_, h, i| {
            let total = data2.len();
            let s = range_start(h).unwrap_or(0);
            let (status, mut hd) = if s > 0 {
                (
                    206,
                    vec![(
                        "Content-Range".to_string(),
                        format!("bytes {s}-{}/{total}", total - 1),
                    )],
                )
            } else {
                (200, vec![])
            };
            hd.push(("Content-Length".into(), (total - s).to_string()));
            let hd: Vec<(&str, &str)> = hd.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let body = Breaks {
                inner: Cursor::new(data2[s..].to_vec()),
                left: if i == 0 { 150_000 } else { usize::MAX },
            };
            Ok(reply(status, &hd, Box::new(body)))
        }));
        run(&f, &e, &dir, FREE).unwrap();
        assert_eq!(fs::read(dir.join("t.gguf")).unwrap(), d);
        let calls = f.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(
            range_start(&calls[1].1).unwrap() > 0,
            "the second request asks only for the rest"
        );
    }

    #[test]
    fn a_partial_file_from_an_earlier_session_is_continued() {
        let d = data(300_000);
        let dir = tmp("partial");
        fs::write(dir.join(".t.gguf.part"), &d[..120_000]).unwrap();
        let f = serving(d.clone());
        run(&f, &entry(&d), &dir, FREE).unwrap();
        assert_eq!(fs::read(dir.join("t.gguf")).unwrap(), d);
        assert_eq!(range_start(&f.calls.borrow()[0].1), Some(120_000));
    }

    #[test]
    fn a_server_that_ignores_the_range_restarts_cleanly() {
        let d = data(250_000);
        let dir = tmp("ignore");
        fs::write(dir.join(".t.gguf.part"), &d[..50_000]).unwrap();
        let data2 = d.clone();
        let f = Fake::new(Box::new(move |_, _, _| {
            Ok(reply(
                200,
                &[("Content-Length", &data2.len().to_string())],
                Box::new(Cursor::new(data2.clone())),
            ))
        }));
        run(&f, &entry(&d), &dir, FREE).unwrap();
        assert_eq!(fs::read(dir.join("t.gguf")).unwrap(), d);
    }

    #[test]
    fn a_wrong_content_range_is_refused() {
        let d = data(250_000);
        let dir = tmp("badrange");
        fs::write(dir.join(".t.gguf.part"), &d[..50_000]).unwrap();
        let data2 = d.clone();
        let f = Fake::new(Box::new(move |_, _, _| {
            Ok(reply(
                206,
                &[
                    ("Content-Range", "bytes 0-249999/250000"),
                    ("Content-Length", "250000"),
                ],
                Box::new(Cursor::new(data2.clone())),
            ))
        }));
        assert!(run(&f, &entry(&d), &dir, FREE).is_err());
        assert!(!dir.join("t.gguf").exists());
    }

    #[test]
    fn a_changed_byte_fails_the_checksum_and_the_file_is_deleted() {
        let d = data(200_000);
        let mut bad = d.clone();
        bad[100_000] ^= 1;
        let dir = tmp("flip");
        let f = serving(bad);
        let err = run(&f, &entry(&d), &dir, FREE).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        assert!(!dir.join("t.gguf").exists());
        assert!(
            !dir.join(".t.gguf.part").exists(),
            "the bad partial file is deleted"
        );
    }

    #[test]
    fn too_much_data_stops_and_too_little_is_reported() {
        let d = data(200_000);
        let e = entry(&d);
        let mut long = d.clone();
        long.extend([0u8; 5000]);
        let dir = tmp("long");
        let longer = Fake::new(Box::new(move |_, _, _| {
            Ok(reply(200, &[], Box::new(Cursor::new(long.clone()))))
        }));
        let err = run(&longer, &e, &dir, FREE).unwrap_err();
        assert!(err.contains("more data than expected"), "{err}");
        assert!(!dir.join(".t.gguf.part").exists());
        let short = d[..100_000].to_vec();
        let dir2 = tmp("short");
        let shorter = Fake::new(Box::new(move |_, _, _| {
            Ok(reply(200, &[], Box::new(Cursor::new(short.clone()))))
        }));
        let err = run(&shorter, &e, &dir2, FREE).unwrap_err();
        assert!(err.contains("stopped sending"), "{err}");
        assert!(
            dir2.join(".t.gguf.part").exists(),
            "kept so the next try can continue"
        );
        assert!(!dir2.join("t.gguf").exists());
    }

    #[test]
    fn a_connection_that_always_breaks_gives_up_after_the_limit() {
        let d = data(300_000);
        let dir = tmp("breaks");
        let data2 = d.clone();
        let f = Fake::new(Box::new(move |_, h, _| {
            let s = range_start(h).unwrap_or(0);
            let total = data2.len();
            let hd: Vec<(String, String)> = if s > 0 {
                vec![(
                    "Content-Range".into(),
                    format!("bytes {s}-{}/{total}", total - 1),
                )]
            } else {
                vec![]
            };
            let hd: Vec<(&str, &str)> = hd.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            Ok(reply(
                if s > 0 { 206 } else { 200 },
                &hd,
                Box::new(Breaks {
                    inner: Cursor::new(data2[s..].to_vec()),
                    left: 10_000,
                }),
            ))
        }));
        let err = run(&f, &entry(&d), &dir, FREE).unwrap_err();
        assert!(err.contains("kept breaking"), "{err}");
        assert_eq!(f.calls.borrow().len(), MAX_RESUMES as usize + 1);
    }

    #[test]
    fn errors_from_the_server_and_a_full_disk_are_explained() {
        let d = data(100_000);
        let e = entry(&d);
        let nf = Fake::new(Box::new(|_, _, _| {
            Ok(reply(404, &[], Box::new(Cursor::new(vec![]))))
        }));
        assert!(run(&nf, &e, &tmp("404"), FREE).unwrap_err().contains("404"));
        let sv = Fake::new(Box::new(|_, _, _| {
            Ok(reply(503, &[], Box::new(Cursor::new(vec![]))))
        }));
        assert!(run(&sv, &e, &tmp("503"), FREE).unwrap_err().contains("503"));
        let f = serving(d);
        let err = run(&f, &e, &tmp("space"), 1_000_000).unwrap_err();
        assert!(err.contains("Not enough free space"), "{err}");
        assert!(
            f.calls.borrow().is_empty(),
            "nothing is fetched when there is no room"
        );
    }

    #[test]
    fn a_file_that_is_not_gguf_is_deleted() {
        let mut d = data(100_000);
        d[0] = b'X';
        let dir = tmp("magic");
        let f = serving(d.clone());
        let err = run(&f, &entry(&d), &dir, FREE).unwrap_err();
        assert!(err.contains("not a GGUF"), "{err}");
        assert!(!dir.join("t.gguf").exists() && !dir.join(".t.gguf.part").exists());
    }

    #[test]
    fn progress_is_reported_and_ends_at_the_full_size() {
        let d = data(2_000_000);
        let dir = tmp("progress");
        let f = serving(d.clone());
        let mut last = 0;
        download(
            &f,
            &entry(&d),
            &dir,
            FREE,
            &Policy::production(),
            &mut |s| {
                assert!(s.done >= last && s.done <= s.total);
                last = s.done;
            },
        )
        .unwrap();
        assert_eq!(last, d.len() as u64);
    }

    #[test]
    fn the_sha256_helper_matches_a_known_value() {
        let mut h = Sha256Hasher::boxed();
        h.update(b"abc");
        assert_eq!(
            h.finish_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_reserve_and_free_space_helpers_are_sane() {
        assert_eq!(reserve_for(0), 256 * 1024 * 1024);
        assert!(reserve_for(2_000_000_000) > 2_000_000_000 / 20);
        assert!(free_bytes(&std::env::temp_dir()) > 0);
    }

    // ----- the real client against a real (loopback) server -----

    use std::net::TcpListener;

    fn serve_once(
        listener: &TcpListener,
        body: &[u8],
        redirect_to: Option<String>,
    ) -> Option<String> {
        let (mut s, _) = listener.accept().ok()?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut req = Vec::new();
        let mut b = [0u8; 1];
        while !req.ends_with(b"\r\n\r\n") {
            if s.read(&mut b).ok()? == 0 {
                return None;
            }
            req.push(b[0]);
        }
        let text = String::from_utf8_lossy(&req).to_string();
        let range = text
            .lines()
            .find_map(|l| {
                l.strip_prefix("Range: bytes=")
                    .or_else(|| l.strip_prefix("range: bytes="))
            })
            .and_then(|r| r.trim().strip_suffix('-')?.parse::<usize>().ok());
        if let Some(to) = redirect_to {
            let _ = write!(s, "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return Some(text);
        }
        match range {
            Some(st) if st < body.len() => {
                let _ = write!(
                    s,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {st}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len() - 1,
                    body.len(),
                    body.len() - st
                );
                let _ = s.write_all(&body[st..]);
            }
            _ => {
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(body);
            }
        }
        Some(text)
    }

    #[test]
    fn the_real_client_follows_a_redirect_and_resumes_over_loopback() {
        let d = data(600_000);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = d.clone();
        let server = std::thread::spawn(move || {
            // 1: redirect, 2: the file (served in full).
            let _ = serve_once(
                &listener,
                &body,
                Some(format!("http://127.0.0.1:{port}/file")),
            );
            serve_once(&listener, &body, None)
        });
        let dir = tmp("real");
        let mut e = entry(&d);
        e.url = format!("http://127.0.0.1:{port}/start.gguf");
        let fetcher =
            HttpFetcher::new(mouli_http::Client::new(mouli_pki::verify::TrustStore::new()));
        let policy = Policy {
            allow_host: |_| true,
            allow_loopback_http: true,
        };
        let out = download(&fetcher, &e, &dir, FREE, &policy, &mut |_| {}).unwrap();
        assert_eq!(out, Outcome::Downloaded(dir.join("t.gguf")));
        assert_eq!(fs::read(dir.join("t.gguf")).unwrap(), d);
        let second_request = server.join().unwrap().unwrap();
        assert!(second_request.starts_with("GET /file"), "{second_request}");
        // Plain http to anything but loopback is refused by the production policy.
        let strict = Policy::production();
        let err = download(&fetcher, &e, &tmp("real2"), FREE, &strict, &mut |_| {}).unwrap_err();
        assert!(err.contains("not encrypted"), "{err}");
    }
}
