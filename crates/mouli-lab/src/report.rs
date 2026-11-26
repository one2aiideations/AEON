//! Writing and aggregating run results (one JSON object per run file).

use crate::continual::RunResult;
use crate::F;
use std::fs;

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn num(v: F) -> String {
    // JSON has no NaN or infinity; write null so the file stays valid.
    if v.is_finite() {
        format!("{}", v)
    } else {
        "null".to_string()
    }
}

pub fn run_to_json(r: &RunResult) -> String {
    let per: Vec<String> = r.per_task.iter().map(|v| num(*v)).collect();
    let trans = match r.transductive_final_avg {
        Some(v) => num(v),
        None => "null".to_string(),
    };
    format!(
        "{{\"arch\":\"{}\",\"strategy\":\"{}\",\"seed\":{},\"final_avg\":{},\"per_task\":[{}],\"forgetting\":{},\"pooled\":{},\"transductive_final_avg\":{},\"seconds\":{}}}",
        escape(&r.arch),
        escape(&r.strategy),
        r.seed,
        num(r.final_avg),
        per.join(","),
        num(r.forgetting),
        num(r.pooled),
        trans,
        num(r.seconds)
    )
}

pub fn write_run(dir: &str, r: &RunResult) -> Result<String, String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {}", dir, e))?;
    let path = format!("{}/run_{}_{}_{}.jsonl", dir, r.arch, r.strategy, r.seed);
    fs::write(&path, format!("{}\n", run_to_json(r)))
        .map_err(|e| format!("write {}: {}", path, e))?;
    Ok(path)
}

fn extract_num(line: &str, key: &str) -> Option<F> {
    let pat = format!("\"{}\":", key);
    let i = line.find(&pat)? + pat.len();
    let rest = line[i..].trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().parse::<F>().ok()
}

fn extract_str(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", key);
    let i = line.find(&pat)? + pat.len();
    let rest = &line[i..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn mean_std(v: &[F]) -> (F, F) {
    if v.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    let m: F = v.iter().sum::<F>() / v.len() as F;
    let var: F = v.iter().map(|x| (x - m) * (x - m)).sum::<F>() / v.len() as F;
    (m, var.sqrt())
}

type Entry = (String, String, u64, F, F, F, Option<F>);

/// Parses the run files in `dir` (only the keys this module writes), for tests and `aggregate`.
pub fn read_entries(dir: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    let rd = fs::read_dir(dir).map_err(|e| format!("read {}: {}", dir, e))?;
    for e in rd {
        let e = e.map_err(|e| e.to_string())?;
        let path = e.path();
        if path.extension().map(|s| s == "jsonl").unwrap_or(false) {
            let content =
                fs::read_to_string(&path).map_err(|e| format!("read {:?}: {}", path, e))?;
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Some(arch) = extract_str(line, "arch") else {
                    continue;
                };
                let Some(strategy) = extract_str(line, "strategy") else {
                    continue;
                };
                let seed = extract_num(line, "seed").unwrap_or(0.0) as u64;
                let Some(fa) = extract_num(line, "final_avg") else {
                    continue;
                };
                let forgetting = extract_num(line, "forgetting").unwrap_or(f64::NAN);
                let pooled = extract_num(line, "pooled").unwrap_or(f64::NAN);
                let trans = extract_num(line, "transductive_final_avg");
                entries.push((arch, strategy, seed, fa, forgetting, pooled, trans));
            }
        }
    }
    Ok(entries)
}

/// File in a run folder that says which data the runs used.
pub const DATASET_FILE: &str = "DATASET.txt";

/// Records which data the runs in `dir` used, so a report cannot be mistaken for another data set.
pub fn write_dataset_marker(dir: &str, text: &str) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {}", dir, e))?;
    fs::write(
        format!("{}/{}", dir, DATASET_FILE),
        format!("{}\n", text.trim()),
    )
    .map_err(|e| format!("write marker: {}", e))
}

/// Reads the marker, or says the data is unknown.
#[must_use]
pub fn read_dataset_marker(dir: &str) -> String {
    fs::read_to_string(format!("{}/{}", dir, DATASET_FILE))
        .map(|t| t.trim().to_string())
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "unknown (no DATASET.txt in this folder)".to_string())
}

/// Aggregates the run files in `dir` into markdown.
pub fn aggregate(dir: &str) -> Result<String, String> {
    let entries = read_entries(dir)?;
    Ok(render(&entries, &read_dataset_marker(dir)))
}

