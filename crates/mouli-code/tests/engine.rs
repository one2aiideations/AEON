//! Tests for the diff engine, quality policy, minimiser and budget.

use mouli_code::apply::{apply_file, apply_patchset, safe_relative_path, ApplyError, ApplyOptions};
use mouli_code::budget::{Budget, Limits, Resource};
use mouli_code::diff::{self, DiffErrorKind, LineKind};
use mouli_code::make::{make_patch, MakeError};
use mouli_code::minimize::minimize;
use mouli_code::quality::{self, brackets_balanced, is_test_path, FindingKind, PatchPolicy};
use std::collections::BTreeMap;

fn exact() -> ApplyOptions {
    ApplyOptions::default()
}

fn files(items: &[(&str, &str)]) -> BTreeMap<String, String> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

const SIMPLE: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,4 +1,4 @@\n fn a() {}\n-fn b() {}\n+fn b() -> u8 { 1 }\n fn c() {}\n fn d() {}\n";
const SIMPLE_OLD: &str = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
const SIMPLE_NEW: &str = "fn a() {}\nfn b() -> u8 { 1 }\nfn c() {}\nfn d() {}\n";

// ---------- parsing ----------

#[test]
fn parses_a_simple_diff() {
    let p = diff::parse(SIMPLE).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].old_path.as_deref(), Some("src/lib.rs"));
    assert_eq!(p[0].new_path.as_deref(), Some("src/lib.rs"));
    assert_eq!(p[0].hunks.len(), 1);
    let h = &p[0].hunks[0];
    assert_eq!(
        (h.old_start, h.old_len, h.new_start, h.new_len),
        (1, 4, 1, 4)
    );
    assert_eq!(
        h.lines.iter().filter(|l| l.kind == LineKind::Add).count(),
        1
    );
    assert_eq!(p[0].counts(), (1, 1));
}

#[test]
fn ranges_without_a_comma_mean_one_line() {
    let d = "--- a/f\n+++ b/f\n@@ -3 +3 @@\n-old\n+new\n";
    let p = diff::parse(d).unwrap();
    let h = &p[0].hunks[0];
    assert_eq!(
        (h.old_start, h.old_len, h.new_start, h.new_len),
        (3, 1, 3, 1)
    );
}

#[test]
fn git_headers_prose_and_trailing_text_are_tolerated() {
    let d = "Here is the fix:\n\ndiff --git a/x.txt b/x.txt\nindex 83db48f..bf2a7e1 100644\n--- a/x.txt\n+++ b/x.txt\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n\nThis should resolve the issue. Let me know!\n";
    let p = diff::parse(d).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(
        apply_file(Some("keep\nold\n"), &p[0], exact())
            .unwrap()
            .as_deref(),
        Some("keep\nnew\n")
    );
}

#[test]
fn a_stripped_empty_context_line_is_accepted() {
    let d = "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n\n-c\n+d\n";
    let p = diff::parse(d).unwrap();
    assert_eq!(
        apply_file(Some("a\n\nc\n"), &p[0], exact())
            .unwrap()
            .as_deref(),
        Some("a\n\nd\n")
    );
}

#[test]
fn multiple_files_and_hunks_parse() {
    let d = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-1\n+one\n@@ -5 +5 @@\n-5\n+five\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-x\n+y\n";
    let p = diff::parse(d).unwrap();
    assert_eq!((p.len(), p[0].hunks.len(), p[1].hunks.len()), (2, 2, 1));
}

#[test]
fn creation_and_deletion_headers() {
    let d = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n--- a/old.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-x\n-y\n";
    let p = diff::parse(d).unwrap();
    assert!(p[0].is_creation() && !p[0].is_deletion());
    assert!(p[1].is_deletion() && !p[1].is_creation());
    assert_eq!(p[0].path(), "new.txt");
    assert_eq!(p[1].path(), "old.txt");
}

