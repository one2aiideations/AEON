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

/// Aggregates the run files into markdown, with paired per-seed aetheron-minus-mlp differences.
pub fn aggregate(dir: &str) -> Result<String, String> {
    let entries = read_entries(dir)?;
    let mut out = String::new();
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
    out.push_str("\n## Paired per-seed differences (aetheron − mlp), same strategy\n\n");
    let mut strategies: Vec<String> = entries.iter().map(|e| e.1.clone()).collect();
    strategies.sort();
    strategies.dedup();
    for s in &strategies {
        let ae: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.0 == "aetheron" && &e.1 == s)
            .collect();
        let ml: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.0 == "mlp" && &e.1 == s)
            .collect();
        let mut diffs = Vec::new();
        for a in &ae {
            if let Some(m) = ml.iter().find(|m| m.2 == a.2) {
                diffs.push(a.3 - m.3);
            }
        }
        if !diffs.is_empty() {
            let (m, sd) = mean_std(&diffs);
            out.push_str(&format!(
                "strategy={}: n={} mean diff = {:+.4}, std = {:.4}\n",
                s,
                diffs.len(),
                m,
                sd
            ));
        }
    }
    Ok(out)
}
