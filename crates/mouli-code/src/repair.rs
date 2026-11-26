//! The repair loop: ask a proposer for a patch, check it against the policy, apply it to a
//! throwaway copy of the folder, run the tests there, and feed what failed back, all inside hard
//! budgets. A patch that makes the tests pass is then shrunk to the hunks the tests need.
//!
//! The real folder is never touched: every try (and every step of the minimiser) runs in a fresh
//! copy made by [`crate::sandbox`]. Applying the result to the real folder is a separate,
//! deliberate step (see [`crate::workspace`]).
//!
//! What this does not show: passing the given tests shows the patch satisfies those tests, not that
//! it is correct, and the policy checks are heuristics. The proposer (usually a model) is outside
//! this crate; this module only enforces what it is allowed to do.

use crate::apply::ApplyOptions;
use crate::budget::{Budget, BudgetExceeded};
use crate::diff::FilePatch;
use crate::minimize::{minimize, Minimized};
use crate::quality::{assess, brackets_balanced, PatchPolicy};
use crate::sandbox::{Limits as RunLimits, Run, Sandbox, SandboxError, Status};
use crate::workspace::Workspace;
use std::path::Path;
use std::time::Duration;

/// The command that runs the tests, for example `cargo` with `test`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCommand {
    /// The program to run.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
}

/// What the proposer is told before each try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feedback {
    /// Which try this is, from 1.
    pub attempt: u32,
    /// The end of the output of the failing test run.
    pub tests: String,
    /// Why the previous proposal was rejected before the tests ran (it was empty, broke the
    /// policy, or did not apply), if it was.
    pub problem: Option<String>,
}

/// Whatever proposes patches (usually a model).
pub trait Proposer {
    /// Proposes a patch set for the failure described by `feedback`.
    ///
    /// # Errors
    /// A message when no proposal can be made; the loop then stops.
    fn propose(&mut self, feedback: &Feedback) -> Result<Vec<FilePatch>, String>;
}

/// Why the loop stopped without a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GaveUp {
    /// A budget ran out.
    Budget(BudgetExceeded),
    /// The proposer could not propose.
    Proposer(String),
    /// The tests could not be run at all (the program is missing, or it hit the time limit on
    /// the unmodified folder).
    TestsDidNotRun(String),
}

/// How a repair ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The tests already passed; there was nothing to fix.
    AlreadyPassing,
    /// A patch made the tests pass.
    Fixed {
        /// The patch as proposed.
        proposed: Vec<FilePatch>,
        /// The proposed patch reduced to the hunks the tests need.
        minimised: Minimized,
        /// How many proposals were tried, the successful one included.
        attempts: u32,
    },
    /// No fix was found.
    GaveUp {
        /// Why it stopped.
        why: GaveUp,
        /// How many proposals were tried.
        attempts: u32,
        /// The end of the last failing test output.
        last_tests: String,
    },
}

/// Settings for [`repair`] besides the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Limits on the sandbox and on each test run (the time limit is also capped by the budget's
    /// `max_test_runtime_secs`).
    pub run: RunLimits,
    /// Rules a patch must satisfy before the tests are run (`allow_test_modification` comes from
    /// the budget's limits).
    pub policy: PatchPolicy,
    /// Most times the tests are re-run while shrinking a fix.
    pub minimise_calls: usize,
    /// How many characters of test output the proposer is shown.
    pub feedback_chars: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            run: RunLimits::default(),
            policy: PatchPolicy::default(),
            minimise_calls: 24,
            feedback_chars: 4000,
        }
    }
}

/// The last `max_chars` characters of a test run's output, on a character boundary.
fn tail(run: &Run, max_chars: usize) -> String {
    let mut text = String::new();
    text.push_str(&run.stdout);
    if !run.stdout.is_empty() && !run.stderr.is_empty() {
        text.push('\n');
    }
    text.push_str(&run.stderr);
    if let Status::SpawnFailed(why) = &run.status {
        text.push_str(why);
    }
    if run.status == Status::TimedOut {
        text.push_str("\n[the tests were stopped at the time limit]");
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text;
    }
    let skip = count - max_chars;
    let start = text.char_indices().nth(skip).map_or(0, |(i, _)| i);
    format!("[earlier output left out]\n{}", &text[start..])
}

