//! Paired statistics for comparing two methods over the same seeds.
//!
//! Given one difference per seed (A minus B), this reports the mean, a seeded bootstrap interval
//! for the mean, an exact sign-flip (paired randomisation) p-value, and how many seeds A won, lost
//! or tied. None of these assume a bell-shaped distribution. What they cannot do: with few seeds
//! the smallest possible p-value is large (n seeds cannot give p below 2/2^n), and an interval
//! that excludes zero over seeds says nothing about other data sets. The functions say so by
//! returning the numbers, not a verdict; [`Paired::describe`] words the result cautiously.

use crate::rng::Rng;
use crate::F;

/// Most seeds for which every sign pattern is enumerated (2^20 is about a million).
pub const EXACT_MAX_N: usize = 20;
/// Resamples for the bootstrap interval, and random sign patterns above [`EXACT_MAX_N`].
pub const RESAMPLES: usize = 4000;

/// The result of comparing paired differences.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    /// Number of pairs used (non-finite differences are left out and counted in `skipped`).
    pub n: usize,
    /// Pairs left out because the difference was NaN or infinite.
    pub skipped: usize,
    /// Mean of the differences.
    pub mean: F,
    /// Sample standard deviation of the differences (0 for fewer than two pairs).
    pub std: F,
    /// 95% bootstrap interval for the mean (percentile method), `None` for fewer than two pairs.
    pub ci95: Option<(F, F)>,
    /// Two-sided sign-flip p-value for "the mean difference is zero", `None` when n is 0.
    pub p_value: Option<F>,
    /// Whether `p_value` was computed by enumerating every sign pattern.
    pub exact: bool,
    /// Pairs where A was better (difference above 0).
    pub wins: usize,
    /// Pairs where A was worse.
    pub losses: usize,
    /// Pairs with a difference of exactly 0.
    pub ties: usize,
}

/// Mean and sample standard deviation.
#[must_use]
pub fn mean_std(v: &[F]) -> (F, F) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let m = v.iter().sum::<F>() / v.len() as F;
    if v.len() < 2 {
        return (m, 0.0);
    }
    let var = v.iter().map(|x| (x - m) * (x - m)).sum::<F>() / (v.len() - 1) as F;
    (m, var.sqrt())
}

fn percentile(sorted: &[F], q: F) -> F {
    let pos = q * (sorted.len() - 1) as F;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as F)
}

/// Compares paired differences. Deterministic: the bootstrap and the large-n randomisation use
/// `seed`.
#[must_use]
pub fn paired(diffs: &[F], seed: u64) -> Paired {
    let d: Vec<F> = diffs.iter().copied().filter(|x| x.is_finite()).collect();
    let skipped = diffs.len() - d.len();
    let n = d.len();
    let (mean, std) = mean_std(&d);
    let wins = d.iter().filter(|&&x| x > 0.0).count();
    let losses = d.iter().filter(|&&x| x < 0.0).count();
    let ties = n - wins - losses;
    let mut rng = Rng::new(seed);

    let ci95 = (n >= 2).then(|| {
        let mut means: Vec<F> = (0..RESAMPLES)
            .map(|_| (0..n).map(|_| d[rng.below(n)]).sum::<F>() / n as F)
            .collect();
        means.sort_by(F::total_cmp);
        (percentile(&means, 0.025), percentile(&means, 0.975))
    });

    let (p_value, exact) = if n == 0 {
        (None, false)
    } else {
        // Under "no difference" each sign is equally likely: count sign patterns whose |sum| is
        // at least the observed |sum| (a small tolerance keeps ties from rounding noise).
        let observed = d.iter().sum::<F>().abs();
        let tol = 1e-12 * (1.0 + observed);
        if n <= EXACT_MAX_N {
            let mut extreme = 0u64;
            for mask in 0u64..(1u64 << n) {
                let s: F = d
                    .iter()
                    .enumerate()
                    .map(|(i, x)| if mask >> i & 1 == 1 { -x } else { *x })
                    .sum();
                if s.abs() >= observed - tol {
                    extreme += 1;
                }
            }
            (Some(extreme as F / (1u64 << n) as F), true)
        } else {
            let mut extreme = 1u64; // the observed pattern counts, so p is never 0
            for _ in 0..RESAMPLES {
                let s: F = d
                    .iter()
                    .map(|x| if rng.next_u64() & 1 == 1 { -x } else { *x })
                    .sum();
                if s.abs() >= observed - tol {
                    extreme += 1;
                }
            }
            (Some(extreme as F / (RESAMPLES + 1) as F), false)
        }
    };
    Paired {
        n,
        skipped,
        mean,
        std,
        ci95,
        p_value,
        exact,
        wins,
        losses,
        ties,
    }
}

