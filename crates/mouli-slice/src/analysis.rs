//! Weight and structure measures from the 55D engine, used to choose and check a slice.
//!
//! What it adds to activation profiling:
//!
//! - **Output-weighted importance.** A neuron matters by how strongly it fires *and* by how much
//!   it can move the output: `|activation| x norm of its column of the down matrix`. A neuron that
//!   fires often but writes almost nothing is a better cut than one that fires less but writes a
//!   lot. ([`weighted_importance`]).
//! - **Structure per layer** from the ported 55D functions: the fraction of near-duplicate gate
//!   rows, the fraction of outlier weights in the down matrix, and its weight entropy. A layer
//!   with many near-duplicate neurons has slack; one with many outlier weights is fragile.
//! - **Dead neurons** and the **share of each layer's contribution a plan keeps**, so the layers a
//!   slice hurts most are visible before any model is scored.
//!
//! What this does not show: these are properties of the weights and of the profiled prompts.
//! They guide the choice; only scoring the sliced model (see [`crate::closure`]) says whether it
//! still works. The near-duplicate fraction is the 55D engine's approximation (rows sorted by
//! their first coordinate and neighbours compared), so it can miss duplicates that sort apart.

use crate::plan::Plan;
use crate::profile::Profiler;
use crate::SliceError;
use mouli_llm::compress::{stats, Matrix};
use mouli_llm::gguf::parse;
use mouli_llm::quant::dequantize_vec;
use mouli_llm::sparse::column_norms;

const DUPLICATE_THRESHOLD: f64 = 0.995;
const DUPLICATE_SAMPLES: usize = 4096;
const OUTLIER_SIGMA: f64 = 3.0;
const ENTROPY_BINS: usize = 128;

/// Weight measures of one layer's feed-forward block.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerAnalysis {
    /// Layer index.
    pub layer: usize,
    /// Norm of each neuron's column of the down matrix (how much it can write to the output).
    pub out_norm: Vec<f64>,
    /// Fraction of near-duplicate neighbours among gate rows, if it could be computed.
    pub gate_duplicate_fraction: Option<f64>,
    /// Fraction of down-matrix weights beyond three standard deviations.
    pub down_outlier_fraction: Option<f64>,
    /// Entropy in bits of the down-matrix weight histogram.
    pub down_entropy_bits: Option<f64>,
}

fn unreadable(what: &str, e: &dyn std::fmt::Debug) -> SliceError {
    let _ = (what, e);
    SliceError::BadShape("the model file could not be read for weight analysis")
}

fn read_f32(
    gguf: &mouli_llm::gguf::Gguf,
    file: &[u8],
    name: &str,
    dims: [u64; 2],
) -> Result<Vec<f32>, SliceError> {
    let info = gguf
        .tensor(name)
        .ok_or(SliceError::BadShape("a feed-forward tensor is missing"))?;
    if info.dims != dims {
        return Err(SliceError::BadShape(
            "a feed-forward tensor has unexpected dimensions",
        ));
    }
    let bytes = gguf
        .tensor_data(file, name)
        .map_err(|e| unreadable(name, &e))?;
    let n = usize::try_from(info.n_elements)
        .map_err(|_| SliceError::BadParameter("tensor element count"))?;
    dequantize_vec(info.ty, bytes, n).map_err(|e| unreadable(name, &e))
}