#[test]
fn malformed_diffs_are_refused_with_a_reason() {
    let kind = |d: &str| diff::parse(d).unwrap_err().kind;
    assert_eq!(kind(""), DiffErrorKind::Empty);
    assert_eq!(
        kind("just some prose\nno diff here\n"),
        DiffErrorKind::Empty
    );
    assert_eq!(
        kind("--- a/f\n@@ -1 +1 @@\n-a\n+b\n"),
        DiffErrorKind::MissingNewHeader
    );
    assert_eq!(
        kind("@@ -1 +1 @@\n-a\n+b\n"),
        DiffErrorKind::HunkWithoutHeader
    );
    assert_eq!(
        kind("--- a/f\n+++ b/f\n@@ nonsense @@\n"),
        DiffErrorKind::BadHunkHeader
    );
    assert_eq!(
        kind("--- a/f\n+++ b/f\n@@ -1 +1 @@\n*weird\n"),
        DiffErrorKind::BadHunkLine
    );
    assert_eq!(kind("--- a/f\n+++ b/f\n"), DiffErrorKind::NoHunks);
    assert!(matches!(
        kind("--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n-b\n+c\n"),
        DiffErrorKind::HunkCountMismatch {
            old_expected: 3,
            old_found: 2,
            ..
        }
    ));
    assert_eq!(
        kind("--- a/old\n+++ b/new\n@@ -1 +1 @@\n-a\n+b\n"),
        DiffErrorKind::RenameUnsupported
    );
    assert_eq!(
        kind("diff --git a/x b/y\nrename from x\nrename to y\n"),
        DiffErrorKind::RenameUnsupported
    );
    let huge = "x".repeat(diff::MAX_DIFF_BYTES + 1);
    assert_eq!(kind(&huge), DiffErrorKind::TooLarge);
}

#[test]
fn render_and_parse_round_trip() {
    let p = diff::parse(SIMPLE).unwrap();
    assert_eq!(diff::parse(&diff::render(&p)).unwrap(), p);
}

// ---------- applying ----------

#[test]
fn applies_exactly() {
    let p = diff::parse(SIMPLE).unwrap();
    assert_eq!(
        apply_file(Some(SIMPLE_OLD), &p[0], exact())
            .unwrap()
            .as_deref(),
        Some(SIMPLE_NEW)
    );
}

#[test]
fn a_changed_context_line_is_a_mismatch_not_a_guess() {
    let p = diff::parse(SIMPLE).unwrap();
    let drifted = "fn a() {}\nfn B() {}\nfn c() {}\nfn d() {}\n";
    match apply_file(Some(drifted), &p[0], exact()) {
        Err(ApplyError::HunkMismatch {
            hunk: 1,
            expected_line: 1,
            wanted,
            ..
        }) => assert_eq!(wanted.as_deref(), Some("fn a() {}")),
        other => panic!("expected a mismatch, got {other:?}"),
    }
    let shorter = "fn a() {}\nfn b() {}\n";
    assert!(matches!(
        apply_file(Some(shorter), &p[0], exact()),
        Err(ApplyError::HunkMismatch { .. })
    ));
}

#[test]
fn a_hunk_shifted_by_extra_lines_needs_an_offset_allowance() {
    let p = diff::parse(SIMPLE).unwrap();
    let shifted = format!("// header\n// more\n{SIMPLE_OLD}");
    assert!(apply_file(Some(&shifted), &p[0], exact()).is_err());
    let out = apply_file(Some(&shifted), &p[0], ApplyOptions { max_offset: 2 })
        .unwrap()
        .unwrap();
    assert_eq!(out, format!("// header\n// more\n{SIMPLE_NEW}"));
    assert!(apply_file(Some(&shifted), &p[0], ApplyOptions { max_offset: 1 }).is_err());
}

#[test]
fn an_offset_found_for_one_hunk_carries_to_the_next() {
    let old: String = (1..=20).map(|i| format!("line{i}\n")).collect();
    let new = old
        .replace("line3\n", "THREE\n")
        .replace("line15\n", "FIFTEEN\n");
    let patch = make_patch("f", Some(&old), Some(&new), 1).unwrap().unwrap();
    assert_eq!(patch.hunks.len(), 2);
    let shifted = format!("pre1\npre2\npre3\n{old}");
    let out = apply_file(Some(&shifted), &patch, ApplyOptions { max_offset: 3 })
        .unwrap()
        .unwrap();
    assert_eq!(out, format!("pre1\npre2\npre3\n{new}"));
}

