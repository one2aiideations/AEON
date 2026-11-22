//! Gated self-repair and improvement.
//!
//! When something fails, a generator proposes candidate fixes. Each
//! candidate runs in a validator (sandboxed tests, benchmarks). A
//! candidate is accepted only if it passes every check and scores at least
//! the current baseline; otherwise the current version stays. Accepted
//! versions are kept so any change can be rolled back.

/// Proposes candidate versions.
pub trait Generator {
    /// Up to `n` candidates fixing `failure` in `current`.
    fn propose(&mut self, current: &str, failure: &str, n: usize) -> Vec<String>;
}

/// Validation of one candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Validation {
    /// All required checks passed.
    pub passed: bool,
    /// Quality score (higher is better), e.g. a robust score from `aeon-theory`.
    pub score: f64,
    /// Notes (failing checks, metrics).
    pub notes: String,
}

/// Runs checks on a candidate (in a sandbox).
pub trait Validator {
    /// Validate `candidate`.
    fn validate(&mut self, candidate: &str) -> Validation;
}

/// What one repair round did.
#[derive(Debug, Clone, PartialEq)]
pub enum RoundResult {
    /// A candidate was accepted.
    Accepted {
        /// Its score.
        score: f64,
        /// Candidates tried.
        tried: usize,
    },
    /// No candidate qualified; the current version stays.
    Kept {
        /// Candidates tried.
        tried: usize,
        /// Best rejected candidate's notes.
        best_notes: String,
    },
}

/// Versioned artefact under repair (a skill, prompt, config or code unit).
#[derive(Debug, Clone)]
pub struct Evolver {
    history: Vec<(String, f64)>,
    /// Candidates requested per round.
    pub candidates_per_round: usize,
    /// Minimum improvement over baseline to accept (0 = no regression).
    pub min_gain: f64,
}

impl Evolver {
    /// Start from `initial` with its measured `score`.
    #[must_use]
    pub fn new(initial: &str, score: f64) -> Self {
        Self {
            history: vec![(initial.to_owned(), score)],
            candidates_per_round: 3,
            min_gain: 0.0,
        }
    }

    /// Current version and score.
    #[must_use]
    pub fn current(&self) -> (&str, f64) {
        let (v, s) = self.history.last().map_or(("", 0.0), |(v, s)| (v.as_str(), *s));
        (v, s)
    }

    /// Number of accepted versions (including the initial one).
    #[must_use]
    pub fn versions(&self) -> usize {
        self.history.len()
    }

    /// One repair round for `failure`.
    pub fn round(&mut self, failure: &str, gen: &mut dyn Generator, val: &mut dyn Validator) -> RoundResult {
        let (current, baseline) = {
            let (c, s) = self.current();
            (c.to_owned(), s)
        };
        let candidates = gen.propose(&current, failure, self.candidates_per_round);
        let tried = candidates.len();
        let mut best: Option<(String, Validation)> = None;
        let mut best_rejected = String::new();
        for c in candidates {
            if c == current {
                continue;
            }
            let v = val.validate(&c);
            if v.passed && v.score >= baseline + self.min_gain {
                if best.as_ref().is_none_or(|(_, b)| v.score > b.score) {
                    best = Some((c, v));
                }
            } else if best_rejected.is_empty() {
                best_rejected = v.notes;
            }
        }
        match best {
            Some((c, v)) => {
                self.history.push((c, v.score));
                RoundResult::Accepted { score: v.score, tried }
            }
            None => RoundResult::Kept {
                tried,
                best_notes: best_rejected,
            },
        }
    }

    /// Undo the last accepted change. Returns false at the initial version.
    pub fn rollback(&mut self) -> bool {
        if self.history.len() > 1 {
            self.history.pop();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proposes appending digits; the validator likes larger numbers but
    /// rejects anything containing '9'.
    struct Digits;
    impl Generator for Digits {
        fn propose(&mut self, current: &str, _failure: &str, n: usize) -> Vec<String> {
            (0..n).map(|i| format!("{current}{}", [9, 3, 1][i % 3])).collect()
        }
    }
    struct NoNines;
    impl Validator for NoNines {
        fn validate(&mut self, c: &str) -> Validation {
            Validation {
                passed: !c.contains('9'),
                score: c.parse::<f64>().unwrap_or(0.0),
                notes: if c.contains('9') { "contains 9".into() } else { String::new() },
            }
        }
    }

    #[test]
    fn accepts_best_valid_and_rolls_back() {
        let mut e = Evolver::new("1", 1.0);
        let r = e.round("too small", &mut Digits, &mut NoNines);
        assert_eq!(r, RoundResult::Accepted { score: 13.0, tried: 3 });
        assert_eq!(e.current().0, "13");
        assert!(e.rollback());
        assert_eq!(e.current(), ("1", 1.0));
        assert!(!e.rollback());
    }

    #[test]
    fn keeps_current_when_nothing_qualifies() {
        struct Worse;
        impl Generator for Worse {
            fn propose(&mut self, _c: &str, _f: &str, _n: usize) -> Vec<String> {
                vec!["9".into(), "0".into()]
            }
        }
        let mut e = Evolver::new("5", 5.0);
        let r = e.round("x", &mut Worse, &mut NoNines);
        assert_eq!(
            r,
            RoundResult::Kept {
                tried: 2,
                best_notes: "contains 9".into()
            }
        );
        assert_eq!(e.versions(), 1);
    }
}