impl Paired {
    /// One cautious sentence about what the numbers allow. `higher_is_better` says whether a
    /// positive difference favours A.
    #[must_use]
    pub fn describe(&self, a: &str, b: &str, higher_is_better: bool) -> String {
        if self.n == 0 {
            return format!("No paired seeds for {a} and {b}: nothing to compare.");
        }
        if self.n < 5 {
            return format!(
                "Only {} paired seed(s): too few to say whether {a} differs from {b} (mean difference {:+.4}).",
                self.n, self.mean
            );
        }
        let (lo, hi) = self.ci95.unwrap_or((self.mean, self.mean));
        let sign = if higher_is_better { 1.0 } else { -1.0 };
        let excludes_zero = lo > 0.0 || hi < 0.0;
        if !excludes_zero {
            return format!(
                "No clear difference between {a} and {b}: the interval ({lo:+.4}, {hi:+.4}) includes 0."
            );
        }
        let a_better = (self.mean * sign) > 0.0;
        let (better, worse) = if a_better { (a, b) } else { (b, a) };
        format!(
            "{better} was better than {worse} on these seeds (interval {lo:+.4} to {hi:+.4}, {} of {} seeds); this says nothing about other data.",
            if a_better { self.wins } else { self.losses },
            self.n
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exact_p_value_matches_a_hand_count() {
        // Differences +1, +2: patterns ++ (3), +- (-1), -+ (1), -- (-3); |sum| >= 3 in 2 of 4.
        let p = paired(&[1.0, 2.0], 1);
        assert_eq!((p.n, p.wins, p.losses, p.ties), (2, 2, 0, 0));
        assert_eq!(p.p_value, Some(0.5));
        assert!(p.exact);
        // Three equal positive differences: only all-plus and all-minus reach |sum| = 3 -> 2/8.
        assert_eq!(paired(&[1.0, 1.0, 1.0], 1).p_value, Some(0.25));
        // Zero mean: every pattern is at least as extreme as 0 -> p = 1.
        assert_eq!(paired(&[1.0, -1.0], 1).p_value, Some(1.0));
    }

    #[test]
    fn a_clear_effect_gives_a_small_p_and_an_interval_away_from_zero_and_noise_does_not() {
        let strong: Vec<F> = (0..12).map(|i| 0.05 + 0.001 * i as F).collect();
        let p = paired(&strong, 7);
        assert!(p.p_value.unwrap() < 0.001, "{p:?}");
        let (lo, hi) = p.ci95.unwrap();
        assert!(lo > 0.0 && hi > lo);
        assert!(
            p.describe("A", "B", true).starts_with("A was better"),
            "{}",
            p.describe("A", "B", true)
        );
        // Lower-is-better flips who is named.
        assert!(p.describe("A", "B", false).starts_with("B was better"));
        // Differences that scatter around zero: no clear difference.
        let noise = [
            0.02, -0.03, 0.01, -0.02, 0.03, -0.01, 0.0, 0.015, -0.025, 0.02,
        ];
        let q = paired(&noise, 7);
        assert!(q.p_value.unwrap() > 0.1, "{q:?}");
        assert!(q
            .describe("A", "B", true)
            .starts_with("No clear difference"));
        assert_eq!(q.ties, 1);
    }

    #[test]
    fn few_seeds_are_not_over_read_and_nothing_is_not_a_result() {
        let p = paired(&[0.1, 0.1, 0.1], 1);
        assert!(
            p.describe("A", "B", true).contains("too few"),
            "{}",
            p.describe("A", "B", true)
        );
        assert!(
            p.p_value.unwrap() >= 0.25,
            "3 seeds cannot reach p below 0.25"
        );
        let e = paired(&[], 1);
        assert_eq!((e.n, e.p_value, e.ci95), (0, None, None));
        assert!(e.describe("A", "B", true).contains("nothing to compare"));
        let one = paired(&[0.3], 1);
        assert_eq!((one.std, one.ci95), (0.0, None));
    }

    #[test]
    fn non_finite_differences_are_left_out_and_counted() {
        let p = paired(&[0.1, F::NAN, 0.2, F::INFINITY, 0.15, 0.12, 0.11, 0.13], 3);
        assert_eq!((p.n, p.skipped), (6, 2));
        assert!(p.mean.is_finite() && p.ci95.unwrap().0.is_finite());
    }

    #[test]
    fn results_are_repeatable_and_large_n_uses_a_seeded_randomisation() {
        let d: Vec<F> = (0..30).map(|i| ((i * 7 % 11) as F - 4.0) * 0.01).collect();
        let a = paired(&d, 9);
        let b = paired(&d, 9);
        assert_eq!(a, b);
        assert!(!a.exact);
        assert!(a.p_value.unwrap() > 0.0, "never exactly 0");
        assert_ne!(
            paired(&d, 10).ci95,
            a.ci95,
            "another seed resamples differently"
        );
    }
}