#[test]
fn creation_and_deletion() {
    let d = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n";
    let p = diff::parse(d).unwrap();
    assert_eq!(
        apply_file(None, &p[0], exact()).unwrap().as_deref(),
        Some("a\nb\n")
    );
    assert!(matches!(
        apply_file(Some("exists\n"), &p[0], exact()),
        Err(ApplyError::AlreadyExists(_))
    ));
    let del = diff::parse("--- a/old.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-x\n-y\n").unwrap();
    assert_eq!(apply_file(Some("x\ny\n"), &del[0], exact()).unwrap(), None);
    assert!(matches!(
        apply_file(None, &del[0], exact()),
        Err(ApplyError::MissingFile(_))
    ));
    // A deletion that does not cover the whole file is refused.
    assert!(matches!(
        apply_file(Some("x\ny\nz\n"), &del[0], exact()),
        Err(ApplyError::IncompleteDeletion(_))
    ));
}

#[test]
fn end_of_file_newlines_are_handled_exactly() {
    // Old file has no final newline, new file gains one.
    let d = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n";
    let p = diff::parse(d).unwrap();
    assert_eq!(
        apply_file(Some("a\nb"), &p[0], exact()).unwrap().as_deref(),
        Some("a\nb\n")
    );
    // The same patch must NOT apply to a file that does end with a newline.
    assert!(apply_file(Some("a\nb\n"), &p[0], exact()).is_err());
    // New file loses its final newline.
    let d2 = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n+b\n\\ No newline at end of file\n";
    let p2 = diff::parse(d2).unwrap();
    assert_eq!(
        apply_file(Some("a\nb\n"), &p2[0], exact())
            .unwrap()
            .as_deref(),
        Some("a\nb")
    );
    // A patch that does not mention the missing newline must not apply to such a file.
    assert!(apply_file(Some("a\nb"), &p2[0], exact()).is_err());
}

#[test]
fn reversing_a_patch_restores_the_original() {
    let p = diff::parse(SIMPLE).unwrap();
    let patched = apply_file(Some(SIMPLE_OLD), &p[0], exact())
        .unwrap()
        .unwrap();
    let back = apply_file(Some(&patched), &diff::reverse(&p[0]), exact())
        .unwrap()
        .unwrap();
    assert_eq!(back, SIMPLE_OLD);
    assert_eq!(diff::reverse(&diff::reverse(&p[0])), p[0]);
}

#[test]
fn unsafe_paths_are_refused() {
    for ok in [
        "a.rs",
        "src/lib.rs",
        "deep/er/path/file.txt",
        ".github/workflows/ci.yml",
        "a b/c.txt",
    ] {
        assert_eq!(safe_relative_path(ok).unwrap(), ok);
    }
    for bad in [
        "",
        "/etc/passwd",
        "../outside",
        "a/../../outside",
        "a/./b",
        "a//b",
        "./a",
        ".git/config",
        "x/.git/hooks/pre-commit",
        ".GIT/config",
        "C:/win",
        "a\\b",
        "nul\0byte",
        "line\nbreak",
    ] {
        assert!(
            matches!(safe_relative_path(bad), Err(ApplyError::UnsafePath(_))),
            "{bad:?} must be refused"
        );
    }
    assert!(matches!(
        safe_relative_path(&"a/".repeat(600)),
        Err(ApplyError::UnsafePath(_))
    ));
}

#[test]
fn a_patch_naming_an_unsafe_path_never_applies() {
    let d = "--- a/../../etc/passwd\n+++ b/../../etc/passwd\n@@ -1 +1 @@\n-root\n+evil\n";
    let patches = diff::parse(d).unwrap();
    let fs = files(&[("../../etc/passwd", "root\n")]);
    assert!(matches!(
        apply_patchset(&fs, &patches, exact()),
        Err(ApplyError::UnsafePath(_))
    ));
}

#[test]
fn patchsets_are_all_or_nothing() {
    let d = "--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-1\n+ONE\n--- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-WRONG\n+TWO\n";
    let patches = diff::parse(d).unwrap();
    let fs = files(&[("one.txt", "1\n"), ("two.txt", "2\n")]);
    let before = fs.clone();
    assert!(apply_patchset(&fs, &patches, exact()).is_err());
    assert_eq!(fs, before, "the input snapshot is never modified");

    let good = diff::parse("--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-1\n+ONE\n--- /dev/null\n+++ b/three.txt\n@@ -0,0 +1 @@\n+3\n--- a/two.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-2\n").unwrap();
    let out = apply_patchset(&fs, &good, exact()).unwrap();
    assert_eq!(out, files(&[("one.txt", "ONE\n"), ("three.txt", "3\n")]));
}

