//! Self-knowledge: how well a model's confidence matches its correctness.

/// Calibration metrics over scored items.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// Items scored.
    pub items: usize,
    /// Fraction correct.
    pub accuracy: f64,
    /// Mean confidence.
    pub mean_confidence: f64,
    /// Expected calibration error over equal-width bins.
    pub ece: f64,
    /// Brier score `E[(P - V)^2]`.
    pub brier: f64,
}

impl Calibration {
    /// Exact expected cost of acting on this confidence: `E[(P - V)^2] / 2`.
    #[must_use]
    pub fn self_knowledge_loss(&self) -> f64 {
        self.brier / 2.0
    }
}

/// Compute calibration from `(confidence, correct)` pairs using `bins` bins.
///
/// Bin `b` holds confidences in `(b/bins, (b+1)/bins]`; a confidence of 0
/// goes to the first bin. Returns `None` for no items or zero bins.
#[must_use]
pub fn calibration(records: &[(f64, bool)], bins: usize) -> Option<Calibration> {
    if records.is_empty() || bins == 0 {
        return None;
    }
    let n = records.len() as f64;
    let mut sum_p = vec![0.0f64; bins];
    let mut sum_v = vec![0.0f64; bins];
    let mut count = vec![0usize; bins];
    for &(p, v) in records {
        let p = p.clamp(0.0, 1.0);
        let b = ((p * bins as f64).ceil() as usize)
            .saturating_sub(1)
            .min(bins - 1);
        sum_p[b] += p;
        sum_v[b] += f64::from(u8::from(v));
        count[b] += 1;
    }
    let ece = (0..bins)
        .filter(|&b| count[b] > 0)
        .map(|b| {
            let c = count[b] as f64;
            c / n * (sum_p[b] / c - sum_v[b] / c).abs()
        })
        .sum();
    let brier = records
        .iter()
        .map(|&(p, v)| (p - f64::from(u8::from(v))).powi(2))
        .sum::<f64>()
        / n;
    Some(Calibration {
        items: records.len(),
        accuracy: records.iter().filter(|r| r.1).count() as f64 / n,
        mean_confidence: records.iter().map(|r| r.0).sum::<f64>() / n,
        ece,
        brier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_and_overconfident() {
        let perfect = [(1.0, true), (0.0, false)];
        let c = calibration(&perfect, 10).unwrap();
        assert!(c.ece.abs() < 1e-12 && c.brier.abs() < 1e-12);

        let over = [(0.9, false), (0.9, true)];
        let c = calibration(&over, 10).unwrap();
        assert!((c.ece - 0.4).abs() < 1e-12);
        assert!((c.brier - (0.81 + 0.01) / 2.0).abs() < 1e-12);
        assert!((c.self_knowledge_loss() - c.brier / 2.0).abs() < 1e-12);
        assert!(calibration(&[], 10).is_none());
    }
}