/// Measures every layer of a dense llama or qwen2 GGUF. One layer's matrices are held in memory
/// at a time (about three times `d_ff x d_model` floats).
///
/// # Errors
/// A [`SliceError`] for an unreadable or unsupported file or a missing tensor.
pub fn analyse_gguf(file: &[u8]) -> Result<Vec<LayerAnalysis>, SliceError> {
    let gguf = parse(file).map_err(|e| unreadable("file", &e))?;
    let arch = gguf
        .architecture()
        .map_err(|e| unreadable("architecture", &e))?
        .to_string();
    if arch != "llama" && arch != "qwen2" {
        return Err(SliceError::BadParameter(
            "architecture must be llama or qwen2",
        ));
    }
    let get = |suffix: &str| -> Result<usize, SliceError> {
        let v = gguf.arch_u32(suffix).map_err(|e| unreadable(suffix, &e))?;
        usize::try_from(v).map_err(|_| SliceError::BadParameter("metadata size"))
    };
    let (layers, d_model, d_ff) = (
        get("block_count")?,
        get("embedding_length")?,
        get("feed_forward_length")?,
    );
    if layers == 0 || d_model == 0 || d_ff == 0 {
        return Err(SliceError::BadParameter("model dimensions"));
    }
    let (m, f) = (d_model as u64, d_ff as u64);
    let mut out = Vec::with_capacity(layers);
    for layer in 0..layers {
        let gate = read_f32(&gguf, file, &format!("blk.{layer}.ffn_gate.weight"), [m, f])?;
        let down = read_f32(&gguf, file, &format!("blk.{layer}.ffn_down.weight"), [f, m])?;
        let out_norm = column_norms(&down, d_model, d_ff)
            .map_err(|_| SliceError::BadShape("down matrix shape"))?
            .into_iter()
            .map(f64::from)
            .collect();
        let gate_duplicate_fraction = Matrix::new(&gate, d_ff, d_model).ok().and_then(|mat| {
            stats::near_duplicate_fraction(mat, DUPLICATE_THRESHOLD, DUPLICATE_SAMPLES)
        });
        out.push(LayerAnalysis {
            layer,
            out_norm,
            gate_duplicate_fraction,
            down_outlier_fraction: stats::outlier_fraction(&down, OUTLIER_SIGMA),
            down_entropy_bits: stats::entropy_bits(&down, ENTROPY_BINS).ok().flatten(),
        });
    }
    Ok(out)
}

/// `activation[j] x out_norm[j]` for every neuron of one layer.
///
/// # Errors
/// A [`SliceError`] for different lengths, an empty layer, or a negative or non-finite value.
pub fn weighted_importance(activation: &[f64], out_norm: &[f64]) -> Result<Vec<f64>, SliceError> {
    if activation.is_empty() {
        return Err(SliceError::EmptyInput("layer importance"));
    }
    if activation.len() != out_norm.len() {
        return Err(SliceError::BadShape(
            "activation and output norm lengths differ",
        ));
    }
    activation
        .iter()
        .zip(out_norm)
        .map(|(a, w)| {
            if !a.is_finite() || !w.is_finite() {
                Err(SliceError::NonFinite("importance input"))
            } else if *a < 0.0 || *w < 0.0 {
                Err(SliceError::BadParameter(
                    "importance inputs must not be negative",
                ))
            } else {
                Ok(a * w)
            }
        })
        .collect()
}

/// Expected squared change of a layer's output if each neuron is removed:
/// `mean(a^2) x norm(output column)^2`.
///
/// This is the second-order saliency of Optimal Brain Damage specialised to dropping a whole
/// neuron, assuming neurons are uncorrelated on the profiled prompts. Squaring weights big
/// contributors more than [`weighted_importance`] does. It ignores correlation between neurons
/// (two near-identical neurons both look important) and says nothing about what the prompts did
/// not cover.
///
/// # Errors
/// A [`SliceError`] for different lengths, an empty layer, or a negative or non-finite value.
pub fn saliency_importance(mean_square: &[f64], out_norm: &[f64]) -> Result<Vec<f64>, SliceError> {
    if mean_square.is_empty() {
        return Err(SliceError::EmptyInput("layer importance"));
    }
    if mean_square.len() != out_norm.len() {
        return Err(SliceError::BadShape(
            "mean square and output norm lengths differ",
        ));
    }
    mean_square
        .iter()
        .zip(out_norm)
        .map(|(m, w)| {
            if !m.is_finite() || !w.is_finite() {
                Err(SliceError::NonFinite("saliency input"))
            } else if *m < 0.0 || *w < 0.0 {
                Err(SliceError::BadParameter(
                    "saliency inputs must not be negative",
                ))
            } else {
                Ok(m * w * w)
            }
        })
        .collect()
}