#[test]
fn the_same_file_twice_in_one_set_is_refused() {
    let d = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-1\n+2\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-2\n+3\n";
    let patches = diff::parse(d).unwrap();
    assert!(matches!(
        apply_patchset(&files(&[("x", "1\n")]), &patches, exact()),
        Err(ApplyError::DuplicatePath(_))
    ));
}

// ---------- generating ----------

#[test]
fn generated_diffs_have_the_expected_shape() {
    let p = make_patch("f.txt", Some("a\nb\nc\n"), Some("a\nB\nc\n"), 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        diff::render(&[p]),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
    );
    assert_eq!(
        make_patch("f", Some("same\n"), Some("same\n"), 3).unwrap(),
        None
    );
    let created = make_patch("n.txt", None, Some("x\ny\n"), 3)
        .unwrap()
        .unwrap();
    assert!(created.is_creation());
    assert_eq!(
        diff::render(&[created]),
        "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n"
    );
    let deleted = make_patch("n.txt", Some("x\ny\n"), None, 3)
        .unwrap()
        .unwrap();
    assert_eq!(
        diff::render(&[deleted]),
        "--- a/n.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-x\n-y\n"
    );
    assert_eq!(
        make_patch("e", None, Some(""), 3),
        Err(MakeError::EmptyFile)
    );
    assert_eq!(
        make_patch("e", Some(""), None, 3),
        Err(MakeError::EmptyFile)
    );
}

#[test]
fn distant_changes_make_separate_hunks_and_close_ones_merge() {
    let old: String = (1..=30).map(|i| format!("l{i}\n")).collect();
    let far = old.replace("l2\n", "X\n").replace("l28\n", "Y\n");
    assert_eq!(
        make_patch("f", Some(&old), Some(&far), 3)
            .unwrap()
            .unwrap()
            .hunks
            .len(),
        2
    );
    let near = old.replace("l2\n", "X\n").replace("l6\n", "Y\n");
    assert_eq!(
        make_patch("f", Some(&old), Some(&near), 3)
            .unwrap()
            .unwrap()
            .hunks
            .len(),
        1
    );
    assert_eq!(
        make_patch("f", Some(&old), Some(&near), 0)
            .unwrap()
            .unwrap()
            .hunks
            .len(),
        2
    );
}

#[test]
fn huge_changed_regions_are_refused_not_hung() {
    let a: String = (0..3000).map(|i| format!("a{i}\n")).collect();
    let b: String = (0..3000).map(|i| format!("b{i}\n")).collect();
    assert_eq!(
        make_patch("f", Some(&a), Some(&b), 3),
        Err(MakeError::TooLarge)
    );
}

/// A small deterministic pseudo-random generator (no dependencies).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> usize {
        (self.next() % n) as usize
    }
}

fn random_text(rng: &mut Rng, vocab: &[&str]) -> String {
    let n = rng.below(14);
    let mut s: String = (0..n)
        .map(|_| format!("{}\n", vocab[rng.below(vocab.len() as u64)]))
        .collect();
    if rng.below(4) == 0 && s.ends_with('\n') {
        s.pop(); // sometimes no final newline
    }
    s
}

