//! `mouli-slice`: shrink a dense llama/qwen2 GGUF for one domain and measure what it cost.
//!
//! ```text
//! mouli-slice MODEL.gguf --profile PROFILE.txt --eval EVAL.txt [--keep 0.8,0.65,0.5]
//!             [--down q8_0|f16] [--tolerance 0.05] [--out DIR]
//!             [--closure N] [--shots 3] [--profile-closure M]
//!             [--score activation|weighted] [--diagnose]
//! ```
//!
//! `--score weighted` ranks neurons by activation times the norm of their output weights (a 55D
//! measure) instead of activation alone; `--diagnose` prints per-layer structure and how much of
//! each layer's contribution the plan keeps.
//!
//! With `--closure N` the gate is the closure test (robust score S over five shortcut conditions,
//! N episodes each) and not plain accuracy; `--profile-closure M` also profiles M episodes per
//! condition so the kept neurons are not tuned to the clean format alone.
//!
//! PROFILE.txt and EVAL.txt hold multiple-choice items (see `mouli_slice::run::parse_items`).
//! The profile set decides which neurons are kept; the eval set is held out and decides whether a
//! slice is accepted. The two must not share items.

use mouli_llm::tokenizer::Tokenizer;
use mouli_llm::{Model, Sampler, Session};
use mouli_slice::analysis::{analyse_gguf, layer_reports, render_layers, weighted_scores};
use mouli_slice::closure::{
    decide_closure, profile_prompts_for_closure, render_closure, run_closure,
};
use mouli_slice::gate::{decide, MIN_ITEMS};
use mouli_slice::gguf_slice::{round_up_to_block, slice_gguf, DownType};
use mouli_slice::plan::{make_plan_from_importance, Rule};
use mouli_slice::report::{render, PlanSummary, RunSummary};
use mouli_slice::run::{parse_items, profile_prompts, score_items, ModelScorer};
use std::path::PathBuf;
use std::time::Instant;

struct Args {
    model: PathBuf,
    profile: PathBuf,
    eval: PathBuf,
    keep: Vec<f64>,
    down: DownType,
    tolerance: f64,
    out: Option<PathBuf>,
    closure: usize,
    shots: usize,
    profile_closure: usize,
    weighted: bool,
    diagnose: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: mouli-slice MODEL.gguf --profile PROFILE.txt --eval EVAL.txt \
         [--keep 0.8,0.65,0.5] [--down q8_0|f16] [--tolerance 0.05] [--out DIR] \
         [--closure N] [--shots 3] [--profile-closure M] [--score activation|weighted] [--diagnose]"
    );
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let model = it.next().map(PathBuf::from).unwrap_or_else(|| usage());
    let (mut profile, mut eval, mut out) = (None, None, None);
    let mut keep = vec![0.8, 0.65, 0.5];
    let mut down = DownType::Q8_0;
    let mut tolerance = 0.05;
    let (mut closure, mut shots, mut profile_closure) = (0usize, 3usize, 0usize);
    let (mut weighted, mut diagnose) = (false, false);
    while let Some(flag) = it.next() {
        if flag == "--diagnose" {
            diagnose = true;
            continue;
        }
        let Some(value) = it.next() else { usage() };
        match flag.as_str() {
            "--profile" => profile = Some(PathBuf::from(value)),
            "--eval" => eval = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--keep" => {
                keep = value
                    .split(',')
                    .map(|v| v.trim().parse::<f64>().unwrap_or_else(|_| usage()))
                    .collect();
            }
            "--down" => {
                down = match value.as_str() {
                    "q8_0" => DownType::Q8_0,
                    "f16" => DownType::F16,
                    _ => usage(),
                };
            }
            "--tolerance" => tolerance = value.parse().unwrap_or_else(|_| usage()),
            "--score" => {
                weighted = match value.as_str() {
                    "weighted" => true,
                    "activation" => false,
                    _ => usage(),
                };
            }
            "--closure" => closure = value.parse().unwrap_or_else(|_| usage()),
            "--shots" => shots = value.parse().unwrap_or_else(|_| usage()),
            "--profile-closure" => profile_closure = value.parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    Args {
        model,
        profile: profile.unwrap_or_else(|| usage()),
        eval: eval.unwrap_or_else(|| usage()),
        keep,
        down,
        tolerance,
        out,
        closure,
        shots,
        profile_closure,
        weighted,
        diagnose,
    }
}

