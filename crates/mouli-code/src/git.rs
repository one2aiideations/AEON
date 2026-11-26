//! A read-only view of a git repository: which branch is checked out, how far it is from its
//! upstream, and which files are changed.
//!
//! This runs `git status` and nothing else. It never writes to the repository: it passes
//! `GIT_OPTIONAL_LOCKS=0` (so git does not even refresh the index), turns off `core.fsmonitor`
//! (a repository setting that can name a program to run), uses the user's own `PATH` only to find
//! `git`, and ignores the user's and system git configuration. Output is read from git's
//! machine-readable form, never from text meant for people.
//!
//! What it does not do: it needs the `git` program, and shows what git reports, not whether the
//! changes are good. A folder that is not a repository gives [`GitError::NotARepository`].

use std::path::Path;
use std::process::{Command, Stdio};

/// What went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// The `git` program could not be started.
    NoGit(String),
    /// The folder is not inside a git repository.
    NotARepository,
    /// Git failed for another reason (its message).
    Failed(String),
    /// Git's output was not in the expected form.
    Unreadable(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGit(e) => write!(f, "git could not be started: {e}"),
            Self::NotARepository => f.write_str("this folder is not a git repository"),
            Self::Failed(e) => write!(f, "git failed: {e}"),
            Self::Unreadable(e) => write!(f, "git's output could not be read: {e}"),
        }
    }
}

impl std::error::Error for GitError {}

/// How one file differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Changed content.
    Modified,
    /// A new file.
    Added,
    /// A removed file.
    Deleted,
    /// Moved to another name.
    Renamed,
    /// Copied from another file.
    Copied,
    /// Changed kind (file to link, for example).
    TypeChanged,
    /// Not tracked by git.
    Untracked,
    /// Both sides changed it and git could not merge.
    Conflicted,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path, from the repository root.
    pub path: String,
    /// The old path for a rename or copy.
    pub from: Option<String>,
    /// The change staged for the next commit, if any.
    pub staged: Option<Change>,
    /// The change in the working folder not yet staged, if any.
    pub unstaged: Option<Change>,
}

/// The branch and how it relates to its upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// The branch name; `None` when HEAD is detached.
    pub name: Option<String>,
    /// The commit (abbreviated) when detached or when the branch has commits.
    pub head: Option<String>,
    /// The upstream branch, when one is set.
    pub upstream: Option<String>,
    /// Commits here that the upstream does not have.
    pub ahead: u32,
    /// Commits the upstream has that are not here.
    pub behind: u32,
}

/// What `git status` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The branch.
    pub branch: Branch,
    /// Changed files, in git's order.
    pub entries: Vec<Entry>,
}

impl Status {
    /// Whether nothing is changed or untracked.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether any file is in conflict.
    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.staged == Some(Change::Conflicted))
    }

    /// One line for a status bar, such as `main ↑1 ↓2 · 3 changed`.
    #[must_use]
    pub fn summary(&self) -> String {
        let b = &self.branch;
        let mut s = match (&b.name, &b.head) {
            (Some(n), _) => n.clone(),
            (None, Some(h)) => format!("detached at {h}"),
            (None, None) => "no commits".to_string(),
        };
        if b.ahead > 0 {
            s.push_str(&format!(" ↑{}", b.ahead));
        }
        if b.behind > 0 {
            s.push_str(&format!(" ↓{}", b.behind));
        }
        if self.entries.is_empty() {
            s.push_str(" · clean");
        } else {
            s.push_str(&format!(" · {} changed", self.entries.len()));
        }
        s
    }
}

fn change_of(c: char) -> Option<Change> {
    match c {
        'M' => Some(Change::Modified),
        'A' => Some(Change::Added),
        'D' => Some(Change::Deleted),
        'R' => Some(Change::Renamed),
        'C' => Some(Change::Copied),
        'T' => Some(Change::TypeChanged),
        _ => None,
    }
}