#[test]
fn random_edits_round_trip_through_generate_apply_reverse_and_parse() {
    let vocab = [
        "alpha",
        "beta",
        "gamma",
        "",
        "delta {",
        "}",
        "  indented",
        "x = 1;",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut cases = 0;
    for _ in 0..1500 {
        let old = random_text(&mut rng, &vocab);
        let new = random_text(&mut rng, &vocab);
        for context in [0usize, 1, 3] {
            let patch = match make_patch("f.txt", Some(&old), Some(&new), context).unwrap() {
                None => {
                    assert_eq!(old, new);
                    continue;
                }
                Some(p) => p,
            };
            cases += 1;
            // Rendering and parsing are inverse.
            let reparsed = diff::parse(&diff::render(std::slice::from_ref(&patch))).unwrap();
            assert_eq!(
                reparsed,
                vec![patch.clone()],
                "render/parse for {old:?} -> {new:?}"
            );
            // Applying yields the new text; the reverse patch restores the old one.
            let applied = apply_file(Some(&old), &patch, exact())
                .unwrap()
                .unwrap_or_default();
            assert_eq!(
                applied, new,
                "apply for {old:?} -> {new:?} (context {context})"
            );
            let back = apply_file(Some(&new), &diff::reverse(&patch), exact())
                .unwrap()
                .unwrap_or_default();
            assert_eq!(
                back, old,
                "reverse for {old:?} -> {new:?} (context {context})"
            );
        }
    }
    assert!(cases > 2000, "only {cases} cases ran");
}

// ---------- independent oracle: real git ----------

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "mouli-code-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn git_accepts_our_diffs_and_agrees_with_our_apply() {
    if !git_available() {
        eprintln!("git not available; skipping the oracle test");
        return;
    }
    let vocab = [
        "alpha",
        "beta",
        "gamma",
        "",
        "delta {",
        "}",
        "  indented",
        "x = 1;",
    ];
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    let dir = temp_dir("git-apply");
    let mut checked = 0;
    for _ in 0..120 {
        let old = random_text(&mut rng, &vocab);
        let new = random_text(&mut rng, &vocab);
        if old.is_empty() || new.is_empty() {
            continue; // an empty file is a create/delete case, covered elsewhere
        }
        let Some(patch) = make_patch("f.txt", Some(&old), Some(&new), 3).unwrap() else {
            continue;
        };
        std::fs::write(dir.join("f.txt"), &old).unwrap();
        std::fs::write(dir.join("p.diff"), diff::render(&[patch])).unwrap();
        let out = std::process::Command::new("git")
            .args(["apply", "--no-index", "p.diff"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git rejected our diff for {old:?} -> {new:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            new,
            "git's result differs for {old:?} -> {new:?}"
        );
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(checked > 60, "only {checked} cases reached git");
}

#[test]
fn we_parse_and_apply_diffs_made_by_git() {
    if !git_available() {
        eprintln!("git not available; skipping the oracle test");
        return;
    }
    let vocab = [
        "alpha",
        "beta",
        "gamma",
        "",
        "delta {",
        "}",
        "  indented",
        "x = 1;",
    ];
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let dir = temp_dir("git-diff");
    let mut checked = 0;
    for _ in 0..120 {
        let old = random_text(&mut rng, &vocab);
        let new = random_text(&mut rng, &vocab);
        if old.is_empty() || new.is_empty() || old == new {
            continue;
        }
        std::fs::write(dir.join("old.txt"), &old).unwrap();
        std::fs::write(dir.join("new.txt"), &new).unwrap();
        let out = std::process::Command::new("git")
            .args([
                "diff",
                "--no-index",
                "--no-color",
                "-U3",
                "old.txt",
                "new.txt",
            ])
            .current_dir(&dir)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout)
            .replace("a/old.txt", "a/f.txt")
            .replace("b/new.txt", "b/f.txt");
        let patches = diff::parse(&text).unwrap_or_else(|e| {
            panic!("could not parse git's diff ({e}) for {old:?} -> {new:?}:\n{text}")
        });
        let applied = apply_file(Some(&old), &patches[0], exact()).unwrap_or_else(|e| {
            panic!("could not apply git's diff ({e}) for {old:?} -> {new:?}:\n{text}")
        });
        assert_eq!(
            applied.as_deref(),
            Some(new.as_str()),
            "git's diff applied wrongly for {old:?} -> {new:?}"
        );
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(checked > 60, "only {checked} cases checked");
}

// ---------- quality policy ----------

#[test]
fn test_files_are_recognised_by_path_component_not_substring() {
    for t in [
        "tests/a.py",
        "src/tests/helper.rs",
        "test/x.js",
        "__tests__/y.ts",
        "pkg/test_foo.py",
        "foo_test.go",
        "widget.test.ts",
        "widget.spec.ts",
        "spec/models/user_spec.rb",
    ] {
        assert!(is_test_path(t), "{t}");
    }
    // The old substring rule ("_test" anywhere) wrongly flagged these.
    for n in [
        "src/latest.rs",
        "src/contest.py",
        "src/attestation.rs",
        "src/protest/mod.rs",
        "src/lib.rs",
        "inspector/main.go",
    ] {
        assert!(!is_test_path(n), "{n}");
    }
}

#[test]
fn delimiter_balance_ignores_strings_and_comments() {
    assert!(brackets_balanced(
        "a.rs",
        "fn f() { let s = \"}\"; /* ) */ g(); } // {\n"
    ));
    assert!(brackets_balanced(
        "a.rs",
        "fn f<'a>(x: &'a str) -> char { '(' }"
    ));
    assert!(brackets_balanced(
        "a.py",
        "d = {'a': [1, 2]}  # ) unbalanced in a comment\n"
    ));
    assert!(!brackets_balanced("a.rs", "fn f() { g(; }"));
    assert!(!brackets_balanced("a.rs", "fn f() {"));
    assert!(!brackets_balanced("a.py", "x = (1, 2\n"));
    assert!(brackets_balanced("a.rs", ""));
}

fn repo() -> BTreeMap<String, String> {
    files(&[
        (
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
        ),
        (
            "tests/add.rs",
            "#[test]\nfn t() {\n    assert_eq!(1 + 1, 2);\n}\n",
        ),
    ])
}

fn fix_patch() -> Vec<mouli_code::diff::FilePatch> {
    let r = repo();
    let new = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    vec![make_patch(
        "src/lib.rs",
        r.get("src/lib.rs").map(String::as_str),
        Some(new),
        3,
    )
    .unwrap()
    .unwrap()]
}

#[test]
fn a_small_source_fix_is_acceptable_and_low_risk() {
    let report = quality::assess(
        &repo(),
        &fix_patch(),
        &PatchPolicy::default(),
        &brackets_balanced,
    )
    .unwrap();
    assert!(report.acceptable, "{:?}", report.findings);
    assert!(report.findings.is_empty());
    assert_eq!(
        (report.stats.added, report.stats.removed, report.stats.hunks),
        (1, 1, 1)
    );
    assert!(!report.stats.touches_tests);
    // 2 changed lines at 2.5 points each.
    assert_eq!(report.risk, 5);
}

#[test]
fn editing_tests_is_blocked_unless_the_policy_allows_it() {
    let r = repo();
    let weakened = "#[test]\nfn t() {\n    assert_eq!(1 + 1, 3);\n}\n";
    let p = vec![make_patch(
        "tests/add.rs",
        r.get("tests/add.rs").map(String::as_str),
        Some(weakened),
        3,
    )
    .unwrap()
    .unwrap()];
    let blocked = quality::assess(&r, &p, &PatchPolicy::default(), &brackets_balanced).unwrap();
    assert!(!blocked.acceptable);
    assert!(blocked
        .findings
        .iter()
        .any(|f| f.kind == FindingKind::TestFileModified));
    assert!(blocked.risk >= 60);
    let allowed = quality::assess(
        &r,
        &p,
        &PatchPolicy {
            allow_test_modification: true,
            ..PatchPolicy::default()
        },
        &brackets_balanced,
    )
    .unwrap();
    assert!(allowed.acceptable);
}

#[test]
fn the_syntax_check_runs_on_the_edited_file_not_the_original() {
    let r = repo();
    let broken = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n";
    let p = vec![make_patch(
        "src/lib.rs",
        r.get("src/lib.rs").map(String::as_str),
        Some(broken),
        3,
    )
    .unwrap()
    .unwrap()];
    let report = quality::assess(&r, &p, &PatchPolicy::default(), &brackets_balanced).unwrap();
    assert!(!report.acceptable);
    assert!(report
        .findings
        .iter()
        .any(|f| f.kind == FindingKind::SyntaxInvalid));
    assert_eq!(report.risk, 100);
    // The ORIGINAL is fine, so a check on the original (the old prototype's behaviour) passes.
    assert!(brackets_balanced(
        "src/lib.rs",
        r.get("src/lib.rs").unwrap()
    ));
}

#[test]
fn a_patch_that_does_not_apply_is_an_error_not_a_score() {
    let d = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n totally\n-different\n+file\n content\n";
    let p = diff::parse(d).unwrap();
    assert!(matches!(
        quality::assess(&repo(), &p, &PatchPolicy::default(), &brackets_balanced),
        Err(ApplyError::HunkMismatch { .. })
    ));
}

#[test]
fn hardcoding_heuristics_look_at_added_lines_only() {
    let r = files(&[(
        "src/lib.rs",
        "pub fn price(item: &str) -> u32 {\n    lookup(item)\n}\n",
    )]);
    // The old text contains a date and "expected", the new lines do not: nothing is flagged.
    let clean_old = files(&[("src/lib.rs", "// expected 2020-01-01\npub fn f() {}\n")]);
    let cleaned = make_patch(
        "src/lib.rs",
        clean_old.get("src/lib.rs").map(String::as_str),
        Some("pub fn f() {}\n"),
        3,
    )
    .unwrap()
    .unwrap();
    let rep = quality::assess(
        &clean_old,
        &[cleaned],
        &PatchPolicy::default(),
        &brackets_balanced,
    )
    .unwrap();
    assert!(rep.findings.is_empty(), "{:?}", rep.findings);

    let hacked = "pub fn price(item: &str) -> u32 {\n    if item == \"expected_widget\" { return 42; } // hidden test\n    let d = \"2024-03-05\";\n    let h = \"0123456789abcdef0123456789abcdef\";\n    lookup(item)\n}\n";
    let p = vec![make_patch(
        "src/lib.rs",
        r.get("src/lib.rs").map(String::as_str),
        Some(hacked),
        3,
    )
    .unwrap()
    .unwrap()];
    let rep = quality::assess(&r, &p, &PatchPolicy::default(), &brackets_balanced).unwrap();
    let kinds: Vec<FindingKind> = rep.findings.iter().map(|f| f.kind).collect();
    assert!(kinds.contains(&FindingKind::HardcodedExpected));
    assert!(kinds.contains(&FindingKind::SuspiciousConstant));
    assert!(kinds.contains(&FindingKind::SpecialCasing));
    // Warnings raise the risk but are not blocking by themselves.
    assert!(rep.acceptable);
    assert!(rep.risk >= 40);
}

#[test]
fn size_limits_whitespace_only_changes_and_deletions_are_reported() {
    let r = repo();
    let tiny = PatchPolicy {
        max_changed_lines: 1,
        ..PatchPolicy::default()
    };
    let rep = quality::assess(&r, &fix_patch(), &tiny, &brackets_balanced).unwrap();
    assert!(rep
        .findings
        .iter()
        .any(|f| f.kind == FindingKind::TooManyLines));
    assert!(!rep.acceptable);
    let one_file = PatchPolicy {
        max_files: 0,
        ..PatchPolicy::default()
    };
    assert!(
        quality::assess(&r, &fix_patch(), &one_file, &brackets_balanced)
            .unwrap()
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::TooManyFiles)
    );

    let ws = vec![make_patch(
        "src/lib.rs",
        r.get("src/lib.rs").map(String::as_str),
        Some("pub fn add(a: i32, b: i32) -> i32 {\n        a - b\n}\n"),
        3,
    )
    .unwrap()
    .unwrap()];
    assert!(
        quality::assess(&r, &ws, &PatchPolicy::default(), &brackets_balanced)
            .unwrap()
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::WhitespaceOnly)
    );

    let del = vec![make_patch(
        "src/lib.rs",
        r.get("src/lib.rs").map(String::as_str),
        None,
        3,
    )
    .unwrap()
    .unwrap()];
    assert!(
        quality::assess(&r, &del, &PatchPolicy::default(), &brackets_balanced)
            .unwrap()
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::FileDeleted)
    );
}

