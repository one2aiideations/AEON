//! Deterministic neuron selection.
//!
//! Uniform selection preserves equal hidden widths across layers.
//! Budget selection distributes a global neuron budget according to
//! per-layer-normalized activation importance.
//!
//! A plan is a selection decision, not evidence that the resulting model
//! retains its quality.

use crate::profile::Profiler;
use crate::SliceError;

/// Strategy for distributing a neuron-retention budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Keep the same number of neurons in every layer.
    Uniform,

    /// Distribute a global neuron budget according to normalized
    /// importance while retaining a minimum number in every layer.
    Budget,
}

/// A neuron selection plan.
///
/// Each inner vector contains strictly ascending neuron indices for
/// one layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Kept neuron indices, indexed by layer.
    pub keep: Vec<Vec<usize>>,
}

impl Plan {
    /// Returns the total number of retained neurons.
    #[must_use]
    pub fn kept_total(&self) -> usize {
        self.keep.iter().map(Vec::len).sum()
    }

    /// Returns the overall retained fraction for equal-width source layers.
    ///
    /// The denominator is `number_of_layers * d_ff`.
    /// A zero denominator returns zero.
    ///
    /// This does not report the retained fraction of any individual layer.
    #[must_use]
    pub fn fraction_kept(&self, d_ff: usize) -> f64 {
        let total = self.keep.len().checked_mul(d_ff);

        match total {
            Some(0) | None => 0.0,
            Some(total) => self.kept_total() as f64 / total as f64,
        }
    }
}

/// Creates a deterministic neuron selection plan.
///
/// The per-layer ranking is descending importance, with ties broken by
/// ascending neuron index.
///
/// `keep_fraction` must be finite and in `(0, 1]`.
///
/// Uniform selection retains `ceil(d_ff * keep_fraction)` neurons per layer.
///
/// Budget selection initially targets
/// `ceil(layers * d_ff * keep_fraction)` neurons. It imposes a minimum
/// per layer of `max(1, floor(d_ff * keep_fraction / 4))`.
///
/// If the global target is smaller than the sum of these mandatory
/// minimums, the effective target is raised to that minimum. This is
/// necessary to satisfy both the no-empty-layer requirement and the
/// minimum-layer requirement.
///
/// The remaining budget is allocated by descending normalized importance.
/// Each layer's importance is divided by its own mean importance. A layer
/// with zero mean receives zero normalized importance for all neurons.
/// Ties are resolved by layer index and then neuron index.
///
/// The result does not prove that the selected neurons are sufficient
/// for the domain.
///
/// # Errors
/// A [`SliceError`] for a bad fraction or non-finite statistics.
pub fn make_plan(profiler: &Profiler, keep_fraction: f64, rule: Rule) -> Result<Plan, SliceError> {
    let importance = (0..profiler.layers())
        .map(|layer| profiler.importance(layer))
        .collect::<Result<Vec<_>, _>>()?;
    make_plan_from_importance(&importance, keep_fraction, rule)
}

/// Like [`make_plan`], but from importance scores given directly: one vector per layer, one
/// value per neuron (all layers the same length). Use it to rank neurons by something other than
/// mean activation, for example activation times output weight norm.
///
/// Scores must be finite and non-negative. The rules, rounding and tie-breaking are those of
/// [`make_plan`]. A score is a ranking input, not evidence that a neuron is needed.
///
/// # Errors
/// A [`SliceError`] for no layers, layers of different lengths, a bad fraction, or a negative or
/// non-finite score.
pub fn make_plan_from_importance(
    importance: &[Vec<f64>],
    keep_fraction: f64,
    rule: Rule,
) -> Result<Plan, SliceError> {
    if !keep_fraction.is_finite() {
        return Err(SliceError::NonFinite("keep_fraction"));
    }

    if !(0.0 < keep_fraction && keep_fraction <= 1.0) {
        return Err(SliceError::BadParameter("keep_fraction"));
    }

    let layers = importance.len();
    let d_ff = importance.first().map_or(0, Vec::len);

    if layers == 0 || d_ff == 0 {
        return Err(SliceError::BadShape("empty importance"));
    }
    for layer in importance {
        if layer.len() != d_ff {
            return Err(SliceError::BadShape("layers differ in neuron count"));
        }
        if layer.iter().any(|v| !v.is_finite()) {
            return Err(SliceError::NonFinite("importance"));
        }
        if layer.iter().any(|v| *v < 0.0) {
            return Err(SliceError::BadParameter("importance must not be negative"));
        }
    }

    let per_layer = match rule {
        Rule::Uniform => {
            let count = ceil_product(d_ff, keep_fraction)?;
            let mut keep = Vec::with_capacity(layers);

            for layer_importance in importance {
                let mut ranked = rank_layer(layer_importance);
                ranked.truncate(count);
                ranked.sort_unstable();
                keep.push(ranked);
            }

            keep
        }
        Rule::Budget => make_budget_plan(importance, d_ff, keep_fraction)?,
    };

    Ok(Plan { keep: per_layer })
}