/// Squared-saliency scores for every layer, from a profile and a weight analysis.
///
/// # Errors
/// A [`SliceError`] when the two cover different layers or neuron counts.
pub fn saliency_scores(
    profiler: &Profiler,
    analysis: &[LayerAnalysis],
) -> Result<Vec<Vec<f64>>, SliceError> {
    if analysis.len() != profiler.layers() {
        return Err(SliceError::BadShape(
            "profile and weight analysis cover different layers",
        ));
    }
    analysis
        .iter()
        .enumerate()
        .map(|(layer, a)| saliency_importance(&profiler.mean_square(layer)?, &a.out_norm))
        .collect()
}

/// Output-weighted importance for every layer, from a profile and a weight analysis.
///
/// # Errors
/// A [`SliceError`] when the two cover different layers or neuron counts.
pub fn weighted_scores(
    profiler: &Profiler,
    analysis: &[LayerAnalysis],
) -> Result<Vec<Vec<f64>>, SliceError> {
    if analysis.len() != profiler.layers() {
        return Err(SliceError::BadShape(
            "profile and weight analysis cover different layers",
        ));
    }
    analysis
        .iter()
        .enumerate()
        .map(|(layer, a)| weighted_importance(&profiler.importance(layer)?, &a.out_norm))
        .collect()
}

/// What one layer looks like and what a plan keeps of it.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerReport {
    /// Layer index.
    pub layer: usize,
    /// Fraction of neurons that never fired on the profiled prompts.
    pub dead_fraction: f64,
    /// Near-duplicate fraction of the gate rows.
    pub duplicate_fraction: Option<f64>,
    /// Outlier fraction of the down weights.
    pub outlier_fraction: Option<f64>,
    /// Entropy of the down weights, in bits.
    pub entropy_bits: Option<f64>,
    /// Share (0 to 1) of the layer's total score the plan keeps.
    pub retained: f64,
}

/// One report per layer for `plan` under `scores`.
///
/// # Errors
/// A [`SliceError`] when the plan, scores, profile and analysis disagree on layers or neurons.
pub fn layer_reports(
    analysis: &[LayerAnalysis],
    profiler: &Profiler,
    scores: &[Vec<f64>],
    plan: &Plan,
) -> Result<Vec<LayerReport>, SliceError> {
    let layers = profiler.layers();
    if analysis.len() != layers || scores.len() != layers || plan.keep.len() != layers {
        return Err(SliceError::BadShape("inputs cover different layers"));
    }
    let mut out = Vec::with_capacity(layers);
    for layer in 0..layers {
        let fire = profiler.fire_rate(layer)?;
        let score = &scores[layer];
        if score.len() != fire.len() {
            return Err(SliceError::BadShape(
                "scores and profile differ in neuron count",
            ));
        }
        let total: f64 = score.iter().sum();
        let mut kept = 0.0;
        for &j in &plan.keep[layer] {
            kept += *score
                .get(j)
                .ok_or(SliceError::BadParameter("a kept neuron is out of range"))?;
        }
        let a = &analysis[layer];
        out.push(LayerReport {
            layer,
            dead_fraction: fire.iter().filter(|r| **r == 0.0).count() as f64 / fire.len() as f64,
            duplicate_fraction: a.gate_duplicate_fraction,
            outlier_fraction: a.down_outlier_fraction,
            entropy_bits: a.down_entropy_bits,
            retained: if total > 0.0 { kept / total } else { 1.0 },
        });
    }
    Ok(out)
}