// ---------- minimising ----------

fn three_hunk_patch() -> (String, String, Vec<mouli_code::diff::FilePatch>) {
    let old: String = (1..=40).map(|i| format!("l{i}\n")).collect();
    let new = old
        .replace("l3\n", "THREE\n")
        .replace("l20\n", "TWENTY\n")
        .replace("l37\n", "THIRTYSEVEN\n");
    let p = make_patch("f.txt", Some(&old), Some(&new), 2)
        .unwrap()
        .unwrap();
    assert_eq!(p.hunks.len(), 3);
    (old, new, vec![p])
}

#[test]
fn only_the_hunks_the_oracle_needs_survive() {
    let (old, _, patches) = three_hunk_patch();
    // The "tests" pass only if line 20 became TWENTY.
    let result = minimize(&patches, 100, |cand| {
        let fs = files(&[("f.txt", &old)]);
        apply_patchset(&fs, cand, exact())
            .map(|out| out["f.txt"].contains("TWENTY"))
            .unwrap_or(false)
    });
    assert_eq!(result.hunks_removed, 2);
    assert_eq!(result.patches[0].hunks.len(), 1);
    assert!(!result.budget_exhausted);
    let fs = files(&[("f.txt", &old)]);
    let out = apply_patchset(&fs, &result.patches, exact()).unwrap();
    assert!(
        out["f.txt"].contains("TWENTY")
            && !out["f.txt"].contains("THREE")
            && !out["f.txt"].contains("THIRTYSEVEN")
    );
}

