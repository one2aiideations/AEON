//! Running a command, such as the tests, in a throwaway copy of the workspace, with a time limit
//! and a cap on how much output is kept.
//!
//! The copy protects the real folder from what the command does to files: a test that deletes or
//! rewrites things only harms the copy, which is removed afterwards. It is NOT a security
//! boundary. The command runs as the same user, with the same network and the rest of the
//! machine in reach; nothing here uses namespaces or seccomp. Run only commands you would run
//! yourself.
//!
//! What is copied: regular files and folders. Symbolic links and other special files are skipped
//! (and listed), as are `.git`, `target` and `node_modules`. The copy is refused when it would
//! pass [`Limits::max_files`] or [`Limits::max_bytes`].
//!
//! The command gets an empty environment except `PATH`, with `HOME` and `TMPDIR` pointing into the
//! copy, no input, and its own process group so the whole group can be stopped at the time limit.
//! Stopping the group uses the system's `kill` program; where there is none, only the command
//! itself is stopped.

use crate::workspace::{Workspace, WorkspaceError};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Folders never copied.
const SKIPPED_DIRS: [&str; 3] = [".git", "target", "node_modules"];

/// Limits on a sandbox and on one run in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long a command may run before it is stopped.
    pub timeout: Duration,
    /// Most bytes kept of each of standard output and standard error.
    pub max_output: usize,
    /// Most files copied into the sandbox.
    pub max_files: usize,
    /// Most bytes copied into the sandbox.
    pub max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_output: 64 * 1024,
            max_files: 20_000,
            max_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Why a sandbox could not be made.
#[derive(Debug)]
pub enum SandboxError {
    /// The source is not a readable folder, or a copy step failed.
    Io {
        /// What was being done.
        action: String,
        /// The underlying error.
        source: io::Error,
    },
    /// The source has more files or bytes than the limits allow.
    TooLarge,
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::Io { action, source } => write!(f, "{action}: {source}"),
            SandboxError::TooLarge => write!(f, "the folder is larger than the sandbox limits"),
        }
    }
}

impl std::error::Error for SandboxError {}

fn io_err(action: impl Into<String>, source: io::Error) -> SandboxError {
    SandboxError::Io {
        action: action.into(),
        source,
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The command ended with this exit code.
    Exited(i32),
    /// The command was ended by a signal it did not choose.
    Signalled,
    /// The command was stopped at the time limit.
    TimedOut,
    /// The command could not be started (the message says why).
    SpawnFailed(String),
}

/// The result of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// How it ended.
    pub status: Status,
    /// What it wrote to standard output (at most [`Limits::max_output`] bytes, lossily decoded).
    pub stdout: String,
    /// What it wrote to standard error (same limit).
    pub stderr: String,
    /// Whether either stream had more than was kept.
    pub truncated: bool,
    /// How long it ran.
    pub elapsed: Duration,
}

impl Run {
    /// True when the command ended with exit code 0 and was not stopped.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.status == Status::Exited(0)
    }
}

/// A throwaway copy of a folder. The copy is removed when this is dropped.
#[derive(Debug)]
pub struct Sandbox {
    root: PathBuf,
    /// Paths that were not copied (links, special files), relative to the source.
    pub skipped: Vec<String>,
}

fn unique_dir() -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "mouli-sandbox-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

struct Copier<'a> {
    limits: &'a Limits,
    files: usize,
    bytes: u64,
    skipped: Vec<String>,
}