/// A Markdown table of the layer reports, with the layers a plan keeps least of listed first.
#[must_use]
pub fn render_layers(reports: &[LayerReport]) -> String {
    let opt = |v: Option<f64>, digits: usize| {
        v.map_or_else(|| "-".to_string(), |x| format!("{x:.digits$}"))
    };
    let mut out = String::from(
        "| Layer | Kept share | Dead | Near-duplicate gate rows | Outlier weights | Weight entropy (bits) |\n|---:|---:|---:|---:|---:|---:|\n",
    );
    for r in reports {
        out.push_str(&format!(
            "| {} | {:.3} | {:.3} | {} | {} | {} |\n",
            r.layer,
            r.retained,
            r.dead_fraction,
            opt(r.duplicate_fraction, 3),
            opt(r.outlier_fraction, 4),
            opt(r.entropy_bits, 2)
        ));
    }
    let mut order: Vec<&LayerReport> = reports.iter().collect();
    order.sort_by(|a, b| {
        a.retained
            .total_cmp(&b.retained)
            .then(a.layer.cmp(&b.layer))
    });
    let weakest: Vec<String> = order
        .iter()
        .take(3)
        .map(|r| format!("{} ({:.3})", r.layer, r.retained))
        .collect();
    out.push_str(&format!(
        "\nLayers keeping the least of their contribution: {}.\n\n\
         These are properties of the weights and the profiled prompts; they guide the choice and \
         do not show that the sliced model still works.\n",
        if weakest.is_empty() {
            "-".to_string()
        } else {
            weakest.join(", ")
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{make_plan_from_importance, Rule};
    use crate::ActivationSink;
    use mouli_llm::gguf::{GgmlType, GgufWriter, Value};

    const D_MODEL: usize = 32;
    const D_FF: usize = 64;

    fn bytes(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// A 2-layer model. `down(layer, row, neuron)` and `gate(layer, neuron, input)` give weights.
    fn model(
        down: impl Fn(usize, usize, usize) -> f32,
        gate: impl Fn(usize, usize, usize) -> f32,
    ) -> Vec<u8> {
        let mut w = GgufWriter::new();
        w.add_metadata("general.architecture", Value::String("llama".into()));
        w.add_metadata("llama.block_count", Value::U32(2));
        w.add_metadata("llama.embedding_length", Value::U32(D_MODEL as u32));
        w.add_metadata("llama.feed_forward_length", Value::U32(D_FF as u32));
        for layer in 0..2 {
            let g: Vec<f32> = (0..D_FF)
                .flat_map(|n| (0..D_MODEL).map(move |i| (n, i)))
                .map(|(n, i)| gate(layer, n, i))
                .collect();
            let up = vec![0.5f32; D_FF * D_MODEL];
            let d: Vec<f32> = (0..D_MODEL)
                .flat_map(|r| (0..D_FF).map(move |n| (r, n)))
                .map(|(r, n)| down(layer, r, n))
                .collect();
            w.add_tensor(
                &format!("blk.{layer}.ffn_gate.weight"),
                &[D_MODEL as u64, D_FF as u64],
                GgmlType::F32,
                bytes(&g),
            )
            .unwrap();
            w.add_tensor(
                &format!("blk.{layer}.ffn_up.weight"),
                &[D_MODEL as u64, D_FF as u64],
                GgmlType::F32,
                bytes(&up),
            )
            .unwrap();
            w.add_tensor(
                &format!("blk.{layer}.ffn_down.weight"),
                &[D_FF as u64, D_MODEL as u64],
                GgmlType::F32,
                bytes(&d),
            )
            .unwrap();
        }
        w.finish().unwrap()
    }

    fn varied(layer: usize, a: usize, b: usize) -> f32 {
        // A hash of (layer, a, b): deterministic, and unrelated from one row to the next.
        let mut x = (layer as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (a as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9)
            ^ (b as u64 + 1).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        (x % 20_001) as f32 / 10_000.0 - 1.0
    }

    #[test]
    fn output_norms_are_the_column_norms_of_the_down_matrix() {
        let file = model(|_, _, n| (n + 1) as f32, varied);
        let analysis = analyse_gguf(&file).unwrap();
        assert_eq!(analysis.len(), 2);
        for a in &analysis {
            assert_eq!(a.out_norm.len(), D_FF);
            for (n, norm) in a.out_norm.iter().enumerate() {
                let expected = (n + 1) as f64 * (D_MODEL as f64).sqrt();
                assert!(
                    (norm - expected).abs() < 1e-3 * expected,
                    "{n}: {norm} vs {expected}"
                );
            }
            assert!(a.down_entropy_bits.is_some());
        }
    }

    #[test]
    fn identical_gate_rows_are_counted_as_duplicates_and_distinct_ones_are_not() {
        let same = model(varied, |_, _, i| 0.1 * (i as f32 + 1.0));
        let a = analyse_gguf(&same).unwrap();
        assert!(
            a[0].gate_duplicate_fraction.unwrap() > 0.99,
            "{:?}",
            a[0].gate_duplicate_fraction
        );
        let distinct = model(varied, varied);
        let b = analyse_gguf(&distinct).unwrap();
        assert!(
            b[0].gate_duplicate_fraction.unwrap() < 0.2,
            "{:?}",
            b[0].gate_duplicate_fraction
        );
    }

    #[test]
    fn a_planted_outlier_weight_is_seen() {
        let file = model(
            |_, r, n| {
                if r == 3 && n == 5 {
                    50.0
                } else {
                    0.01 * ((r + n) % 7) as f32
                }
            },
            varied,
        );
        let a = analyse_gguf(&file).unwrap();
        let plain = analyse_gguf(&model(|_, r, n| 0.01 * ((r + n) % 7) as f32, varied)).unwrap();
        assert!(a[0].down_outlier_fraction.unwrap() > 0.0);
        assert!(
            a[0].down_outlier_fraction.unwrap() > plain[0].down_outlier_fraction.unwrap_or(0.0)
        );
    }

    #[test]
    fn unsupported_or_broken_files_are_errors() {
        assert!(analyse_gguf(b"not a gguf").is_err());
        let mut w = GgufWriter::new();
        w.add_metadata("general.architecture", Value::String("mamba".into()));
        assert!(analyse_gguf(&w.finish().unwrap()).is_err());
        let mut w = GgufWriter::new();
        w.add_metadata("general.architecture", Value::String("llama".into()));
        w.add_metadata("llama.block_count", Value::U32(1));
        w.add_metadata("llama.embedding_length", Value::U32(32));
        w.add_metadata("llama.feed_forward_length", Value::U32(64));
        assert!(analyse_gguf(&w.finish().unwrap()).is_err());
    }

    #[test]
    fn weighting_demotes_a_neuron_that_fires_but_writes_nothing() {
        // Neuron 0 fires hardest but its output weights are zero; neuron 1 fires less and writes a lot.
        let weighted = weighted_importance(&[10.0, 4.0, 1.0], &[0.0, 3.0, 2.0]).unwrap();
        assert_eq!(weighted, vec![0.0, 12.0, 2.0]);
        let plain =
            make_plan_from_importance(&[vec![10.0, 4.0, 1.0, 0.5]], 0.5, Rule::Uniform).unwrap();
        let with_weights = make_plan_from_importance(
            &[weighted_importance(&[10.0, 4.0, 1.0, 0.5], &[0.0, 3.0, 2.0, 1.0]).unwrap()],
            0.5,
            Rule::Uniform,
        )
        .unwrap();
        assert_eq!(plain.keep, vec![vec![0, 1]]);
        assert_eq!(with_weights.keep, vec![vec![1, 2]]);
    }

    #[test]
    fn saliency_squares_both_factors() {
        // Neuron 0: mean square 4, norm 1 -> 4. Neuron 1: mean square 1, norm 3 -> 9.
        let s = saliency_importance(&[4.0, 1.0], &[1.0, 3.0]).unwrap();
        assert_eq!(s, vec![4.0, 9.0]);
        assert!(saliency_importance(&[], &[]).is_err());
        assert!(saliency_importance(&[1.0], &[1.0, 2.0]).is_err());
        assert!(saliency_importance(&[-1.0], &[1.0]).is_err());
        assert!(saliency_importance(&[f64::NAN], &[1.0]).is_err());
    }

    #[test]
    fn saliency_prefers_a_neuron_with_rare_large_activations_over_a_steady_one() {
        // Both neurons write with the same weight norm. Neuron 0 fires steadily at 1.0
        // (mean |a| 1.0, mean square 1.0). Neuron 1 is quiet on three tokens and fires at 3.0 on
        // one (mean |a| 0.75, mean square 2.25). The linear score ranks neuron 0 above neuron 1;
        // the squared score flips it, because a rare large write changes the output more than its
        // average suggests.
        let mut p = Profiler::new(1, 2).unwrap();
        for spiky in [0.0f32, 0.0, 3.0, 0.0] {
            p.observe(0, &[1.0, spiky]).unwrap();
        }
        let norms = [1.0, 1.0];
        let linear = weighted_importance(&p.importance(0).unwrap(), &norms).unwrap();
        let squared = saliency_importance(&p.mean_square(0).unwrap(), &norms).unwrap();
        assert!(linear[0] > linear[1], "linear: {linear:?}");
        assert!(squared[1] > squared[0], "squared: {squared:?}");
    }

    #[test]
    fn saliency_scores_follow_the_profile_and_the_weights() {
        let file = model(|_, _, n| (n + 1) as f32, varied);
        let analysis = analyse_gguf(&file).unwrap();
        let profiler = profiled();
        let scores = saliency_scores(&profiler, &analysis).unwrap();
        for layer_scores in &scores {
            // Dead neurons score zero; live ones score mean-square x norm squared.
            assert!(layer_scores[..D_FF / 2].iter().all(|v| *v == 0.0));
            let n = D_FF - 1;
            let a = (n + 1) as f64;
            let norm = (n + 1) as f64 * (D_MODEL as f64).sqrt();
            let expected = a * a * norm * norm;
            assert!((layer_scores[n] - expected).abs() < 1e-3 * expected);
        }
        assert!(saliency_scores(&profiler, &analysis[..1]).is_err());
    }

    #[test]
    fn weighting_refuses_bad_input() {
        assert!(weighted_importance(&[], &[]).is_err());
        assert!(weighted_importance(&[1.0], &[1.0, 2.0]).is_err());
        assert!(weighted_importance(&[f64::NAN], &[1.0]).is_err());
        assert!(weighted_importance(&[-1.0], &[1.0]).is_err());
        assert!(weighted_importance(&[1.0], &[-1.0]).is_err());
    }

    fn profiled() -> Profiler {
        let mut p = Profiler::new(2, D_FF).unwrap();
        for layer in 0..2 {
            // Half the neurons never fire.
            let a: Vec<f32> = (0..D_FF)
                .map(|n| if n < D_FF / 2 { 0.0 } else { (n + 1) as f32 })
                .collect();
            p.observe(layer, &a).unwrap();
        }
        p
    }

    #[test]
    fn reports_show_dead_neurons_and_the_kept_share() {
        let file = model(|_, _, n| (n + 1) as f32, varied);
        let analysis = analyse_gguf(&file).unwrap();
        let profiler = profiled();
        let scores = weighted_scores(&profiler, &analysis).unwrap();
        let all: Vec<usize> = (0..D_FF).collect();
        let keep_all = Plan {
            keep: vec![all.clone(), all],
        };
        let reports = layer_reports(&analysis, &profiler, &scores, &keep_all).unwrap();
        for r in &reports {
            assert!((r.dead_fraction - 0.5).abs() < 1e-12);
            assert!((r.retained - 1.0).abs() < 1e-12);
        }
        // Keep only dead neurons in layer 0: they carry no score, so nothing is retained.
        let dead_only = Plan {
            keep: vec![(0..32).collect(), (32..64).collect()],
        };
        let reports = layer_reports(&analysis, &profiler, &scores, &dead_only).unwrap();
        assert!(reports[0].retained < 1e-12);
        assert!((reports[1].retained - 1.0).abs() < 1e-12);
        let text = render_layers(&reports);
        assert!(text.contains("| 0 | 0.000 |"));
        assert!(text.contains("Layers keeping the least of their contribution: 0 (0.000)"));
        assert!(text.contains("do not show that the sliced model still works"));
    }

    #[test]
    fn mismatched_inputs_are_refused() {
        let file = model(varied, varied);
        let analysis = analyse_gguf(&file).unwrap();
        let profiler = profiled();
        let scores = weighted_scores(&profiler, &analysis).unwrap();
        let one_layer = Plan {
            keep: vec![vec![0]],
        };
        assert!(layer_reports(&analysis, &profiler, &scores, &one_layer).is_err());
        assert!(weighted_scores(&profiler, &analysis[..1]).is_err());
        let out_of_range = Plan {
            keep: vec![vec![999], vec![0]],
        };
        assert!(layer_reports(&analysis, &profiler, &scores, &out_of_range).is_err());
    }
}
