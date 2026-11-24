//! Tests for applying patches to a real directory and rolling them back.

use mouli_code::apply::{ApplyError, ApplyOptions};
use mouli_code::diff;
use mouli_code::make::make_patch;
use mouli_code::workspace::{Workspace, WorkspaceError};
use std::fs;
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!("mouli-ws-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

fn exact() -> ApplyOptions {
    ApplyOptions::default()
}

/// A tree snapshot: every file path with its content.
fn snapshot(root: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                out.push((
                    p.strip_prefix(root).unwrap().to_string_lossy().into_owned(),
                    fs::read_to_string(&p).unwrap_or_default(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn apply_changes_files_and_rollback_restores_them_exactly() {
    let root = tmp("roundtrip");
    write(&root, "src/lib.rs", "fn a() {}\nfn b() {}\n");
    write(&root, "keep.txt", "untouched\n");
    let before = snapshot(&root);
    let ws = Workspace::open(&root).unwrap();

    let p1 = make_patch(
        "src/lib.rs",
        Some("fn a() {}\nfn b() {}\n"),
        Some("fn a() {}\nfn b() -> u8 { 1 }\n"),
        3,
    )
    .unwrap()
    .unwrap();
    let p2 = make_patch("src/new.rs", None, Some("pub fn n() {}\n"), 3)
        .unwrap()
        .unwrap();
    let receipt = ws.apply(&[p1, p2], exact()).unwrap();

    assert_eq!(read(&root, "src/lib.rs"), "fn a() {}\nfn b() -> u8 { 1 }\n");
    assert_eq!(read(&root, "src/new.rs"), "pub fn n() {}\n");
    assert_eq!(read(&root, "keep.txt"), "untouched\n");
    assert_eq!(receipt.changed_paths(), vec!["src/lib.rs", "src/new.rs"]);

    ws.rollback(&receipt).unwrap();
    assert_eq!(
        snapshot(&root),
        before,
        "rollback restores the tree and removes created files"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn deletion_and_its_rollback() {
    let root = tmp("delete");
    write(&root, "old.txt", "bye\n");
    let ws = Workspace::open(&root).unwrap();
    let del = make_patch("old.txt", Some("bye\n"), None, 3)
        .unwrap()
        .unwrap();
    let receipt = ws.apply(&[del], exact()).unwrap();
    assert!(!root.join("old.txt").exists());
    ws.rollback(&receipt).unwrap();
    assert_eq!(read(&root, "old.txt"), "bye\n");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_patch_that_does_not_fit_changes_nothing() {
    let root = tmp("nofit");
    write(&root, "a.txt", "1\n");
    write(&root, "b.txt", "2\n");
    let before = snapshot(&root);
    let ws = Workspace::open(&root).unwrap();
    let good = make_patch("a.txt", Some("1\n"), Some("ONE\n"), 3)
        .unwrap()
        .unwrap();
    let bad = diff::parse("--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-WRONG\n+TWO\n")
        .unwrap()
        .remove(0);
    let err = ws.apply(&[good, bad], exact()).unwrap_err();
    assert!(
        matches!(err, WorkspaceError::Apply(ApplyError::HunkMismatch { .. })),
        "{err}"
    );
    assert_eq!(
        snapshot(&root),
        before,
        "the good patch must not have been written either"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn rollback_refuses_when_a_file_was_edited_afterwards() {
    let root = tmp("changed");
    write(&root, "a.txt", "1\n");
    write(&root, "b.txt", "2\n");
    let ws = Workspace::open(&root).unwrap();
    let pa = make_patch("a.txt", Some("1\n"), Some("ONE\n"), 3)
        .unwrap()
        .unwrap();
    let pb = make_patch("b.txt", Some("2\n"), Some("TWO\n"), 3)
        .unwrap()
        .unwrap();
    let receipt = ws.apply(&[pa, pb], exact()).unwrap();
    // Someone edits b.txt after the patch.
    write(&root, "b.txt", "TWO plus a human edit\n");
    let err = ws.rollback(&receipt).unwrap_err();
    assert!(
        matches!(err, WorkspaceError::ChangedSince(ref p) if p == "b.txt"),
        "{err}"
    );
    // Nothing was rolled back, not even the untouched-since file.
    assert_eq!(read(&root, "a.txt"), "ONE\n");
    assert_eq!(read(&root, "b.txt"), "TWO plus a human edit\n");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn paths_outside_the_workspace_are_refused() {
    let root = tmp("outside");
    let ws = Workspace::open(&root).unwrap();
    for rel in [
        "../escape.txt",
        "/tmp/abs.txt",
        "a/../../escape.txt",
        ".git/config",
    ] {
        let d = format!("--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1 @@\n+x\n");
        let patches = diff::parse(&d).unwrap();
        let err = ws.apply(&patches, exact()).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::Apply(ApplyError::UnsafePath(_))),
            "{rel}: {err}"
        );
    }
    assert!(!root.parent().unwrap().join("escape.txt").exists());
    assert!(snapshot(&root).is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn symbolic_links_cannot_be_used_to_escape() {
    use std::os::unix::fs::symlink;
    let root = tmp("symlink");
    let outside = tmp("symlink-outside");
    write(&outside, "secret.txt", "outside\n");
    // A directory link pointing out of the workspace, and a file link pointing at an outside file.
    symlink(&outside, root.join("link_dir")).unwrap();
    symlink(outside.join("secret.txt"), root.join("link_file")).unwrap();
    let ws = Workspace::open(&root).unwrap();

    let create =
        diff::parse("--- /dev/null\n+++ b/link_dir/new.txt\n@@ -0,0 +1 @@\n+pwned\n").unwrap();
    assert!(matches!(
        ws.apply(&create, exact()).unwrap_err(),
        WorkspaceError::Escape(_)
    ));
    assert!(
        !outside.join("new.txt").exists(),
        "nothing may be written through the directory link"
    );

    let edit =
        diff::parse("--- a/link_file\n+++ b/link_file\n@@ -1 +1 @@\n-outside\n+pwned\n").unwrap();
    assert!(matches!(
        ws.apply(&edit, exact()).unwrap_err(),
        WorkspaceError::Escape(_)
    ));
    assert_eq!(read(&outside, "secret.txt"), "outside\n");

    assert!(matches!(
        ws.read("link_file").unwrap_err(),
        WorkspaceError::Escape(_)
    ));
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn file_permissions_survive_a_patch_and_a_rollback() {
    use std::os::unix::fs::PermissionsExt;
    let root = tmp("perm");
    write(&root, "run.sh", "echo a\n");
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let ws = Workspace::open(&root).unwrap();
    let p = make_patch("run.sh", Some("echo a\n"), Some("echo b\n"), 3)
        .unwrap()
        .unwrap();
    let receipt = ws.apply(&[p], exact()).unwrap();
    let mode = |f: &str| fs::metadata(root.join(f)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode("run.sh"), 0o755);
    ws.rollback(&receipt).unwrap();
    assert_eq!(mode("run.sh"), 0o755);
    assert_eq!(read(&root, "run.sh"), "echo a\n");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn binary_files_are_refused_not_mangled() {
    let root = tmp("binary");
    fs::write(root.join("blob.bin"), [0xFF, 0xFE, 0x00, 0x01]).unwrap();
    let ws = Workspace::open(&root).unwrap();
    assert!(matches!(
        ws.read("blob.bin").unwrap_err(),
        WorkspaceError::NotText(_)
    ));
    let p = diff::parse("--- a/blob.bin\n+++ b/blob.bin\n@@ -1 +1 @@\n-x\n+y\n").unwrap();
    assert!(matches!(
        ws.apply(&p, exact()).unwrap_err(),
        WorkspaceError::NotText(_)
    ));
    assert_eq!(
        fs::read(root.join("blob.bin")).unwrap(),
        vec![0xFF, 0xFE, 0x00, 0x01]
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_write_that_fails_part_way_restores_what_was_already_written() {
    let root = tmp("midway");
    write(&root, "a.txt", "1\n");
    // `blocker` is a regular FILE, so creating `blocker/child.txt` must fail.
    write(&root, "blocker", "i am a file\n");
    let before = snapshot(&root);
    let ws = Workspace::open(&root).unwrap();
    let first = make_patch("a.txt", Some("1\n"), Some("ONE\n"), 3)
        .unwrap()
        .unwrap();
    let second = make_patch("blocker/child.txt", None, Some("x\n"), 3)
        .unwrap()
        .unwrap();
    let err = ws.apply(&[first, second], exact()).unwrap_err();
    assert!(matches!(err, WorkspaceError::Io { .. }), "{err}");
    assert_eq!(
        snapshot(&root),
        before,
        "the first file must have been put back"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reading_and_opening_edge_cases() {
    let root = tmp("read");
    write(&root, "dir/a.txt", "a\n");
    let ws = Workspace::open(&root).unwrap();
    assert_eq!(ws.read("dir/a.txt").unwrap().as_deref(), Some("a\n"));
    assert_eq!(ws.read("missing.txt").unwrap(), None);
    let many = ws.read_many(["dir/a.txt", "missing.txt"]).unwrap();
    assert_eq!(many.len(), 1);
    assert!(Workspace::open(&root.join("does-not-exist")).is_err());
    write(&root, "file.txt", "x\n");
    assert!(
        Workspace::open(&root.join("file.txt")).is_err(),
        "a file is not a workspace"
    );
    assert!(ws.root().is_absolute());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn no_temporary_files_are_left_behind() {
    let root = tmp("tmpfiles");
    write(&root, "a.txt", "1\n");
    let ws = Workspace::open(&root).unwrap();
    let p = make_patch("a.txt", Some("1\n"), Some("2\n"), 3)
        .unwrap()
        .unwrap();
    let receipt = ws.apply(&[p], exact()).unwrap();
    ws.rollback(&receipt).unwrap();
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.txt"]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn concurrent_applies_in_one_directory_do_not_collide() {
    let root = tmp("threads");
    for i in 0..8 {
        write(&root, &format!("f{i}.txt"), "old\n");
    }
    let ws = Workspace::open(&root).unwrap();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let ws = ws.clone();
            std::thread::spawn(move || {
                for round in 0..20 {
                    let (from, to) = if round % 2 == 0 {
                        ("old\n", "new\n")
                    } else {
                        ("new\n", "old\n")
                    };
                    let p = make_patch(&format!("f{i}.txt"), Some(from), Some(to), 3)
                        .unwrap()
                        .unwrap();
                    let r = ws.apply(&[p], ApplyOptions::default()).unwrap();
                    assert_eq!(ws.read(&format!("f{i}.txt")).unwrap().as_deref(), Some(to));
                    drop(r);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    for i in 0..8 {
        assert_eq!(read(&root, &format!("f{i}.txt")), "old\n");
    }
    let leftovers = fs::read_dir(&root)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mouli-tmp")
        })
        .count();
    assert_eq!(leftovers, 0);
    let _ = fs::remove_dir_all(&root);
}