impl Copier<'_> {
    fn copy_dir(&mut self, from: &Path, to: &Path, rel: &str) -> Result<(), SandboxError> {
        let entries =
            fs::read_dir(from).map_err(|e| io_err(format!("read {}", from.display()), e))?;
        let mut names: Vec<_> = entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io_err(format!("read {}", from.display()), e))?;
        names.sort_by_key(fs::DirEntry::file_name);
        for entry in names {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let kind = entry
                .file_type()
                .map_err(|e| io_err(format!("inspect {child_rel}"), e))?;
            if kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
                self.skipped.push(child_rel);
                continue;
            }
            if kind.is_dir() {
                if SKIPPED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                let target = to.join(&name);
                fs::create_dir(&target).map_err(|e| io_err(format!("create {child_rel}"), e))?;
                self.copy_dir(&entry.path(), &target, &child_rel)?;
            } else {
                let len = entry
                    .metadata()
                    .map_err(|e| io_err(format!("inspect {child_rel}"), e))?
                    .len();
                self.files += 1;
                self.bytes = self.bytes.saturating_add(len);
                if self.files > self.limits.max_files || self.bytes > self.limits.max_bytes {
                    return Err(SandboxError::TooLarge);
                }
                fs::copy(entry.path(), to.join(&name))
                    .map_err(|e| io_err(format!("copy {child_rel}"), e))?;
            }
        }
        Ok(())
    }
}

impl Sandbox {
    /// Copies `source` into a new folder under the system's temporary directory.
    ///
    /// # Errors
    /// [`SandboxError::TooLarge`] past the limits; [`SandboxError::Io`] when something cannot be
    /// read or written (a partly made copy is removed).
    pub fn copy_of(source: &Path, limits: &Limits) -> Result<Sandbox, SandboxError> {
        let source = fs::canonicalize(source)
            .map_err(|e| io_err(format!("open {}", source.display()), e))?;
        if !source.is_dir() {
            return Err(io_err(
                format!("open {}", source.display()),
                io::Error::other("not a directory"),
            ));
        }
        let root = unique_dir();
        make_private_dir(&root).map_err(|e| io_err(format!("create {}", root.display()), e))?;
        let mut copier = Copier {
            limits,
            files: 0,
            bytes: 0,
            skipped: Vec::new(),
        };
        let mut sandbox = Sandbox {
            root: root.clone(),
            skipped: Vec::new(),
        };
        // `sandbox` removes the folder if the copy fails and it is dropped by the `?`.
        copier.copy_dir(&source, &root, "")?;
        sandbox.skipped = copier.skipped;
        Ok(sandbox)
    }

    /// The folder holding the copy.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The copy as a [`Workspace`], so a patch can be applied to it.
    ///
    /// # Errors
    /// [`WorkspaceError`] when the folder cannot be opened.
    pub fn workspace(&self) -> Result<Workspace, WorkspaceError> {
        Workspace::open(&self.root)
    }

    /// Runs `program args...` in the copy under `limits`. Never panics and never blocks past the
    /// time limit by more than a moment.
    #[must_use]
    pub fn run(&self, program: &str, args: &[&str], limits: &Limits) -> Run {
        let start = Instant::now();
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.root)
            .env("TMPDIR", &self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Run {
                    status: Status::SpawnFailed(e.to_string()),
                    stdout: String::new(),
                    stderr: String::new(),
                    truncated: false,
                    elapsed: start.elapsed(),
                }
            }
        };
        let cap = limits.max_output;
        let (tx, rx) = mpsc::channel();
        for (which, pipe) in [
            (
                0u8,
                child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
            ),
            (
                1u8,
                child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
            ),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send((
                    which,
                    pipe.map_or((Vec::new(), false), |p| read_capped(p, cap)),
                ));
            });
        }
        drop(tx);
        let deadline = start + limits.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => {
                    break st.code().map_or(Status::Signalled, Status::Exited);
                }
                Ok(None) if Instant::now() >= deadline => {
                    stop_group(&mut child);
                    break Status::TimedOut;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => {
                    stop_group(&mut child);
                    break Status::Signalled;
                }
            }
        };
        // The readers finish when the command's output pipes close. A grandchild that outlived
        // the command can keep them open, so wait only a moment and keep what has arrived.
        let mut out = [(Vec::new(), false), (Vec::new(), false)];
        let wait_until = Instant::now() + Duration::from_millis(1500);
        let mut got = 0;
        while got < 2 {
            let left = wait_until.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((which, data)) => {
                    out[usize::from(which)] = data;
                    got += 1;
                }
                Err(_) => break,
            }
        }
        let truncated = out[0].1 || out[1].1;
        Run {
            status,
            stdout: String::from_utf8_lossy(&out[0].0).into_owned(),
            stderr: String::from_utf8_lossy(&out[1].0).into_owned(),
            truncated,
            elapsed: start.elapsed(),
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn make_private_dir(path: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(path)
}

/// Reads to the end, keeping at most `cap` bytes; returns them and whether more was dropped.
fn read_capped(mut pipe: Box<dyn Read + Send>, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut dropped = false;
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                if n > room {
                    dropped = true;
                }
            }
        }
    }
    (kept, dropped)
}