/// Parses `git status --porcelain=v1 -z --branch` output.
///
/// # Errors
/// [`GitError::Unreadable`] when a record is malformed.
pub fn parse(output: &[u8]) -> Result<Status, GitError> {
    let text = String::from_utf8_lossy(output);
    let mut records = text.split('\0').filter(|r| !r.is_empty());
    let header = records
        .next()
        .and_then(|h| h.strip_prefix("## "))
        .ok_or_else(|| GitError::Unreadable("no branch line".into()))?;
    let branch = parse_branch(header);
    let mut entries = Vec::new();
    while let Some(rec) = records.next() {
        let mut chars = rec.chars();
        let (x, y) = (
            chars
                .next()
                .ok_or_else(|| GitError::Unreadable(rec.into()))?,
            chars
                .next()
                .ok_or_else(|| GitError::Unreadable(rec.into()))?,
        );
        let path = rec
            .get(3..)
            .filter(|p| !p.is_empty() && rec.as_bytes().get(2) == Some(&b' '))
            .ok_or_else(|| GitError::Unreadable(rec.into()))?
            .to_string();
        let conflicted = matches!((x, y), ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D'));
        let (staged, unstaged) = if (x, y) == ('?', '?') {
            (None, Some(Change::Untracked))
        } else if (x, y) == ('!', '!') {
            continue;
        } else if conflicted {
            (Some(Change::Conflicted), Some(Change::Conflicted))
        } else {
            (change_of(x), change_of(y))
        };
        // In -z form a rename or copy is followed by its old path as a separate record.
        let from = if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            Some(
                records
                    .next()
                    .ok_or_else(|| GitError::Unreadable(format!("{rec}: no old path")))?
                    .to_string(),
            )
        } else {
            None
        };
        entries.push(Entry {
            path,
            from,
            staged,
            unstaged,
        });
    }
    Ok(Status { branch, entries })
}

fn parse_branch(h: &str) -> Branch {
    let (names, counts) = match h.split_once(" [") {
        Some((n, c)) => (n, c.trim_end_matches(']')),
        None => (h, ""),
    };
    let mut ahead = 0;
    let mut behind = 0;
    for part in counts.split(", ") {
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.parse().unwrap_or(0);
        }
    }
    if let Some(rest) = names.strip_prefix("No commits yet on ") {
        return Branch {
            name: Some(rest.to_string()),
            head: None,
            upstream: None,
            ahead,
            behind,
        };
    }
    if names.starts_with("HEAD (no branch)") {
        return Branch {
            name: None,
            head: None,
            upstream: None,
            ahead,
            behind,
        };
    }
    let (name, upstream) = match names.split_once("...") {
        Some((n, u)) => (n, Some(u.to_string())),
        None => (names, None),
    };
    Branch {
        name: Some(name.to_string()),
        head: None,
        upstream,
        ahead,
        behind,
    }
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=false", "-c", "core.quotepath=false"])
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| GitError::NoGit(e.to_string()))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if err.contains("not a git repository") {
        Err(GitError::NotARepository)
    } else {
        Err(GitError::Failed(err))
    }
}