/// Renders entries as markdown: a table of every method, then for each strategy how Aetheron
/// compared with the MLP seed by seed, for accuracy and for forgetting, wins and losses alike.
#[must_use]
pub fn render(entries: &[Entry], dataset: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("Data: {}\n\n", dataset));
    out.push_str("| arch | strategy | n | final_avg mean±std | forgetting mean±std | pooled mean±std | transductive mean±std |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    let mut keys: Vec<(String, String)> =
        entries.iter().map(|e| (e.0.clone(), e.1.clone())).collect();
    keys.sort();
    keys.dedup();
    for (a, s) in &keys {
        let sel: Vec<&Entry> = entries.iter().filter(|e| &e.0 == a && &e.1 == s).collect();
        let fa: Vec<F> = sel.iter().map(|e| e.3).collect();
        let fg: Vec<F> = sel.iter().map(|e| e.4).collect();
        let pl: Vec<F> = sel.iter().map(|e| e.5).collect();
        let tr: Vec<F> = sel.iter().filter_map(|e| e.6).collect();
        let (m1, s1) = mean_std(&fa);
        let (m2, s2) = mean_std(&fg);
        let (m3, s3) = mean_std(&pl);
        let tr_s = if tr.is_empty() {
            "n/a".to_string()
        } else {
            let (m, s) = mean_std(&tr);
            format!("{:.4} ± {:.4}", m, s)
        };
        out.push_str(&format!(
            "| {} | {} | {} | {:.4} ± {:.4} | {:.4} ± {:.4} | {:.4} ± {:.4} | {} |\n",
            a,
            s,
            sel.len(),
            m1,
            s1,
            m2,
            s2,
            m3,
            s3,
            tr_s
        ));
    }
    out.push_str("\n## Aetheron compared with the MLP, seed by seed (same strategy)\n\n");
    out.push_str("A positive difference is better for accuracy; for forgetting, lower is better. Intervals are 95% bootstrap over seeds, p-values are exact sign-flip tests; with few seeds neither can show much.\n\n");
    let mut strategies: Vec<String> = entries.iter().map(|e| e.1.clone()).collect();
    strategies.sort();
    strategies.dedup();
    let mut any = false;
    for s in &strategies {
        let ae: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.0 == "aetheron" && &e.1 == s)
            .collect();
        let ml: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.0 == "mlp" && &e.1 == s)
            .collect();
        for (metric, higher_better, pick) in [
            ("final accuracy", true, (|e: &Entry| e.3) as fn(&Entry) -> F),
            ("forgetting", false, (|e: &Entry| e.4) as fn(&Entry) -> F),
        ] {
            let diffs: Vec<F> = ae
                .iter()
                .filter_map(|a| ml.iter().find(|m| m.2 == a.2).map(|m| pick(a) - pick(m)))
                .collect();
            if diffs.is_empty() {
                continue;
            }
            any = true;
            let p = crate::stats::paired(&diffs, 20240601);
            let ci = p
                .ci95
                .map_or("n/a".to_string(), |(l, h)| format!("{l:+.4} to {h:+.4}"));
            let pv = p.p_value.map_or("n/a".to_string(), |v| format!("{v:.4}"));
            out.push_str(&format!(
                "- **{s}, {metric}**: n={}, mean difference {:+.4}, interval {ci}, p={pv}; Aetheron better on {}, worse on {}, equal on {}. {}\n",
                p.n,
                p.mean,
                if higher_better { p.wins } else { p.losses },
                if higher_better { p.losses } else { p.wins },
                p.ties,
                p.describe("Aetheron", "the MLP", higher_better)
            ));
        }
    }
    if !any {
        out.push_str("No seed has both an Aetheron run and an MLP run for the same strategy, so nothing can be compared.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(arch: &str, strat: &str, seed: u64, acc: F, forget: F) -> Entry {
        (arch.into(), strat.into(), seed, acc, forget, 0.5, None)
    }

    fn eight_seeds() -> Vec<Entry> {
        let mut v = Vec::new();
        for s in 0..8u64 {
            let j = s as F * 0.001;
            // Aetheron is more accurate (+0.05) but forgets more (+0.10) than the MLP.
            v.push(e("aetheron", "none", s, 0.60 + j, 0.30 + j));
            v.push(e("mlp", "none", s, 0.55 + j, 0.20 + j));
        }
        v
    }

    #[test]
    fn the_report_states_losses_as_plainly_as_wins_and_names_the_data() {
        let r = render(&eight_seeds(), "synthetic blobs, not MNIST");
        assert!(r.starts_with("Data: synthetic blobs, not MNIST"), "{r}");
        let acc = r
            .lines()
            .find(|l| l.contains("none, final accuracy"))
            .unwrap();
        assert!(acc.contains("better on 8, worse on 0"), "{acc}");
        assert!(acc.contains("Aetheron was better than the MLP"), "{acc}");
        let fg = r.lines().find(|l| l.contains("none, forgetting")).unwrap();
        assert!(fg.contains("better on 0, worse on 8"), "{fg}");
        assert!(
            fg.contains("the MLP was better than Aetheron"),
            "lower forgetting is better: {fg}"
        );
    }

    #[test]
    fn missing_pairs_and_unknown_data_are_said_not_hidden() {
        let only_mlp = vec![e("mlp", "none", 1, 0.5, 0.1)];
        let r = render(&only_mlp, "unknown");
        assert!(r.contains("nothing can be compared"), "{r}");
        let dir = std::env::temp_dir().join(format!("mouli-lab-marker-{}", std::process::id()));
        let d = dir.to_str().unwrap();
        let _ = fs::remove_dir_all(d);
        fs::create_dir_all(d).unwrap();
        assert!(read_dataset_marker(d).starts_with("unknown"));
        write_dataset_marker(d, "  MNIST (IDX files given)  ").unwrap();
        assert_eq!(read_dataset_marker(d), "MNIST (IDX files given)");
        let _ = fs::remove_dir_all(d);
    }
}