/// Greedy generation speed on synthetic tokens: (prompt tok/s, generated tok/s), one run.
fn speed(model: &Model) -> Result<(f64, f64), String> {
    let vocab = model.config().n_vocab as u32;
    let prompt: Vec<u32> = (0..32u32).map(|i| 1 + (i * 7919) % (vocab - 1)).collect();
    let mut s = Session::new(model, 64).map_err(|e| format!("{e:?}"))?;
    let t = Instant::now();
    let mut logits = s.prefill(&prompt).map_err(|e| format!("{e:?}"))?.to_vec();
    let pp = prompt.len() as f64 / t.elapsed().as_secs_f64();
    let t = Instant::now();
    for _ in 0..16 {
        let next = Sampler::greedy().sample(&logits);
        logits = s.forward(next).map_err(|e| format!("{e:?}"))?.to_vec();
    }
    Ok((pp, 16.0 / t.elapsed().as_secs_f64()))
}

fn main() -> Result<(), String> {
    let args = parse_args();
    let read =
        |p: &PathBuf| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let profile_items = parse_items(&read(&args.profile)?).map_err(|e| e.to_string())?;
    let eval_items = parse_items(&read(&args.eval)?).map_err(|e| e.to_string())?;
    if eval_items.len() < MIN_ITEMS {
        eprintln!(
            "warning: {} eval items; the gate refuses anything under {MIN_ITEMS}",
            eval_items.len()
        );
    }
    let overlap = eval_items
        .iter()
        .filter(|e| profile_items.iter().any(|p| p.question == e.question))
        .count();
    if overlap > 0 {
        return Err(format!(
            "{overlap} eval item(s) also appear in the profile set; use disjoint sets"
        ));
    }

    let file = std::fs::read(&args.model).map_err(|e| format!("{}: {e}", args.model.display()))?;
    let model = Model::from_bytes(file.clone()).map_err(|e| format!("{e:?}"))?;
    let tokenizer = Tokenizer::from_gguf(model.gguf()).map_err(|e| format!("{e:?}"))?;
    let d_ff = model.config().n_ff;
    println!(
        "model: {} layers, d_ff {d_ff}; profile items {}, eval items {}",
        model.config().n_layer,
        profile_items.len(),
        eval_items.len()
    );

    let t = Instant::now();
    let mut prompts: Vec<String> = profile_items.iter().map(|i| i.prompt()).collect();
    if args.profile_closure > 0 {
        let extra =
            profile_prompts_for_closure(&profile_items, args.profile_closure, args.shots, 1)
                .map_err(|e| e.to_string())?;
        println!("adding {} closure prompts to the profile", extra.len());
        prompts.extend(extra);
    }
    let (profiler, tokens) =
        profile_prompts(&model, &tokenizer, &prompts).map_err(|e| e.to_string())?;
    println!(
        "profiled {tokens} tokens in {:.1}s",
        t.elapsed().as_secs_f64()
    );

    let analysis = if args.weighted || args.diagnose {
        let t = Instant::now();
        let a = analyse_gguf(&file).map_err(|e| e.to_string())?;
        println!("55D weight analysis in {:.1}s", t.elapsed().as_secs_f64());
        Some(a)
    } else {
        None
    };
    let scores: Vec<Vec<f64>> = match (&analysis, args.weighted) {
        (Some(a), true) => weighted_scores(&profiler, a).map_err(|e| e.to_string())?,
        _ => (0..profiler.layers())
            .map(|l| profiler.importance(l))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
    };
    println!(
        "ranking neurons by {}",
        if args.weighted {
            "activation x output weight norm"
        } else {
            "activation"
        }
    );

    let t = Instant::now();
    let full = score_items(&model, &tokenizer, &eval_items).map_err(|e| e.to_string())?;
    println!("scored the full model in {:.1}s", t.elapsed().as_secs_f64());
    let full_closure = if args.closure > 0 {
        let t = Instant::now();
        let mut scorer = ModelScorer::new(&model, &tokenizer).map_err(|e| e.to_string())?;
        let report = run_closure(
            &mut scorer,
            &profile_items,
            &eval_items,
            args.closure,
            args.shots,
            7,
        )
        .map_err(|e| e.to_string())?;
        println!(
            "closure test of the full model in {:.0}s: S = {:?}",
            t.elapsed().as_secs_f64(),
            report.summary.robust_s
        );
        Some(report)
    } else {
        None
    };
    let (full_pp, full_tg) = speed(&model)?;
    println!(
        "full: {} MB, prompt {full_pp:.1} tok/s, generate {full_tg:.2} tok/s",
        file.len() / 1_000_000
    );

    let mut runs = Vec::new();
    let mut closure_runs = Vec::new();
    for &fraction in &args.keep {
        let wanted = (d_ff as f64 * fraction - 1e-9).ceil() as usize;
        let Some(count) = round_up_to_block(wanted, d_ff) else {
            eprintln!("keep {fraction}: no valid width, skipped");
            continue;
        };
        let plan = make_plan_from_importance(&scores, count as f64 / d_ff as f64, Rule::Uniform)
            .map_err(|e| e.to_string())?;
        if args.diagnose {
            if let Some(a) = &analysis {
                let reports =
                    layer_reports(a, &profiler, &scores, &plan).map_err(|e| e.to_string())?;
                println!("\nkeep {fraction}:\n{}", render_layers(&reports));
            }
        }
        let t = Instant::now();
        let sliced = slice_gguf(&file, &plan.keep, args.down).map_err(|e| e.to_string())?;
        println!(
            "keep {fraction} -> {count} of {d_ff} neurons: {} MB ({:.0}%) in {:.1}s",
            sliced.bytes_after / 1_000_000,
            100.0 * sliced.bytes_after as f64 / sliced.bytes_before as f64,
            t.elapsed().as_secs_f64()
        );
        if let Some(dir) = &args.out {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let name = format!("sliced-{count}.gguf");
            std::fs::write(dir.join(&name), &sliced.bytes).map_err(|e| e.to_string())?;
        }
        let small = Model::from_bytes(sliced.bytes).map_err(|e| format!("{e:?}"))?;
        let scores = score_items(&small, &tokenizer, &eval_items).map_err(|e| e.to_string())?;
        let (pp, tg) = speed(&small)?;
        println!("  prompt {pp:.1} tok/s, generate {tg:.2} tok/s");
        if let Some(full_report) = &full_closure {
            let mut scorer = ModelScorer::new(&small, &tokenizer).map_err(|e| e.to_string())?;
            let report = run_closure(
                &mut scorer,
                &profile_items,
                &eval_items,
                args.closure,
                args.shots,
                7,
            )
            .map_err(|e| e.to_string())?;
            let verdict =
                decide_closure(full_report, &report, args.tolerance).map_err(|e| e.to_string())?;
            println!(
                "  closure: S = {:?}; {}",
                report.summary.robust_s, verdict.note
            );
            closure_runs.push((
                format!("keep {fraction} ({count} neurons)"),
                report,
                verdict,
            ));
        }
        let decision = decide(&full, &scores, args.tolerance).map_err(|e| e.to_string())?;
        runs.push(RunSummary {
            plan: PlanSummary {
                keep_fraction: count as f64 / d_ff as f64,
                kept_neurons: plan.kept_total(),
            },
            decision,
        });
    }
    println!("\n{}", render(&runs));
    if let Some(full_report) = &full_closure {
        println!("{}", render_closure(full_report, &closure_runs));
    }
    Ok(())
}
