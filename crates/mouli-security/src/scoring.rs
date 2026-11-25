//! Risk scoring: inherent score from the factors, residual score after controls, and a level.

use std::fmt;

/// Highest value of each 1..=5 scale.
const SCALE_MAX: f64 = 5.0;
/// Most the residual can be reduced by controls (a control can never make risk vanish).
const MAX_CONTROL_EFFECT: f64 = 0.9;
/// Points added at full compliance exposure.
const COMPLIANCE_PENALTY: f64 = 10.0;

/// Relative importance of each factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    /// Weight of likelihood.
    pub likelihood: f64,
    /// Weight of impact.
    pub impact: f64,
    /// Weight of velocity (how fast the risk is building).
    pub velocity: f64,
    /// Weight of exposure (how much is at stake).
    pub exposure: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            likelihood: 0.30,
            impact: 0.30,
            velocity: 0.15,
            exposure: 0.10,
        }
    }
}

/// Factor ratings on a 0..=5 scale (likelihood and impact are 1..=5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Factors {
    /// Chance the risk occurs, 1 (rare) to 5 (almost certain).
    pub likelihood: u8,
    /// Damage if it occurs, 1 (minor) to 5 (severe).
    pub impact: u8,
    /// How fast it is building, 0 (not at all) to 5.
    pub velocity: u8,
    /// How much is exposed, 0 to 5.
    pub exposure: u8,
}

/// Why scoring refused its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScoreError {
    /// A rating is outside its scale.
    OutOfRange(&'static str),
    /// Weights are negative, non-finite or all zero.
    BadWeights,
    /// Level thresholds are not strictly increasing within 0..=100.
    BadAppetite,
}

impl fmt::Display for ScoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScoreError::OutOfRange(n) => write!(f, "{n} is outside its scale"),
            ScoreError::BadWeights => {
                write!(f, "weights must be finite, non-negative and not all zero")
            }
            ScoreError::BadAppetite => {
                write!(f, "level thresholds must increase strictly within 0..=100")
            }
        }
    }
}

impl std::error::Error for ScoreError {}

/// A risk level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Below the medium threshold.
    Low,
    /// At or above medium.
    Medium,
    /// At or above high.
    High,
    /// At or above critical.
    Critical,
}

/// Score thresholds that define the organisation's risk appetite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Appetite {
    /// Score from which a risk is Medium.
    pub medium: f64,
    /// Score from which a risk is High.
    pub high: f64,
    /// Score from which a risk is Critical.
    pub critical: f64,
}

impl Default for Appetite {
    fn default() -> Self {
        Self {
            medium: 40.0,
            high: 70.0,
            critical: 85.0,
        }
    }
}

impl Appetite {
    /// Checks the thresholds are strictly increasing and within 0..=100.
    pub fn validate(&self) -> Result<(), ScoreError> {
        let ok = self.medium.is_finite()
            && self.high.is_finite()
            && self.critical.is_finite()
            && self.medium >= 0.0
            && self.medium < self.high
            && self.high < self.critical
            && self.critical <= 100.0;
        if ok {
            Ok(())
        } else {
            Err(ScoreError::BadAppetite)
        }
    }

