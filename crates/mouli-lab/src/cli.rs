//! The `mouli-lab` command line: `selftest`, `run`, `temporal`, `aggregate`.
//!
//! Every command returns text instead of printing, so it can be tested. Real results need real
//! data: `run` refuses to start without MNIST IDX files unless `--synthetic` is given, and a
//! synthetic run is labelled as such in its folder (`DATASET.txt`) and at the top of every report.

use crate::aetheron_ml::{gradcheck_model, AetheronMlCfg, AetheronMlLearner};
use crate::baselines::{gradcheck_mlp, MlpLearner};
use crate::continual::{run_split, tune, RunCfg, Strategy, ARCHES};
use crate::core::{GradCheck, Learner, Mode};
use crate::data::{load_idx, normalise_mnist, synthetic_blobs, Dataset};
use crate::experiments::{run_temporal, temporal_table, TemporalCfg, TemporalModel};
use crate::report::{aggregate, write_dataset_marker, write_run};
use crate::rng::Rng;
use crate::temporal::{make_batch, GatedAetheron, MiniTransformer, RecurrentAetheron};
use crate::F;

/// The text of `mouli-lab --help`.
pub const USAGE: &str = "mouli-lab: research harness for the Aetheron model (not product code)\n\
\n\
  mouli-lab selftest\n\
      check every hand-written gradient against finite differences\n\
  mouli-lab run --out DIR (--mnist TRAIN_IMAGES TRAIN_LABELS TEST_IMAGES TEST_LABELS | --synthetic)\n\
                [--arch a,b] [--strategy s,t] [--seeds N] [--epochs N]\n\
      split-class benchmark; learning rates are tuned once per method on a held-out tenth of\n\
      the training data with the same budget for every method, then each seed is run\n\
  mouli-lab temporal --out DIR [--model recurrent,gated,transformer] [--seeds N] [--steps N]\n\
      delayed-match task, with stability counters\n\
  mouli-lab aggregate DIR\n\
      report from the run files in DIR, with wins and losses\n";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn number<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> Result<T, String> {
    match flag(args, name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| format!("{name} needs a number, got '{v}'")),
    }
}

fn list<'a>(args: &'a [String], name: &str, default: &'a str) -> Vec<&'a str> {
    flag(args, name)
        .unwrap_or(default)
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Runs one command.
///
/// # Errors
/// A message when the command is unknown, an argument is wrong, data cannot be read, or (for
/// `selftest`) a gradient check fails.
pub fn run(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => Ok(USAGE.to_string()),
        Some("selftest") => selftest(),
        Some("run") => run_benchmark(&args[1..]),
        Some("temporal") => run_temporal_cmd(&args[1..]),
        Some("aggregate") => match args.get(1) {
            Some(dir) => aggregate(dir),
            None => Err("aggregate needs a folder".into()),
        },
        Some(other) => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    }
}

fn line(name: &str, checks: &[GradCheck], out: &mut String, failed: &mut usize) {
    let worst = checks.iter().map(|c| c.max_rel).fold(0.0, F::max);
    let ok = !checks.is_empty() && checks.iter().all(|c| c.pass);
    if !ok {
        *failed += 1;
    }
    out.push_str(&format!(
        "{} {name}: {} arrays, worst relative error {worst:.2e}\n",
        if ok { "PASS" } else { "FAIL" },
        checks.len()
    ));
}

fn selftest() -> Result<String, String> {
    let mut out = String::new();
    let mut failed = 0;
    let mut rng = Rng::new(3);
    let blobs = synthetic_blobs(16, 6, 3, 1);
    line(
        "mlp",
        &gradcheck_mlp(
            &MlpLearner::new(6, 5, 3, 2, 0.05),
            &blobs.x,
            &blobs.y,
            12,
            &mut rng,
        ),
        &mut out,
        &mut failed,
    );
    let ds = synthetic_blobs(12, 6, 3, 4);
    let base = AetheronMlCfg {
        in_dim: 6,
        units: 4,
        branches: 2,
        hidden: 3,
        n_classes: 3,
        dk: 3,
        eps: -1.0,
        ..Default::default()
    };
    for (name, cfg) in [
        ("aetheron", base.clone()),
        (
            "aetheron without attention",
            AetheronMlCfg {
                attention: false,
                ..base.clone()
            },
        ),
    ] {
        let mut m = AetheronMlLearner::new(cfg, 5);
        for _ in 0..3 {
            m.step(&ds.x, &ds.y, Mode::Encode);
        }
        line(
            name,
            &gradcheck_model(&m, &ds.x, &ds.y, 10, &mut rng),
            &mut out,
            &mut failed,
        );
    }
    let (xs, y) = make_batch(5, 6, 3, 0.1, &mut rng);
    line(
        "recurrent",
        &RecurrentAetheron::new(5, 3, 2, 3, 8).gradcheck(&xs, &y, 8, &mut rng),
        &mut out,
        &mut failed,
    );
    line(
        "gated",
        &GatedAetheron::new(5, 3, 2, 3, 9).gradcheck(&xs, &y, 8, &mut rng),
        &mut out,
        &mut failed,
    );
    line(
        "transformer",
        &MiniTransformer::new(5, 4, 10).gradcheck(&xs, &y, 8, &mut rng),
        &mut out,
        &mut failed,
    );
    if failed > 0 {
        return Err(format!("{out}{failed} gradient check(s) FAILED"));
    }
    out.push_str("All gradient checks passed.\n");
    Ok(out)
}

