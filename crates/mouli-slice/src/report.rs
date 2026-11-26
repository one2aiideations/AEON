//! Plain-text Markdown reporting.
//!
//! Produces a human-readable table without JSON serialization or external
//! formatting dependencies.
//!
//! Reports describe supplied plans and measured decisions. They do not
//! establish unmeasured quality, generalization, or speed improvements.

use crate::gate::Decision;

/// Summary of one retention plan for reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanSummary {
    /// Requested neuron-retention fraction.
    pub keep_fraction: f64,

    /// Total retained neurons across all layers.
    pub kept_neurons: usize,
}

/// A plan summary paired with its measured quality decision.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    /// Plan information.
    pub plan: PlanSummary,

    /// Quality comparison for that plan.
    pub decision: Decision,
}

/// Renders a Markdown report containing one row per run.
///
/// The table includes retention fraction, retained neuron count, mean
/// scores, robust scores, mean and robust drops, and acceptance status.
///
/// A "What this does not show" section explicitly identifies the limits
/// of the evidence.
///
/// Numeric values are formatted deterministically. Pipe characters in
/// textual cells are escaped so they cannot corrupt the Markdown table.
///
/// This function does not independently verify that supplied decisions
/// were actually measured. The caller must supply genuine scorer results
/// if the report is to be treated as experimental evidence.
#[must_use]
pub fn render(runs: &[RunSummary]) -> String {
    let mut output = String::new();

    output.push_str("# MOULI Slice Report\n\n");

    output.push_str(
        "| Keep fraction | Kept neurons | Full vs sliced mean \
         | Full vs sliced robust | Drop (mean / robust) | Accepted |\n",
    );

    output.push_str("|---:|---:|---:|---:|---:|:---:|\n");

    for run in runs {
        let decision = &run.decision;

        let accepted = if decision.accepted { "Yes" } else { "No" };

        let mean = format!("{:.6} / {:.6}", decision.full_mean, decision.sliced_mean);

        let robust = format!(
            "{:.6} / {:.6}",
            decision.full_robust, decision.sliced_robust
        );

        let drops = format!("{:.6} / {:.6}", decision.drop_mean, decision.drop_robust);

        output.push_str(&format!(
            "| {:.6} | {} | {} | {} | {} | {} |\n",
            run.plan.keep_fraction,
            run.plan.kept_neurons,
            escape_cell(&mean),
            escape_cell(&robust),
            escape_cell(&drops),
            accepted,
        ));
    }

    output.push_str("\n## Decision notes\n\n");

    for (index, run) in runs.iter().enumerate() {
        output.push_str(&format!(
            "- Run {} (keep {:.6}): {}\n",
            index + 1,
            run.plan.keep_fraction,
            escape_cell(&run.decision.note),
        ));
    }

    output.push_str("\n## What this does not show\n\n");
    output.push_str(
        "- Scores cover only the held-out items given; they do not \
         establish generalization to other questions or distributions.\n",
    );
    output.push_str(
        "- A slice can fail outside the profiled domain; activation \
         statistics do not guarantee domain-wide preservation.\n",
    );
    output.push_str(
        "- Speed was not measured here; fewer neurons do not establish \
         a particular inference latency improvement.\n",
    );

    output
}

fn escape_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(accepted: bool) -> Decision {
        Decision {
            accepted,
            full_mean: 0.95,
            sliced_mean: 0.92,
            full_robust: 0.80,
            sliced_robust: 0.75,
            drop_mean: 0.03,
            drop_robust: 0.05,
            note: "Measured held-out result".to_string(),
        }
    }

    fn run(fraction: f64, kept: usize, accepted: bool) -> RunSummary {
        RunSummary {
            plan: PlanSummary {
                keep_fraction: fraction,
                kept_neurons: kept,
            },
            decision: decision(accepted),
        }
    }

    #[test]
    fn renders_empty_report() {
        let report = render(&[]);

        assert!(report.contains("# MOULI Slice Report"));
        assert!(report.contains("| Keep fraction"));
        assert!(report.contains("## What this does not show"));
    }

    #[test]
    fn renders_one_run() {
        let report = render(&[run(0.5, 1024, true)]);

        assert!(report.contains("0.500000"));
        assert!(report.contains("1024"));
        assert!(report.contains("Yes"));
        assert!(report.contains("0.950000 / 0.920000"));
    }

    #[test]
    fn renders_rejected_run() {
        assert!(render(&[run(0.25, 512, false)]).contains("| No |"));
    }

    #[test]
    fn escapes_pipe_characters_in_notes() {
        let mut result = decision(false);
        result.note = "mean | robust".to_string();

        let runs = vec![RunSummary {
            plan: PlanSummary {
                keep_fraction: 0.5,
                kept_neurons: 100,
            },
            decision: result,
        }];

        assert!(render(&runs).contains("mean \\| robust"));
    }

    #[test]
    fn includes_all_required_limitations() {
        let report = render(&[]);

        assert!(report.contains("held-out items"));
        assert!(report.contains("profiled domain"));
        assert!(report.contains("Speed was not measured"));
    }

    #[test]
    fn includes_one_row_per_run() {
        let report = render(&[run(0.25, 100, false), run(0.50, 200, true)]);

        let rows = report.lines().filter(|l| l.starts_with("| 0.")).count();
        assert_eq!(rows, 2);
        assert!(report.contains("0.250000"));
        assert!(report.contains("0.500000"));
    }
}