/// Reads the status of the repository containing `root`.
///
/// # Errors
/// See [`GitError`].
pub fn status(root: &Path) -> Result<Status, GitError> {
    let out = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--branch",
            "--untracked-files=normal",
        ],
    )?;
    let mut st = parse(&out)?;
    // The branch line has no commit id; ask for it separately (empty in a repository with no commits).
    if let Ok(h) = git(root, &["rev-parse", "--short", "HEAD"]) {
        let h = String::from_utf8_lossy(&h).trim().to_string();
        if !h.is_empty() {
            st.branch.head = Some(h);
        }
    }
    if st.branch.name.is_none() && st.branch.head.is_none() {
        st.branch.name = None;
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), &["init", "-q", "-b", "main"]);
        fs::write(d.path().join("a.txt"), "one\n").unwrap();
        fs::write(d.path().join("b.txt"), "two\n").unwrap();
        run(d.path(), &["add", "."]);
        run(d.path(), &["commit", "-q", "-m", "first"]);
        d
    }

    #[test]
    fn a_clean_repository_says_so_with_its_branch() {
        let d = repo();
        let s = status(d.path()).unwrap();
        assert!(s.is_clean());
        assert_eq!(s.branch.name.as_deref(), Some("main"));
        assert!(s.branch.head.as_deref().is_some_and(|h| h.len() >= 4));
        assert_eq!(s.summary(), "main · clean");
    }

    #[test]
    fn staged_unstaged_and_untracked_changes_are_told_apart() {
        let d = repo();
        let p = d.path();
        fs::write(p.join("a.txt"), "changed\n").unwrap(); // unstaged edit
        fs::write(p.join("c.txt"), "new\n").unwrap();
        run(p, &["add", "c.txt"]); // staged addition
        fs::write(p.join("d.txt"), "loose\n").unwrap(); // untracked
        fs::remove_file(p.join("b.txt")).unwrap(); // unstaged deletion
        let s = status(p).unwrap();
        let find = |n: &str| {
            s.entries
                .iter()
                .find(|e| e.path == n)
                .unwrap_or_else(|| panic!("{n}: {s:?}"))
        };
        assert_eq!(
            (find("a.txt").staged, find("a.txt").unstaged),
            (None, Some(Change::Modified))
        );
        assert_eq!(
            (find("c.txt").staged, find("c.txt").unstaged),
            (Some(Change::Added), None)
        );
        assert_eq!(
            (find("d.txt").staged, find("d.txt").unstaged),
            (None, Some(Change::Untracked))
        );
        assert_eq!(
            (find("b.txt").staged, find("b.txt").unstaged),
            (None, Some(Change::Deleted))
        );
        assert_eq!(s.summary(), "main · 4 changed");
    }

    #[test]
    fn a_rename_carries_its_old_name_and_odd_names_survive() {
        let d = repo();
        let p = d.path();
        run(p, &["mv", "a.txt", "älter name.txt"]);
        let s = status(p).unwrap();
        let e = &s.entries[0];
        assert_eq!(e.path, "älter name.txt");
        assert_eq!(e.from.as_deref(), Some("a.txt"));
        assert_eq!(e.staged, Some(Change::Renamed));
    }

    #[test]
    fn detached_head_upstream_counts_and_conflicts_are_reported() {
        let d = repo();
        let p = d.path();
        // Conflict: two branches change the same line.
        run(p, &["checkout", "-q", "-b", "other"]);
        fs::write(p.join("a.txt"), "other side\n").unwrap();
        run(p, &["commit", "-q", "-am", "other"]);
        run(p, &["checkout", "-q", "main"]);
        fs::write(p.join("a.txt"), "main side\n").unwrap();
        run(p, &["commit", "-q", "-am", "main"]);
        let merged = Command::new("git")
            .arg("-C")
            .arg(p)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "merge",
                "other",
            ])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(!merged.status.success());
        let s = status(p).unwrap();
        assert!(s.has_conflicts(), "{s:?}");
        run(p, &["merge", "--abort"]);
        // Detached.
        run(p, &["checkout", "-q", "--detach"]);
        let s = status(p).unwrap();
        assert_eq!(s.branch.name, None);
        assert!(s.summary().starts_with("detached at "), "{}", s.summary());
        // Upstream counts, from a clone that is one behind and one ahead.
        let up = tempfile::tempdir().unwrap();
        run(up.path(), &["clone", "-q", p.to_str().unwrap(), "w"]);
        let w = up.path().join("w");
        run(p, &["checkout", "-q", "main"]);
        fs::write(p.join("n.txt"), "x\n").unwrap();
        run(p, &["add", "n.txt"]);
        run(p, &["commit", "-q", "-m", "origin moves"]);
        fs::write(w.join("m.txt"), "y\n").unwrap();
        run(&w, &["add", "m.txt"]);
        run(&w, &["commit", "-q", "-m", "local"]);
        run(&w, &["fetch", "-q"]);
        let s = status(&w).unwrap();
        assert_eq!((s.branch.ahead, s.branch.behind), (1, 1), "{s:?}");
        assert!(s.branch.upstream.is_some());
        assert_eq!(s.summary(), "main ↑1 ↓1 · clean");
    }

    #[test]
    fn a_repository_with_no_commits_and_a_plain_folder_are_handled() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(status(d.path()), Err(GitError::NotARepository));
        run(d.path(), &["init", "-q", "-b", "trunk"]);
        let s = status(d.path()).unwrap();
        assert_eq!(s.branch.name.as_deref(), Some("trunk"));
        assert_eq!(s.branch.head, None);
    }

    #[test]
    fn looking_at_a_repository_changes_nothing_in_it_and_runs_no_configured_program() {
        let d = repo();
        let p = d.path();
        fs::write(p.join("a.txt"), "dirty\n").unwrap();
        // A repository setting that names a program to run on every status.
        let marker = p.join("RAN");
        let hook = p.join("hook.sh");
        fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        run(p, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
        let _ = Command::new("chmod").arg("+x").arg(&hook).status();
        let before = fs::read(p.join(".git/index")).unwrap();
        let mtime = fs::metadata(p.join(".git/index"))
            .unwrap()
            .modified()
            .unwrap();
        status(p).unwrap();
        assert!(!marker.exists(), "the configured program must not run");
        assert_eq!(fs::read(p.join(".git/index")).unwrap(), before);
        assert_eq!(
            fs::metadata(p.join(".git/index"))
                .unwrap()
                .modified()
                .unwrap(),
            mtime
        );
    }

    #[test]
    fn malformed_output_is_an_error_not_a_panic() {
        assert!(matches!(parse(b""), Err(GitError::Unreadable(_))));
        assert!(matches!(
            parse(b"no header\0"),
            Err(GitError::Unreadable(_))
        ));
        assert!(matches!(
            parse(b"## main\0M\0"),
            Err(GitError::Unreadable(_))
        ));
        assert!(matches!(
            parse(b"## main\0R  new\0"),
            Err(GitError::Unreadable(_))
        ));
        assert!(parse(b"## main\0!! ignored\0").unwrap().is_clean());
    }
}