#[test]
fn minimising_keeps_everything_when_every_hunk_is_needed() {
    let (_, _, patches) = three_hunk_patch();
    let result = minimize(&patches, 100, |cand| {
        cand.iter().map(|p| p.hunks.len()).sum::<usize>() == 3
    });
    assert_eq!(result.hunks_removed, 0);
    assert_eq!(result.patches, patches);
}

#[test]
fn a_file_whose_hunks_are_all_unneeded_is_dropped() {
    let (_, _, mut patches) = three_hunk_patch();
    let other = make_patch("g.txt", Some("a\nb\n"), Some("a\nB\n"), 1)
        .unwrap()
        .unwrap();
    patches.push(other);
    // Only f.txt matters.
    let result = minimize(&patches, 100, |cand| {
        cand.iter()
            .any(|p| p.path() == "f.txt" && p.hunks.len() == 3)
    });
    assert_eq!(result.patches.len(), 1);
    assert_eq!(result.patches[0].path(), "f.txt");
    assert_eq!(result.hunks_removed, 1);
}

#[test]
fn creations_and_deletions_are_never_split() {
    let create = make_patch("new.txt", None, Some("a\nb\nc\n"), 3)
        .unwrap()
        .unwrap();
    let result = minimize(std::slice::from_ref(&create), 100, |_| true);
    // Even an oracle that accepts anything cannot remove part of (or all of) a creation.
    assert_eq!(result.patches, vec![create]);
    assert_eq!(result.hunks_removed, 0);
}