/// Tolerance for the rounding error of `value * fraction` before taking
/// the ceiling, so that 10 x 0.7 means 7 neurons and not 8.
const CEIL_SLACK: f64 = 1e-9;

fn ceil_product(value: usize, fraction: f64) -> Result<usize, SliceError> {
    let product = value as f64 * fraction;

    if !product.is_finite() {
        return Err(SliceError::NonFinite("retention target"));
    }

    // Take away the floating-point fuzz first; then never exceed `value`.
    let rounded = (product - CEIL_SLACK).ceil().max(1.0).min(value as f64);

    Ok(rounded as usize)
}

fn rank_layer(importance: &[f64]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..importance.len()).collect();

    indices.sort_by(|&left, &right| {
        importance[right]
            .total_cmp(&importance[left])
            .then_with(|| left.cmp(&right))
    });

    indices
}

fn make_budget_plan(
    all_importance: &[Vec<f64>],
    d_ff: usize,
    fraction: f64,
) -> Result<Vec<Vec<usize>>, SliceError> {
    let layers = all_importance.len();

    let total = layers
        .checked_mul(d_ff)
        .ok_or(SliceError::BadParameter("total neuron count overflow"))?;

    let requested = ceil_product(total, fraction)?;

    let minimum_value = (d_ff as f64 * fraction / 4.0 + CEIL_SLACK).floor();
    if !minimum_value.is_finite() || minimum_value > d_ff as f64 {
        return Err(SliceError::BadParameter("budget minimum"));
    }

    let minimum = (minimum_value as usize).max(1).min(d_ff);

    let minimum_total = layers
        .checked_mul(minimum)
        .ok_or(SliceError::BadParameter("minimum budget overflow"))?;

    let target = requested.max(minimum_total).min(total);

    let mut importance = Vec::with_capacity(layers);
    let mut means = Vec::with_capacity(layers);

    for values in all_importance {
        let values = values.clone();

        if values.iter().any(|value| !value.is_finite()) {
            return Err(SliceError::NonFinite("importance"));
        }

        let sum: f64 = values.iter().sum();
        if !sum.is_finite() {
            return Err(SliceError::NonFinite("importance sum"));
        }

        let mean = sum / d_ff as f64;
        means.push(mean);
        importance.push(values);
    }

    let mut keep = vec![Vec::new(); layers];
    let mut selected = vec![vec![false; d_ff]; layers];

    // First satisfy every layer's mandatory minimum.
    for layer in 0..layers {
        let mut ranked = rank_layer(&importance[layer]);
        ranked.truncate(minimum);
        ranked.sort_unstable();

        for neuron in ranked {
            selected[layer][neuron] = true;
            keep[layer].push(neuron);
        }
    }

    // Rank all remaining candidates globally.
    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();

    for layer in 0..layers {
        for neuron in 0..d_ff {
            if selected[layer][neuron] {
                continue;
            }

            let normalized = if means[layer] > 0.0 {
                importance[layer][neuron] / means[layer]
            } else {
                0.0
            };

            if !normalized.is_finite() {
                return Err(SliceError::NonFinite("normalized importance"));
            }

            candidates.push((normalized, layer, neuron));
        }
    }

    candidates.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let already_kept = minimum_total;
    let additional = target - already_kept;

    if additional > candidates.len() {
        return Err(SliceError::BadShape("budget cannot be distributed"));
    }

    for (_, layer, neuron) in candidates.into_iter().take(additional) {
        keep[layer].push(neuron);
    }

    for layer_keep in &mut keep {
        layer_keep.sort_unstable();
    }

    Ok(keep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActivationSink;

    fn profiled(layers: usize, d_ff: usize) -> Profiler {
        let mut profiler = Profiler::new(layers, d_ff).unwrap();

        for layer in 0..layers {
            let values: Vec<f32> = (0..d_ff)
                .map(|neuron| (neuron + 1 + layer * d_ff) as f32)
                .collect();

            profiler.observe(layer, &values).unwrap();
        }

        profiler
    }

    #[test]
    fn uniform_counts_are_equal() {
        let profiler = profiled(3, 8);
        let plan = make_plan(&profiler, 0.5, Rule::Uniform).unwrap();

        assert_eq!(plan.keep.len(), 3);
        assert_eq!(plan.keep[0].len(), 4);
        assert_eq!(plan.keep[1].len(), 4);
        assert_eq!(plan.keep[2].len(), 4);
    }

    #[test]
    fn fractions_that_are_inexact_in_floating_point_do_not_round_up() {
        // 100 * 0.55 is 55.00000000000001 in f64; the plan must keep 55, not 56.
        assert!(std::hint::black_box(100.0_f64) * 0.55 > 55.0);
        let profiler = profiled(2, 100);
        let plan = make_plan(&profiler, 0.55, Rule::Uniform).unwrap();
        assert_eq!(plan.keep[0].len(), 55);
        // 8960 neurons at 65% is 5824 exactly.
        let wide = profiled(1, 8960);
        let plan = make_plan(&wide, 0.65, Rule::Uniform).unwrap();
        assert_eq!(plan.keep[0].len(), 5824);
        // A real fractional part still rounds up.
        let plan = make_plan(&profiled(1, 10), 0.75, Rule::Uniform).unwrap();
        assert_eq!(plan.keep[0].len(), 8);
    }

    #[test]
    fn budget_has_exact_target() {
        let profiler = profiled(4, 8);
        let plan = make_plan(&profiler, 0.5, Rule::Budget).unwrap();

        assert_eq!(plan.kept_total(), 16);
    }

    #[test]
    fn budget_never_empties_a_layer() {
        let profiler = profiled(4, 8);
        let plan = make_plan(&profiler, 0.1, Rule::Budget).unwrap();

        assert!(plan.keep.iter().all(|layer| !layer.is_empty()));
    }

    #[test]
    fn ties_break_by_lowest_index() {
        let profiler = Profiler::new(1, 5).unwrap();
        let plan = make_plan(&profiler, 0.4, Rule::Uniform).unwrap();

        assert_eq!(plan.keep[0], vec![0, 1]);
    }

    #[test]
    fn fraction_one_keeps_everything() {
        let profiler = profiled(3, 7);

        for rule in [Rule::Uniform, Rule::Budget] {
            let plan = make_plan(&profiler, 1.0, rule).unwrap();

            assert_eq!(plan.kept_total(), 21);

            for layer in &plan.keep {
                assert_eq!(layer, &(0..7).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn invalid_fractions_are_rejected() {
        let profiler = profiled(2, 4);

        for fraction in [0.0, -0.1, 1.1, f64::NAN, f64::INFINITY] {
            assert!(make_plan(&profiler, fraction, Rule::Uniform).is_err());
        }
    }

    #[test]
    fn budget_distributes_by_normalized_importance() {
        let mut profiler = Profiler::new(2, 4).unwrap();

        profiler.observe(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        profiler.observe(1, &[10.0, 20.0, 30.0, 40.0]).unwrap();

        let plan = make_plan(&profiler, 0.5, Rule::Budget).unwrap();

        assert_eq!(plan.kept_total(), 4);
        assert_eq!(plan.keep[0].len(), 2);
        assert_eq!(plan.keep[1].len(), 2);
    }

    #[test]
    fn small_fraction_respects_mandatory_minimum() {
        let profiler = profiled(4, 2);
        let plan = make_plan(&profiler, 0.01, Rule::Budget).unwrap();

        assert_eq!(plan.kept_total(), 4);
        assert!(plan.keep.iter().all(|layer| !layer.is_empty()));
    }

    #[test]
    fn plans_from_given_importance_follow_the_scores() {
        let importance = vec![vec![0.1, 5.0, 0.2, 4.0], vec![9.0, 0.0, 8.0, 0.0]];
        let plan = make_plan_from_importance(&importance, 0.5, Rule::Uniform).unwrap();
        assert_eq!(plan.keep, vec![vec![1, 3], vec![0, 2]]);
        let budget = make_plan_from_importance(&importance, 0.5, Rule::Budget).unwrap();
        assert_eq!(budget.kept_total(), 4);
        // The profiler path and the direct path agree.
        let profiler = profiled(3, 8);
        let direct: Vec<Vec<f64>> = (0..3).map(|l| profiler.importance(l).unwrap()).collect();
        for rule in [Rule::Uniform, Rule::Budget] {
            assert_eq!(
                make_plan(&profiler, 0.5, rule).unwrap(),
                make_plan_from_importance(&direct, 0.5, rule).unwrap()
            );
        }
    }

    #[test]
    fn given_importance_is_validated() {
        assert!(make_plan_from_importance(&[], 0.5, Rule::Uniform).is_err());
        assert!(make_plan_from_importance(&[vec![]], 0.5, Rule::Uniform).is_err());
        let ragged = vec![vec![1.0, 2.0], vec![1.0]];
        assert!(make_plan_from_importance(&ragged, 0.5, Rule::Uniform).is_err());
        let bad = vec![vec![1.0, f64::NAN]];
        assert!(make_plan_from_importance(&bad, 0.5, Rule::Uniform).is_err());
        let negative = vec![vec![1.0, -1.0]];
        assert!(make_plan_from_importance(&negative, 0.5, Rule::Uniform).is_err());
        assert!(make_plan_from_importance(&[vec![1.0, 2.0]], 0.0, Rule::Uniform).is_err());
    }

    #[test]
    fn fraction_kept_is_overall_fraction() {
        let plan = Plan {
            keep: vec![vec![0, 1], vec![1, 2]],
        };

        assert_eq!(plan.kept_total(), 4);
        assert!((plan.fraction_kept(4) - 0.5).abs() < f64::EPSILON);
    }
}