    /// The level of `score` under these thresholds.
    pub fn level(&self, score: f64) -> Level {
        if score >= self.critical {
            Level::Critical
        } else if score >= self.high {
            Level::High
        } else if score >= self.medium {
            Level::Medium
        } else {
            Level::Low
        }
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// The weighted average of the factors, 0..=100. Each factor is first scaled to 0..=1, so a
/// maximal rating on every factor scores exactly 100.
pub fn inherent(f: Factors, w: Weights) -> Result<f64, ScoreError> {
    if !(1..=5).contains(&f.likelihood) {
        return Err(ScoreError::OutOfRange("likelihood"));
    }
    if !(1..=5).contains(&f.impact) {
        return Err(ScoreError::OutOfRange("impact"));
    }
    if f.velocity > 5 {
        return Err(ScoreError::OutOfRange("velocity"));
    }
    if f.exposure > 5 {
        return Err(ScoreError::OutOfRange("exposure"));
    }
    let ws = [w.likelihood, w.impact, w.velocity, w.exposure];
    let total: f64 = ws.iter().sum();
    if ws.iter().any(|x| !x.is_finite() || *x < 0.0) || total <= 0.0 {
        return Err(ScoreError::BadWeights);
    }
    let weighted = w.likelihood * f64::from(f.likelihood)
        + w.impact * f64::from(f.impact)
        + w.velocity * f64::from(f.velocity)
        + w.exposure * f64::from(f.exposure);
    Ok(round2(
        (weighted / (total * SCALE_MAX) * 100.0).clamp(0.0, 100.0),
    ))
}

/// The score left after controls. `control_effectiveness` and `compliance_exposure` are
/// fractions in 0..=1 (values outside are clamped). Controls cut at most 90% of the inherent
/// score; compliance exposure adds up to 10 points.
pub fn residual(inherent: f64, control_effectiveness: f64, compliance_exposure: f64) -> f64 {
    if !inherent.is_finite() {
        return 0.0;
    }
    let ce = if control_effectiveness.is_finite() {
        control_effectiveness.clamp(0.0, MAX_CONTROL_EFFECT)
    } else {
        0.0
    };
    let co = if compliance_exposure.is_finite() {
        compliance_exposure.clamp(0.0, 1.0)
    } else {
        0.0
    };
    round2((inherent.clamp(0.0, 100.0) * (1.0 - ce) + co * COMPLIANCE_PENALTY).clamp(0.0, 100.0))
}

/// A full assessment.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    /// Score before controls.
    pub inherent: f64,
    /// Score after controls and compliance exposure.
    pub residual: f64,
    /// Level of the residual score.
    pub level: Level,
    /// Human-readable summary.
    pub explanation: String,
}

/// Scores a risk end to end.
pub fn assess(
    f: Factors,
    w: Weights,
    control_effectiveness: f64,
    compliance_exposure: f64,
    appetite: Appetite,
) -> Result<Assessment, ScoreError> {
    appetite.validate()?;
    let inh = inherent(f, w)?;
    let res = residual(inh, control_effectiveness, compliance_exposure);
    let level = appetite.level(res);
    Ok(Assessment {
        inherent: inh,
        residual: res,
        level,
        explanation: format!(
            "inherent {inh}, residual {res} (controls {control_effectiveness}, compliance {compliance_exposure}) => {level:?}"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(l: u8, i: u8, v: u8, e: u8) -> Factors {
        Factors {
            likelihood: l,
            impact: i,
            velocity: v,
            exposure: e,
        }
    }

    #[test]
    fn maximal_and_minimal_ratings_hit_the_ends() {
        assert_eq!(inherent(f(5, 5, 5, 5), Weights::default()).unwrap(), 100.0);
        // Minimum allowed: likelihood 1, impact 1, others 0: (0.3 + 0.3) / (0.85 * 5) * 100 = 14.12.
        assert_eq!(inherent(f(1, 1, 0, 0), Weights::default()).unwrap(), 14.12);
    }

    #[test]
    fn exact_mid_value() {
        // (0.3*3 + 0.3*4 + 0.15*2 + 0.1*1) / 4.25 * 100 = 2.5 / 4.25 * 100 = 58.82.
        assert_eq!(inherent(f(3, 4, 2, 1), Weights::default()).unwrap(), 58.82);
    }

    #[test]
    fn raising_any_factor_never_lowers_the_score() {
        let base = inherent(f(2, 2, 2, 2), Weights::default()).unwrap();
        for g in [f(3, 2, 2, 2), f(2, 3, 2, 2), f(2, 2, 3, 2), f(2, 2, 2, 3)] {
            assert!(inherent(g, Weights::default()).unwrap() > base);
        }
    }

    #[test]
    fn out_of_range_ratings_are_refused() {
        assert_eq!(
            inherent(f(0, 3, 0, 0), Weights::default()),
            Err(ScoreError::OutOfRange("likelihood"))
        );
        assert_eq!(
            inherent(f(6, 3, 0, 0), Weights::default()),
            Err(ScoreError::OutOfRange("likelihood"))
        );
        assert_eq!(
            inherent(f(3, 0, 0, 0), Weights::default()),
            Err(ScoreError::OutOfRange("impact"))
        );
        assert_eq!(
            inherent(f(3, 3, 6, 0), Weights::default()),
            Err(ScoreError::OutOfRange("velocity"))
        );
        assert_eq!(
            inherent(f(3, 3, 0, 6), Weights::default()),
            Err(ScoreError::OutOfRange("exposure"))
        );
    }

    #[test]
    fn bad_weights_are_refused() {
        let zero = Weights {
            likelihood: 0.0,
            impact: 0.0,
            velocity: 0.0,
            exposure: 0.0,
        };
        let neg = Weights {
            likelihood: -1.0,
            ..Weights::default()
        };
        let nan = Weights {
            impact: f64::NAN,
            ..Weights::default()
        };
        for w in [zero, neg, nan] {
            assert_eq!(inherent(f(3, 3, 3, 3), w), Err(ScoreError::BadWeights));
        }
    }

    #[test]
    fn residual_formula_and_clamps() {
        assert_eq!(residual(80.0, 0.5, 0.0), 40.0);
        assert_eq!(residual(80.0, 0.5, 0.5), 45.0);
        // Controls cannot remove more than 90%.
        assert_eq!(residual(100.0, 1.0, 0.0), 10.0);
        assert_eq!(residual(100.0, 5.0, 0.0), 10.0);
        assert_eq!(residual(100.0, 0.0, 1.0), 100.0);
        assert_eq!(residual(50.0, -3.0, -3.0), 50.0);
        assert_eq!(residual(f64::NAN, 0.5, 0.5), 0.0);
        assert_eq!(residual(50.0, f64::NAN, f64::NAN), 50.0);
    }

    #[test]
    fn levels_at_the_boundaries() {
        let a = Appetite::default();
        assert_eq!(a.level(39.99), Level::Low);
        assert_eq!(a.level(40.0), Level::Medium);
        assert_eq!(a.level(69.99), Level::Medium);
        assert_eq!(a.level(70.0), Level::High);
        assert_eq!(a.level(85.0), Level::Critical);
        assert_eq!(a.level(100.0), Level::Critical);
        assert!(
            Level::Critical > Level::High
                && Level::High > Level::Medium
                && Level::Medium > Level::Low
        );
    }

    #[test]
    fn custom_appetite_changes_levels_and_is_validated() {
        let cautious = Appetite {
            medium: 20.0,
            high: 40.0,
            critical: 60.0,
        };
        assert_eq!(cautious.level(45.0), Level::High);
        assert!(cautious.validate().is_ok());
        for bad in [
            Appetite {
                medium: 50.0,
                high: 40.0,
                critical: 60.0,
            },
            Appetite {
                medium: 10.0,
                high: 10.0,
                critical: 60.0,
            },
            Appetite {
                medium: -1.0,
                high: 40.0,
                critical: 60.0,
            },
            Appetite {
                medium: 10.0,
                high: 40.0,
                critical: 101.0,
            },
            Appetite {
                medium: f64::NAN,
                high: 40.0,
                critical: 60.0,
            },
        ] {
            assert_eq!(bad.validate(), Err(ScoreError::BadAppetite));
            assert!(assess(f(3, 3, 3, 3), Weights::default(), 0.0, 0.0, bad).is_err());
        }
    }

    #[test]
    fn assessment_end_to_end() {
        let a = assess(
            f(5, 5, 5, 5),
            Weights::default(),
            0.8,
            0.0,
            Appetite::default(),
        )
        .unwrap();
        assert_eq!((a.inherent, a.residual, a.level), (100.0, 20.0, Level::Low));
        let b = assess(
            f(5, 5, 5, 5),
            Weights::default(),
            0.0,
            0.0,
            Appetite::default(),
        )
        .unwrap();
        assert_eq!(b.level, Level::Critical);
        assert!(b.explanation.contains("Critical") && b.explanation.contains("inherent 100"));
    }
}