#[test]
fn the_oracle_call_budget_is_respected() {
    let (_, _, patches) = three_hunk_patch();
    let mut asked = 0;
    let result = minimize(&patches, 2, |_| {
        asked += 1;
        true
    });
    assert_eq!(asked, 2);
    assert_eq!(result.oracle_calls, 2);
    assert!(result.budget_exhausted);
    let none = minimize(&patches, 0, |_| true);
    assert_eq!((none.oracle_calls, none.hunks_removed), (0, 0));
}

#[test]
fn a_rejecting_oracle_removes_nothing() {
    let (_, _, patches) = three_hunk_patch();
    let result = minimize(&patches, 100, |_| false);
    assert_eq!(result.hunks_removed, 0);
    assert_eq!(result.oracle_calls, 3);
}

// ---------- budget ----------

#[test]
fn budgets_stop_exactly_at_the_limit() {
    let mut b = Budget::new(Limits {
        max_iterations: 2,
        max_model_calls: 3,
        max_repair_attempts: 1,
        ..Limits::default()
    });
    assert!(b.charge_iteration().is_ok() && b.charge_iteration().is_ok());
    let e = b.charge_iteration().unwrap_err();
    assert_eq!((e.resource, e.limit), (Resource::Iterations, 2));
    for _ in 0..3 {
        b.charge_model_call().unwrap();
    }
    assert_eq!(b.model_calls_left(), 0);
    assert_eq!(
        b.charge_model_call().unwrap_err().resource,
        Resource::ModelCalls
    );
    assert!(b.charge_repair_attempt().is_ok());
    assert_eq!(
        b.charge_repair_attempt().unwrap_err().resource,
        Resource::RepairAttempts
    );
    // A failed charge does not change the count.
    assert_eq!(b.model_calls_left(), 0);
    assert!(
        !Budget::new(Limits::default())
            .limits()
            .allow_test_modification
    );
    assert!(b
        .charge_iteration()
        .unwrap_err()
        .to_string()
        .contains("exhausted"));
}