/// Stops the command and everything it started (best effort, see the module notes).
fn stop_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let group = format!("-{}", child.id());
        let _ = Command::new("kill")
            .args(["-KILL", "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick() -> Limits {
        Limits {
            timeout: Duration::from_secs(10),
            ..Limits::default()
        }
    }

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fs::create_dir_all(p.join("src/deep")).unwrap();
        fs::write(p.join("src/a.txt"), "alpha").unwrap();
        fs::write(p.join("src/deep/b.txt"), "beta").unwrap();
        fs::write(p.join("top.txt"), "top").unwrap();
        for skipped in [".git", "target", "node_modules"] {
            fs::create_dir_all(p.join(skipped)).unwrap();
            fs::write(p.join(skipped).join("big"), "should not be copied").unwrap();
        }
        d
    }

    #[test]
    fn the_copy_has_the_files_but_not_git_target_or_node_modules() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        assert_eq!(
            fs::read_to_string(sb.path().join("src/a.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(sb.path().join("src/deep/b.txt")).unwrap(),
            "beta"
        );
        assert_eq!(
            fs::read_to_string(sb.path().join("top.txt")).unwrap(),
            "top"
        );
        for skipped in [".git", "target", "node_modules"] {
            assert!(!sb.path().join(skipped).exists(), "{skipped}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed_and_are_listed() {
        let src = tree();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "do not copy").unwrap();
        std::os::unix::fs::symlink(outside.path(), src.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), src.path().join("flink"))
            .unwrap();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        assert!(!sb.path().join("link").exists() && !sb.path().join("flink").exists());
        assert_eq!(sb.skipped, vec!["flink".to_string(), "link".to_string()]);
    }

    #[test]
    fn what_a_command_does_in_the_copy_never_reaches_the_original() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let r = sb.run(
            "sh",
            &[
                "-c",
                "rm -r src; echo changed > top.txt; echo new > added.txt",
            ],
            &quick(),
        );
        assert!(r.passed(), "{r:?}");
        assert!(!sb.path().join("src").exists());
        assert_eq!(
            fs::read_to_string(src.path().join("src/a.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(src.path().join("top.txt")).unwrap(),
            "top"
        );
        assert!(!src.path().join("added.txt").exists());
    }

    #[test]
    fn the_copy_is_removed_when_dropped() {
        let src = tree();
        let path;
        {
            let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
            path = sb.path().to_path_buf();
            assert!(path.exists());
        }
        assert!(!path.exists());
    }

    #[test]
    fn a_folder_over_the_limits_is_refused_and_leaves_nothing_behind() {
        let src = tree();
        let few = Limits {
            max_files: 2,
            ..quick()
        };
        assert!(matches!(
            Sandbox::copy_of(src.path(), &few),
            Err(SandboxError::TooLarge)
        ));
        let small = Limits {
            max_bytes: 5,
            ..quick()
        };
        assert!(matches!(
            Sandbox::copy_of(src.path(), &small),
            Err(SandboxError::TooLarge)
        ));
        assert!(Sandbox::copy_of(Path::new("/definitely/not/here"), &quick()).is_err());
        let file = src.path().join("top.txt");
        assert!(Sandbox::copy_of(&file, &quick()).is_err());
    }

    #[test]
    fn exit_codes_and_both_output_streams_are_captured() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let r = sb.run("sh", &["-c", "echo out; echo err >&2; exit 3"], &quick());
        assert_eq!(r.status, Status::Exited(3));
        assert_eq!((r.stdout.as_str(), r.stderr.as_str()), ("out\n", "err\n"));
        assert!(!r.passed() && !r.truncated);
        assert!(sb.run("true", &[], &quick()).passed());
    }

    #[test]
    fn a_command_that_runs_too_long_is_stopped_promptly_even_with_children() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let limits = Limits {
            timeout: Duration::from_millis(300),
            ..quick()
        };
        for script in ["sleep 30", "sleep 30 & sleep 30"] {
            let r = sb.run("sh", &["-c", script], &limits);
            assert_eq!(r.status, Status::TimedOut, "{script}");
            assert!(
                r.elapsed < Duration::from_secs(8),
                "{script}: took {:?}",
                r.elapsed
            );
        }
    }

    /// How many processes have exactly this command line, or `None` when `pgrep` is not available.
    /// (An exact match: a shell whose arguments merely mention the text is not counted.)
    fn processes_running(command_line: &str) -> Option<usize> {
        let out = Command::new("pgrep")
            .args(["-f", "-x", "-c", command_line])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    #[cfg(unix)]
    #[test]
    fn stopping_a_run_stops_everything_it_started() {
        // A sleep length nothing else on a machine would use, so the count is only ours.
        let marker = "sleep 4242";
        if processes_running(marker).is_none() {
            eprintln!("skipped: pgrep is not available");
            return;
        }
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let limits = Limits {
            timeout: Duration::from_millis(400),
            ..quick()
        };
        let r = sb.run(
            "sh",
            &["-c", "sleep 4242 & sleep 4242 & sleep 4242"],
            &limits,
        );
        assert_eq!(r.status, Status::TimedOut);
        // The kill is asynchronous: give the system a moment to remove the processes.
        let deadline = Instant::now() + Duration::from_secs(3);
        while processes_running(marker) != Some(0) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            processes_running(marker),
            Some(0),
            "processes were left running"
        );
    }

    #[test]
    fn output_beyond_the_cap_is_dropped_and_reported() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let limits = Limits {
            max_output: 1000,
            ..quick()
        };
        let r = sb.run(
            "sh",
            &["-c", "head -c 200000 /dev/zero | tr '\\0' a"],
            &limits,
        );
        assert!(r.passed(), "{:?}", r.status);
        assert!(r.truncated);
        assert_eq!(r.stdout.len(), 1000);
    }

    #[test]
    fn the_command_sees_the_copy_and_a_small_environment_and_no_input() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let here = fs::canonicalize(sb.path()).unwrap();
        let r = sb.run(
            "sh",
            &[
                "-c",
                "pwd; echo \"[$HOME]\"; echo \"[${MOULI_SANDBOX_TEST_SECRET:-unset}]\"; cat",
            ],
            &quick(),
        );
        assert!(r.passed(), "{r:?}");
        let lines: Vec<&str> = r.stdout.lines().collect();
        assert_eq!(fs::canonicalize(lines[0]).unwrap(), here);
        assert_eq!(lines[1], format!("[{}]", sb.path().display()));
        assert_eq!(lines[2], "[unset]");
    }

    #[test]
    fn a_program_that_does_not_exist_is_reported_not_panicked_on() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let r = sb.run("definitely-not-a-program-xyz", &[], &quick());
        assert!(matches!(r.status, Status::SpawnFailed(_)), "{r:?}");
        assert!(!r.passed());
    }

    #[test]
    fn a_patch_can_be_applied_to_the_copy_through_its_workspace() {
        let src = tree();
        let sb = Sandbox::copy_of(src.path(), &quick()).unwrap();
        let ws = sb.workspace().unwrap();
        assert_eq!(ws.read("top.txt").unwrap().as_deref(), Some("top"));
        assert_eq!(ws.root(), fs::canonicalize(sb.path()).unwrap());
    }
}