/// Applies `patches` to a fresh copy of `source` and runs the tests there. `None` when the patch
/// does not apply (the reason is lost on purpose: callers that need it apply separately).
fn try_in_copy(
    source: &Path,
    patches: &[FilePatch],
    test: &TestCommand,
    run: &RunLimits,
) -> Result<Option<Run>, SandboxError> {
    let sandbox = Sandbox::copy_of(source, run)?;
    if !patches.is_empty() {
        let ws = sandbox.workspace().map_err(|e| SandboxError::Io {
            action: "open the copy".into(),
            source: std::io::Error::other(e.to_string()),
        })?;
        if ws.apply(patches, ApplyOptions::default()).is_err() {
            return Ok(None);
        }
    }
    let args: Vec<&str> = test.args.iter().map(String::as_str).collect();
    Ok(Some(sandbox.run(&test.program, &args, run)))
}

/// Runs the repair loop on the folder `source`.
///
/// # Errors
/// [`SandboxError`] when a copy of the folder cannot be made (for example it is over the size
/// limits).
pub fn repair<P: Proposer>(
    source: &Path,
    test: &TestCommand,
    proposer: &mut P,
    budget: &mut Budget,
    opts: &Options,
) -> Result<Outcome, SandboxError> {
    let mut run_limits = opts.run;
    run_limits.timeout = run_limits.timeout.min(Duration::from_secs(u64::from(
        budget.limits().max_test_runtime_secs,
    )));
    let mut policy = opts.policy;
    policy.allow_test_modification = budget.limits().allow_test_modification;

    // Does it fail now? A folder whose tests cannot run cannot be repaired.
    let baseline =
        try_in_copy(source, &[], test, &run_limits)?.ok_or_else(|| SandboxError::Io {
            action: "run the tests".into(),
            source: std::io::Error::other("an empty patch did not apply"),
        })?;
    match &baseline.status {
        Status::Exited(0) => return Ok(Outcome::AlreadyPassing),
        Status::SpawnFailed(_) | Status::TimedOut => {
            return Ok(Outcome::GaveUp {
                why: GaveUp::TestsDidNotRun(tail(&baseline, 500)),
                attempts: 0,
                last_tests: tail(&baseline, opts.feedback_chars),
            })
        }
        _ => {}
    }
    let mut last_tests = tail(&baseline, opts.feedback_chars);
    let mut problem: Option<String> = None;
    let mut attempts = 0u32;
    let original = Workspace::open(source).map_err(|e| SandboxError::Io {
        action: "open the folder".into(),
        source: std::io::Error::other(e.to_string()),
    })?;

    loop {
        let stop = |why: GaveUp, attempts: u32, last_tests: &str| Outcome::GaveUp {
            why,
            attempts,
            last_tests: last_tests.to_string(),
        };
        // Every try costs one iteration, one model call and one repair attempt, charged when it
        // starts, so a limit of N allows exactly N tries.
        if let Err(e) = budget
            .charge_iteration()
            .and_then(|()| budget.charge_model_call())
            .and_then(|()| budget.charge_repair_attempt())
        {
            return Ok(stop(GaveUp::Budget(e), attempts, &last_tests));
        }
        attempts += 1;
        let feedback = Feedback {
            attempt: attempts,
            tests: last_tests.clone(),
            problem: problem.take(),
        };
        let patches = match proposer.propose(&feedback) {
            Ok(p) => p,
            Err(m) => return Ok(stop(GaveUp::Proposer(m), attempts - 1, &last_tests)),
        };

        // Checks before anything is run.
        let rejected = if patches.is_empty() {
            Some("the proposal changed nothing".to_string())
        } else {
            let files = original
                .read_many(patches.iter().map(FilePatch::path))
                .map_err(|e| SandboxError::Io {
                    action: "read the files".into(),
                    source: std::io::Error::other(e.to_string()),
                })?;
            match assess(&files, &patches, &policy, &brackets_balanced) {
                Err(e) => Some(format!("the patch does not apply: {e}")),
                Ok(report) if !report.acceptable => Some(format!(
                    "the patch breaks the rules: {}",
                    report
                        .findings
                        .iter()
                        .filter(|f| f.kind.is_blocking())
                        .map(|f| f.detail.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
                Ok(_) => None,
            }
        };
        if let Some(why) = rejected {
            problem = Some(why);
            continue;
        }

        match try_in_copy(source, &patches, test, &run_limits)? {
            None => {
                problem = Some("the patch does not apply to the folder".to_string());
            }
            Some(run) if run.passed() => {
                let minimised = minimize(
                    &patches,
                    opts.minimise_calls,
                    |subset| matches!(try_in_copy(source, subset, test, &run_limits), Ok(Some(r)) if r.passed()),
                );
                return Ok(Outcome::Fixed {
                    proposed: patches,
                    minimised,
                    attempts,
                });
            }
            Some(run) => last_tests = tail(&run, opts.feedback_chars),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{Limits, Resource};
    use crate::make::make_patch;
    use std::fs;

    /// A folder whose "tests" pass only when `value.txt` says `good` and `config.txt` says `on`.
    fn project(value: &str, config: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("value.txt"), value).unwrap();
        fs::write(d.path().join("config.txt"), config).unwrap();
        fs::write(
            d.path().join("notes.txt"),
            "line one\nline two\nline three\n",
        )
        .unwrap();
        fs::create_dir(d.path().join("tests")).unwrap();
        fs::write(d.path().join("tests/check.txt"), "expect good and on\n").unwrap();
        d
    }

    fn command() -> TestCommand {
        TestCommand {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "test \"$(cat value.txt)\" = good && test \"$(cat config.txt)\" = on || { echo \"FAIL: value=$(cat value.txt) config=$(cat config.txt)\"; exit 1; }".into(),
            ],
        }
    }

    fn change(path: &str, old: &str, new: &str) -> FilePatch {
        make_patch(path, Some(old), Some(new), 3).unwrap().unwrap()
    }

    /// Hands out scripted proposals and records what it was told.
    struct Script {
        proposals: Vec<Result<Vec<FilePatch>, String>>,
        told: Vec<Feedback>,
    }

    impl Script {
        fn new(proposals: Vec<Result<Vec<FilePatch>, String>>) -> Self {
            Self {
                proposals,
                told: Vec::new(),
            }
        }
    }

    impl Proposer for Script {
        fn propose(&mut self, f: &Feedback) -> Result<Vec<FilePatch>, String> {
            self.told.push(f.clone());
            if self.proposals.is_empty() {
                return Err("no more proposals".into());
            }
            self.proposals.remove(0)
        }
    }

    fn quick() -> Options {
        Options {
            run: RunLimits {
                timeout: Duration::from_secs(10),
                ..RunLimits::default()
            },
            ..Options::default()
        }
    }

    fn files_unchanged(d: &Path, value: &str, config: &str) -> bool {
        fs::read_to_string(d.join("value.txt")).unwrap() == value
            && fs::read_to_string(d.join("config.txt")).unwrap() == config
            && fs::read_to_string(d.join("notes.txt")).unwrap()
                == "line one\nline two\nline three\n"
    }

    #[test]
    fn tests_that_already_pass_need_no_fix_and_no_model_call() {
        let d = project("good", "on");
        let mut p = Script::new(vec![]);
        let mut b = Budget::new(Limits::default());
        let out = repair(d.path(), &command(), &mut p, &mut b, &quick()).unwrap();
        assert_eq!(out, Outcome::AlreadyPassing);
        assert!(p.told.is_empty());
    }

    #[test]
    fn a_fix_is_found_shrunk_to_what_the_tests_need_and_the_real_folder_is_untouched() {
        let d = project("bad", "on");
        // The proposal fixes value.txt but also rewrites notes.txt, which the tests do not need.
        let fix = vec![
            change("value.txt", "bad", "good"),
            change(
                "notes.txt",
                "line one\nline two\nline three\n",
                "LINE ONE\nline two\nline three\n",
            ),
        ];
        let mut p = Script::new(vec![Ok(fix)]);
        let mut b = Budget::new(Limits::default());
        let out = repair(d.path(), &command(), &mut p, &mut b, &quick()).unwrap();
        let Outcome::Fixed {
            proposed,
            minimised,
            attempts,
        } = out
        else {
            panic!("{out:?}")
        };
        assert_eq!(attempts, 1);
        assert_eq!(proposed.len(), 2);
        assert_eq!(minimised.patches.len(), 1, "{minimised:?}");
        assert_eq!(minimised.patches[0].path(), "value.txt");
        assert!(minimised.hunks_removed >= 1);
        assert!(
            files_unchanged(d.path(), "bad", "on"),
            "the real folder must not change"
        );
        // The proposer was told the tests fail and how.
        assert!(
            p.told[0].tests.contains("FAIL: value=bad config=on"),
            "{:?}",
            p.told[0]
        );
        assert_eq!(p.told[0].attempt, 1);
        assert!(p.told[0].problem.is_none());
    }

    #[test]
    fn a_failed_try_is_fed_back_and_the_next_one_can_succeed() {
        let d = project("bad", "off");
        // First try fixes only half of it; the second fixes the rest (and cumulates nothing:
        // each try starts from the original folder, so the second must contain both changes).
        let first = vec![change("value.txt", "bad", "good")];
        let second = vec![
            change("value.txt", "bad", "good"),
            change("config.txt", "off", "on"),
        ];
        let mut p = Script::new(vec![Ok(first), Ok(second)]);
        let mut b = Budget::new(Limits::default());
        let out = repair(d.path(), &command(), &mut p, &mut b, &quick()).unwrap();
        let Outcome::Fixed {
            attempts,
            minimised,
            ..
        } = out
        else {
            panic!("{out:?}")
        };
        assert_eq!(attempts, 2);
        assert_eq!(minimised.patches.len(), 2, "both changes are needed");
        // After the first try the proposer was shown that try's failure: it had fixed value.txt
        // but config.txt was still off.
        assert!(
            p.told[1].tests.contains("value=good config=off"),
            "{:?}",
            p.told[1]
        );
        assert_eq!(p.told[1].attempt, 2);
        assert!(files_unchanged(d.path(), "bad", "off"));
    }

    #[test]
    fn a_patch_that_does_not_apply_or_breaks_the_rules_is_reported_and_costs_an_attempt() {
        let d = project("bad", "on");
        // Wrong context: the file does not say "worse".
        let does_not_apply = vec![change("value.txt", "worse", "good")];
        // Touches a test file, which the default budget forbids.
        let touches_tests = vec![change(
            "tests/check.txt",
            "expect good and on\n",
            "expect nothing\n",
        )];
        let right = vec![change("value.txt", "bad", "good")];
        let mut p = Script::new(vec![
            Ok(does_not_apply),
            Ok(touches_tests),
            Ok(vec![]),
            Ok(right),
        ]);
        let mut b = Budget::new(Limits::default());
        let out = repair(d.path(), &command(), &mut p, &mut b, &quick()).unwrap();
        let Outcome::Fixed { attempts, .. } = out else {
            panic!("{out:?}")
        };
        assert_eq!(attempts, 4);
        assert!(
            p.told[1]
                .problem
                .as_deref()
                .unwrap()
                .contains("does not apply"),
            "{:?}",
            p.told[1]
        );
        let second = p.told[2].problem.as_deref().unwrap();
        assert!(second.contains("breaks the rules"), "{second}");
        assert!(p.told[3]
            .problem
            .as_deref()
            .unwrap()
            .contains("changed nothing"));
    }

    #[test]
    fn the_budget_stops_the_loop_and_says_which_limit_was_hit() {
        let d = project("bad", "on");
        let wrong = || Ok(vec![change("value.txt", "bad", "still bad")]);
        let mut p = Script::new(vec![wrong(), wrong(), wrong(), wrong(), wrong()]);
        let limits = Limits {
            max_repair_attempts: 2,
            ..Limits::default()
        };
        let mut b = Budget::new(limits);
        let out = repair(d.path(), &command(), &mut p, &mut b, &quick()).unwrap();
        let Outcome::GaveUp {
            why,
            attempts,
            last_tests,
        } = out
        else {
            panic!("{out:?}")
        };
        assert_eq!(
            why,
            GaveUp::Budget(BudgetExceeded {
                resource: Resource::RepairAttempts,
                limit: 2
            })
        );
        assert_eq!(attempts, 2);
        assert!(
            last_tests.contains("value=still bad"),
            "the last failure is kept: {last_tests}"
        );
        // The model-call limit is separate.
        let mut p = Script::new(vec![wrong(), wrong(), wrong()]);
        let mut b = Budget::new(Limits {
            max_model_calls: 1,
            ..Limits::default()
        });
        let Outcome::GaveUp { why, .. } =
            repair(d.path(), &command(), &mut p, &mut b, &quick()).unwrap()
        else {
            panic!()
        };
        assert!(matches!(why, GaveUp::Budget(e) if e.resource == Resource::ModelCalls));
        assert_eq!(p.told.len(), 1, "no second call was made");
        assert!(files_unchanged(d.path(), "bad", "on"));
    }

    #[test]
    fn a_proposer_that_cannot_help_ends_the_loop() {
        let d = project("bad", "on");
        let mut p = Script::new(vec![Err("the model is offline".into())]);
        let mut b = Budget::new(Limits::default());
        let out = repair(d.path(), &command(), &mut p, &mut b, &quick()).unwrap();
        let Outcome::GaveUp { why, attempts, .. } = out else {
            panic!("{out:?}")
        };
        assert_eq!(why, GaveUp::Proposer("the model is offline".into()));
        assert_eq!(attempts, 0, "nothing was tried");
    }

    #[test]
    fn tests_that_cannot_run_are_reported_instead_of_repaired() {
        let d = project("bad", "on");
        let missing = TestCommand {
            program: "no-such-test-runner-xyz".into(),
            args: vec![],
        };
        let mut p = Script::new(vec![]);
        let mut b = Budget::new(Limits::default());
        let out = repair(d.path(), &missing, &mut p, &mut b, &quick()).unwrap();
        assert!(
            matches!(
                out,
                Outcome::GaveUp {
                    why: GaveUp::TestsDidNotRun(_),
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(p.told.is_empty());
        // Tests that hang hit the time limit, which the budget caps too.
        let hang = TestCommand {
            program: "sh".into(),
            args: vec!["-c".into(), "sleep 20".into()],
        };
        let mut b = Budget::new(Limits {
            max_test_runtime_secs: 1,
            ..Limits::default()
        });
        let started = std::time::Instant::now();
        let out = repair(d.path(), &hang, &mut p, &mut b, &quick()).unwrap();
        assert!(
            matches!(
                out,
                Outcome::GaveUp {
                    why: GaveUp::TestsDidNotRun(_),
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn the_test_output_shown_to_the_proposer_is_cut_to_its_end_on_a_character_boundary() {
        let run = Run {
            status: Status::Exited(1),
            stdout: "é".repeat(100),
            stderr: "the end".into(),
            truncated: false,
            elapsed: Duration::ZERO,
        };
        let t = tail(&run, 20);
        assert!(t.starts_with("[earlier output left out]"));
        assert!(t.ends_with("the end"));
        assert_eq!(tail(&run, 10_000).chars().count(), 100 + 1 + 7);
    }
}