/// Splits off the last tenth of the training data for tuning.
fn split_validation(train: &Dataset) -> (Dataset, Dataset) {
    let n = train.y.len();
    let cut = n - (n / 10).max(1);
    let fit: Vec<usize> = (0..cut).collect();
    let val: Vec<usize> = (cut..n).collect();
    (train.subset(&fit), train.subset(&val))
}

fn run_benchmark(args: &[String]) -> Result<String, String> {
    let out_dir = flag(args, "--out").ok_or("run needs --out DIR")?;
    let (train, test, marker) = if let Some(i) = args.iter().position(|a| a == "--mnist") {
        let p: Vec<&String> = args.iter().skip(i + 1).take(4).collect();
        if p.len() < 4 || p.iter().any(|a| a.starts_with("--")) {
            return Err(
                "--mnist needs four files: train images, train labels, test images, test labels"
                    .into(),
            );
        }
        let mut train = load_idx(p[0], p[1])?;
        let mut test = load_idx(p[2], p[3])?;
        normalise_mnist(&mut train);
        normalise_mnist(&mut test);
        (
            train,
            test,
            format!(
                "MNIST from IDX files: {}, {}, {}, {}",
                p[0], p[1], p[2], p[3]
            ),
        )
    } else if args.iter().any(|a| a == "--synthetic") {
        (
            synthetic_blobs(600, 16, 10, 31),
            synthetic_blobs(200, 16, 10, 32),
            "SYNTHETIC blobs generated by this program, NOT MNIST: these numbers say nothing about MNIST".to_string(),
        )
    } else {
        return Err("run needs real data: give --mnist with the four IDX files. For a quick check of the machinery only, use --synthetic (its results are labelled as synthetic).".into());
    };
    let archs = list(args, "--arch", "aetheron,mlp");
    for a in &archs {
        if !ARCHES.contains(a) {
            return Err(format!("unknown arch '{a}' (expected one of {ARCHES:?})"));
        }
    }
    let strategies: Vec<Strategy> = list(args, "--strategy", "none")
        .into_iter()
        .map(|s| Strategy::parse(s).ok_or_else(|| format!("unknown strategy '{s}'")))
        .collect::<Result<_, _>>()?;
    let seeds: u64 = number(args, "--seeds", 5)?;
    if seeds == 0 {
        return Err("--seeds must be above 0".into());
    }
    let cfg = RunCfg {
        epochs: number(args, "--epochs", 3)?,
        ..RunCfg::default()
    };
    write_dataset_marker(out_dir, &marker)?;
    let (fit, val) = split_validation(&train);
    let mut log = format!("Data: {marker}\n");
    for arch in &archs {
        for &s in &strategies {
            // The same tuning budget for every method: the grid inside `tune`, one fixed seed.
            let tuned = tune(arch, s, &fit, &val, &cfg);
            let mut c = cfg.clone();
            c.lr = tuned.lr;
            if let Some(l) = tuned.ewc_lambda {
                c.ewc_lambda = l;
            }
            for seed in 0..seeds {
                let r = run_split(arch, s, seed, &train, &test, &c);
                write_run(out_dir, &r)?;
                log.push_str(&format!(
                    "{} {} seed {seed}: final accuracy {:.4}, forgetting {:.4} (lr {})\n",
                    r.arch, r.strategy, r.final_avg, r.forgetting, tuned.lr
                ));
            }
        }
    }
    log.push_str(&format!(
        "\nRun files are in {out_dir}; `mouli-lab aggregate {out_dir}` makes the report.\n"
    ));
    Ok(log)
}

