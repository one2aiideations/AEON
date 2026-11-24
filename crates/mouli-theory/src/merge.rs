//! Task-vector screening and model merging (TIES and linear).
//!
//! A task vector is `finetuned - base` for one tensor. TIES merging trims
//! small entries per model, elects the majority sign per entry, and averages
//! only the entries that agree with it.

/// Magnitude threshold keeping the top `density` fraction of `tau`.
///
/// `density >= 1` keeps everything (threshold 0).
#[must_use]
pub fn ties_threshold(tau: &[f32], density: f32) -> f32 {
    if tau.is_empty() || density >= 1.0 {
        return 0.0;
    }
    let mut mags: Vec<f32> = tau.iter().map(|x| x.abs()).collect();
    let drop = ((mags.len() as f32 * (1.0 - density.max(0.0))) as usize).max(1);
    if drop >= mags.len() {
        return mags.iter().copied().fold(0.0, f32::max);
    }
    let (_, kth, _) = mags.select_nth_unstable_by(drop - 1, f32::total_cmp);
    *kth
}

/// TIES-combine task vectors `taus` (equal length) with per-model thresholds.
#[must_use]
pub fn ties_combine(taus: &[&[f32]], thresholds: &[f32]) -> Vec<f32> {
    let Some(first) = taus.first() else {
        return Vec::new();
    };
    let n = first.len();
    (0..n)
        .map(|i| {
            let vals: Vec<f32> = taus
                .iter()
                .zip(thresholds)
                .map(|(t, &h)| {
                    let v = t.get(i).copied().unwrap_or(0.0);
                    if v.abs() >= h {
                        v
                    } else {
                        0.0
                    }
                })
                .collect();
            let sign = vals.iter().sum::<f32>().signum();
            let (sum, cnt) = vals
                .iter()
                .filter(|&&v| v != 0.0 && v.signum() == sign)
                .fold((0.0f32, 0u32), |(s, c), &v| (s + v, c + 1));
            if cnt == 0 {
                0.0
            } else {
                sum / cnt as f32
            }
        })
        .collect()
}

/// Merge method.
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    /// TIES with the given density.
    Ties {
        /// Fraction of entries kept per model.
        density: f32,
    },
    /// Weighted sum of task vectors.
    Linear {
        /// One weight per model.
        weights: Vec<f32>,
    },
}

/// Merge one tensor: `base + lambda * combine(finetuned - base)`.
///
/// Returns `None` when shapes differ or the weight count is wrong; callers
/// then keep the base tensor unchanged.
#[must_use]
pub fn merge_tensor(
    base: &[f32],
    finetuned: &[&[f32]],
    method: &Method,
    lambda: f32,
) -> Option<Vec<f32>> {
    if finetuned.is_empty() || finetuned.iter().any(|f| f.len() != base.len()) {
        return None;
    }
    let taus: Vec<Vec<f32>> = finetuned
        .iter()
        .map(|f| f.iter().zip(base).map(|(a, b)| a - b).collect())
        .collect();
    let refs: Vec<&[f32]> = taus.iter().map(Vec::as_slice).collect();
    let tau = match method {
        Method::Ties { density } => {
            let thr: Vec<f32> = refs.iter().map(|t| ties_threshold(t, *density)).collect();
            ties_combine(&refs, &thr)
        }
        Method::Linear { weights } => {
            if weights.len() != refs.len() {
                return None;
            }
            (0..base.len())
                .map(|i| refs.iter().zip(weights).map(|(t, w)| w * t[i]).sum())
                .collect()
        }
    };
    Some(base.iter().zip(tau).map(|(b, t)| b + lambda * t).collect())
}

/// Screening verdict for one candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Screen {
    /// Keep.
    Keep,
    /// Task vector is ~zero.
    IdenticalToBase,
    /// Task vector too large relative to the base.
    Diverged(f64),
    /// Near-duplicate of an earlier kept candidate (its index).
    Duplicate(usize),
}

/// Screen candidates given accumulated statistics over all tensors:
/// `gram[i][j] = <tau_i, tau_j>` and `base_sq = |base|^2`.
///
/// Candidates are visited in the given order (put preferred ones first).
#[must_use]
pub fn screen(gram: &[Vec<f64>], base_sq: f64, max_rel: f64, dup_cos: f64) -> Vec<Screen> {
    let n = gram.len();
    let norms: Vec<f64> = (0..n).map(|i| gram[i][i].max(0.0).sqrt()).collect();
    let bnorm = base_sq.max(0.0).sqrt();
    let mut kept: Vec<usize> = Vec::new();
    (0..n)
        .map(|i| {
            let rel = if bnorm > 0.0 { norms[i] / bnorm } else { 0.0 };
            if rel < 1e-5 {
                return Screen::IdenticalToBase;
            }
            if rel > max_rel {
                return Screen::Diverged(rel);
            }
            if let Some(&j) = kept
                .iter()
                .find(|&&j| gram[i][j] / (norms[i] * norms[j] + 1e-12) > dup_cos)
            {
                return Screen::Duplicate(j);
            }
            kept.push(i);
            Screen::Keep
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_keeps_top_fraction() {
        let t = [0.1, -0.4, 0.3, -0.2];
        assert!((ties_threshold(&t, 0.5) - 0.2).abs() < 1e-6);
        assert_eq!(ties_threshold(&t, 1.0), 0.0);
    }

    #[test]
    fn ties_resolves_sign_conflicts() {
        let a = [1.0f32, 1.0, -0.1];
        let b = [3.0f32, -2.0, -0.1];
        let c = [-1.0f32, -2.0, 0.0];
        let out = ties_combine(&[&a, &b, &c], &[0.0, 0.0, 0.0]);
        assert!((out[0] - 2.0).abs() < 1e-6); // + wins: mean of 1, 3
        assert!((out[1] + 2.0).abs() < 1e-6); // - wins: mean of -2, -2
        assert!((out[2] + 0.1).abs() < 1e-6);
    }

    #[test]
    fn merge_linear_and_ties() {
        let base = [1.0f32, 1.0];
        let f1 = [2.0f32, 1.0];
        let f2 = [1.0f32, 3.0];
        let lin = merge_tensor(
            &base,
            &[&f1, &f2],
            &Method::Linear {
                weights: vec![0.5, 0.5],
            },
            1.0,
        )
        .unwrap();
        assert_eq!(lin, vec![1.5, 2.0]);
        let ties = merge_tensor(&base, &[&f1, &f2], &Method::Ties { density: 1.0 }, 1.0).unwrap();
        assert_eq!(ties, vec![2.0, 3.0]);
        assert!(
            merge_tensor(&base, &[&[1.0f32][..]], &Method::Ties { density: 1.0 }, 1.0).is_none()
        );
    }

    #[test]
    fn screening() {
        // tau0 = (1,0), tau1 = (1,0) duplicate, tau2 = (0,0), tau3 = (10,0) diverged
        let gram = vec![
            vec![1.0, 1.0, 0.0, 10.0],
            vec![1.0, 1.0, 0.0, 10.0],
            vec![0.0, 0.0, 0.0, 0.0],
            vec![10.0, 10.0, 0.0, 100.0],
        ];
        let v = screen(&gram, 25.0, 1.0, 0.99);
        assert_eq!(v[0], Screen::Keep);
        assert_eq!(v[1], Screen::Duplicate(0));
        assert_eq!(v[2], Screen::IdenticalToBase);
        assert!(matches!(v[3], Screen::Diverged(r) if (r - 2.0).abs() < 1e-9));
    }
}