fn run_temporal_cmd(args: &[String]) -> Result<String, String> {
    let out_dir = flag(args, "--out").ok_or("temporal needs --out DIR")?;
    let models: Vec<TemporalModel> = list(args, "--model", "recurrent,gated,transformer")
        .into_iter()
        .map(|m| TemporalModel::parse(m).ok_or_else(|| format!("unknown model '{m}'")))
        .collect::<Result<_, _>>()?;
    let seeds: u64 = number(args, "--seeds", 3)?;
    if seeds == 0 {
        return Err("--seeds must be above 0".into());
    }
    let cfg = TemporalCfg {
        steps: number(args, "--steps", 400)?,
        ..TemporalCfg::default()
    };
    let mut results = Vec::new();
    for &m in &models {
        for seed in 0..seeds {
            results.push(run_temporal(m, seed, &cfg).map_err(|e| e.to_string())?);
        }
    }
    let table = temporal_table(&results);
    std::fs::create_dir_all(out_dir).map_err(|e| format!("create {out_dir}: {e}"))?;
    std::fs::write(format!("{out_dir}/temporal.md"), &table).map_err(|e| format!("write: {e}"))?;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn tmp(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("mouli-lab-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.to_string_lossy().into_owned()
    }

    #[test]
    fn selftest_passes_on_the_real_models() {
        let out = run(&args("selftest")).unwrap();
        assert!(out.contains("PASS aetheron without attention"), "{out}");
        assert!(out.ends_with("All gradient checks passed.\n"), "{out}");
    }

    #[test]
    fn help_and_unknown_commands_and_missing_arguments_are_clear() {
        assert!(run(&[]).unwrap().contains("selftest"));
        assert!(run(&args("nonsense"))
            .unwrap_err()
            .contains("unknown command"));
        assert!(run(&args("run")).unwrap_err().contains("--out"));
        assert!(run(&args("aggregate")).unwrap_err().contains("folder"));
        assert!(run(&args("temporal")).unwrap_err().contains("--out"));
    }

    #[test]
    fn a_run_without_real_data_is_refused_and_a_synthetic_one_is_labelled_everywhere() {
        let dir = tmp("run");
        let refused = run(&args(&format!("run --out {dir}"))).unwrap_err();
        assert!(
            refused.contains("real data") && refused.contains("--mnist"),
            "{refused}"
        );
        assert!(
            !std::path::Path::new(&dir).exists(),
            "nothing is written when refused"
        );
        let out = run(&args(&format!(
            "run --out {dir} --synthetic --arch aetheron,mlp --strategy none --seeds 2 --epochs 1"
        )))
        .unwrap();
        assert!(
            out.contains("SYNTHETIC") && out.contains("NOT MNIST"),
            "{out}"
        );
        let report = run(&args(&format!("aggregate {dir}"))).unwrap();
        assert!(report.starts_with("Data: SYNTHETIC"), "{report}");
        assert!(
            report.contains("| aetheron | none | 2 |") && report.contains("| mlp | none | 2 |"),
            "{report}"
        );
        assert!(
            report.contains("none, final accuracy, Aetheron vs the MLP"),
            "{report}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_arguments_are_named_not_ignored() {
        let dir = tmp("bad");
        for (cmd, needle) in [
            (
                format!("run --out {dir} --synthetic --arch nope"),
                "unknown arch",
            ),
            (
                format!("run --out {dir} --synthetic --strategy nope"),
                "unknown strategy",
            ),
            (format!("run --out {dir} --synthetic --seeds 0"), "above 0"),
            (
                format!("run --out {dir} --synthetic --seeds many"),
                "needs a number",
            ),
            (format!("run --out {dir} --mnist a b"), "four files"),
            (
                format!("temporal --out {dir} --model lstm"),
                "unknown model",
            ),
        ] {
            let e = run(&args(&cmd)).unwrap_err();
            assert!(e.contains(needle), "{cmd}: {e}");
        }
        // Missing IDX files are an error from the loader, not a fabricated data set.
        let e = run(&args(&format!(
            "run --out {dir} --mnist /nope/a /nope/b /nope/c /nope/d"
        )))
        .unwrap_err();
        assert!(
            e.contains("/nope/a") || e.to_lowercase().contains("read"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_temporal_command_prints_and_saves_a_table_with_stability() {
        let dir = tmp("temporal");
        let out = run(&args(&format!(
            "temporal --out {dir} --model gated,transformer --seeds 2 --steps 40"
        )))
        .unwrap();
        assert!(
            out.contains("| gated | 2 |") && out.contains("| transformer | 2 |"),
            "{out}"
        );
        assert!(out.contains("stable"), "{out}");
        assert_eq!(
            std::fs::read_to_string(format!("{dir}/temporal.md")).unwrap(),
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
