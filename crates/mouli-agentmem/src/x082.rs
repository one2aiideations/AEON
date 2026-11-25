//! Restore policy validation for audit-report replay checkpoints.
//!
//! This module validates a decoded M080 checkpoint against generation,
//! cardinality, sequence-ceiling, and live-watermark continuity policies.
//! It performs no I/O and never mutates the supplied live state.

use crate::x080::{
    encode, ReplayCheckpoint, ReplayCheckpointError, ReplayCheckpointLimits, ReplayPrincipal,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayCheckpointRestorePolicy {
    pub minimum_generation: u64,
    pub maximum_generation: Option<u64>,
    pub current_generation: Option<u64>,
    pub maximum_principals: usize,
    pub maximum_sequence: Option<u64>,
    pub preserve_existing_principals: bool,
    pub current_watermarks: BTreeMap<ReplayPrincipal, u64>,
}

impl Default for ReplayCheckpointRestorePolicy {
    fn default() -> Self {
        Self {
            minimum_generation: 1,
            maximum_generation: None,
            current_generation: None,
            maximum_principals: 16_384,
            maximum_sequence: None,
            preserve_existing_principals: true,
            current_watermarks: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayCheckpointRestoreReport {
    pub generation: u64,
    pub principal_count: usize,
    pub added_principals: usize,
    pub retained_principals: usize,
    pub removed_principals: usize,
    pub maximum_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestorePolicyError {
    InvalidPolicy,
    Checkpoint(ReplayCheckpointError),
    GenerationBelowMinimum {
        actual: u64,
        minimum: u64,
    },
    GenerationAboveMaximum {
        actual: u64,
        maximum: u64,
    },
    GenerationNotIncreasing {
        current: u64,
        proposed: u64,
    },
    TooManyPrincipals {
        actual: usize,
        maximum: usize,
    },
    SequenceAboveMaximum {
        principal: ReplayPrincipal,
        actual: u64,
        maximum: u64,
    },
    WatermarkRegression {
        principal: ReplayPrincipal,
        current: u64,
        proposed: Option<u64>,
    },
}

impl From<ReplayCheckpointError> for RestorePolicyError {
    fn from(value: ReplayCheckpointError) -> Self {
        Self::Checkpoint(value)
    }
}

/// Validates a checkpoint before installation. Existing principals may not
/// regress; by default they may not disappear either. A successful report is
/// evidence of policy validation only, not authentication of checkpoint bytes.
pub fn validate_restore(
    checkpoint: &ReplayCheckpoint,
    policy: &ReplayCheckpointRestorePolicy,
    codec_limits: &ReplayCheckpointLimits,
) -> Result<ReplayCheckpointRestoreReport, RestorePolicyError> {
    if policy.maximum_principals == 0
        || policy.minimum_generation == 0
        || policy
            .maximum_generation
            .is_some_and(|maximum| maximum < policy.minimum_generation)
        || policy
            .current_generation
            .is_some_and(|current| current == u64::MAX)
        || policy
            .current_watermarks
            .values()
            .any(|sequence| *sequence == 0)
    {
        return Err(RestorePolicyError::InvalidPolicy);
    }

    // Reuse the canonical codec validator so in-memory values obey the same
    // identity, sequence, generation, and encoded-size constraints as wire data.
    let _encoded = encode(checkpoint, codec_limits)?;

    if checkpoint.generation < policy.minimum_generation {
        return Err(RestorePolicyError::GenerationBelowMinimum {
            actual: checkpoint.generation,
            minimum: policy.minimum_generation,
        });
    }
    if let Some(maximum) = policy.maximum_generation {
        if checkpoint.generation > maximum {
            return Err(RestorePolicyError::GenerationAboveMaximum {
                actual: checkpoint.generation,
                maximum,
            });
        }
    }
    if let Some(current) = policy.current_generation {
        if checkpoint.generation <= current {
            return Err(RestorePolicyError::GenerationNotIncreasing {
                current,
                proposed: checkpoint.generation,
            });
        }
    }
    if checkpoint.watermarks.len() > policy.maximum_principals {
        return Err(RestorePolicyError::TooManyPrincipals {
            actual: checkpoint.watermarks.len(),
            maximum: policy.maximum_principals,
        });
    }

    for (principal, proposed) in &checkpoint.watermarks {
        if let Some(maximum) = policy.maximum_sequence {
            if *proposed > maximum {
                return Err(RestorePolicyError::SequenceAboveMaximum {
                    principal: principal.clone(),
                    actual: *proposed,
                    maximum,
                });
            }
        }
    }

    let mut retained = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    for (principal, current) in &policy.current_watermarks {
        match checkpoint.watermarks.get(principal).copied() {
            Some(proposed) if proposed >= *current => retained = retained.saturating_add(1),
            Some(proposed) => {
                return Err(RestorePolicyError::WatermarkRegression {
                    principal: principal.clone(),
                    current: *current,
                    proposed: Some(proposed),
                })
            }
            None if policy.preserve_existing_principals => {
                return Err(RestorePolicyError::WatermarkRegression {
                    principal: principal.clone(),
                    current: *current,
                    proposed: None,
                });
            }
            None => removed = removed.saturating_add(1),
        }
    }
    for principal in checkpoint.watermarks.keys() {
        if !policy.current_watermarks.contains_key(principal) {
            added = added.saturating_add(1);
        }
    }

    Ok(ReplayCheckpointRestoreReport {
        generation: checkpoint.generation,
        principal_count: checkpoint.watermarks.len(),
        added_principals: added,
        retained_principals: retained,
        removed_principals: removed,
        maximum_sequence: checkpoint.maximum_sequence(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(consumer: &str, key: &str) -> ReplayPrincipal {
        ReplayPrincipal {
            consumer_id: consumer.to_owned(),
            key_id: key.to_owned(),
        }
    }

    fn checkpoint(generation: u64, entries: &[(&str, &str, u64)]) -> ReplayCheckpoint {
        let watermarks = entries
            .iter()
            .map(|(consumer, key, sequence)| (principal(consumer, key), *sequence))
            .collect();
        ReplayCheckpoint::new(generation, watermarks)
    }

    #[test]
    fn accepts_monotonic_checkpoint_and_reports_delta() {
        let current = BTreeMap::from([(principal("consumer-a", "key-a"), 7)]);
        let policy = ReplayCheckpointRestorePolicy {
            current_generation: Some(3),
            current_watermarks: current,
            ..ReplayCheckpointRestorePolicy::default()
        };
        let report = validate_restore(
            &checkpoint(4, &[("consumer-a", "key-a", 8), ("consumer-b", "key-b", 2)]),
            &policy,
            &ReplayCheckpointLimits::default(),
        )
        .expect("valid checkpoint");
        assert_eq!(report.added_principals, 1);
        assert_eq!(report.retained_principals, 1);
        assert_eq!(report.removed_principals, 0);
        assert_eq!(report.maximum_sequence, 8);
    }

    #[test]
    fn rejects_generation_regression_and_watermark_regression() {
        let policy = ReplayCheckpointRestorePolicy {
            current_generation: Some(4),
            current_watermarks: BTreeMap::from([(principal("c", "k"), 9)]),
            ..ReplayCheckpointRestorePolicy::default()
        };
        assert!(matches!(
            validate_restore(
                &checkpoint(4, &[("c", "k", 10)]),
                &policy,
                &ReplayCheckpointLimits::default()
            ),
            Err(RestorePolicyError::GenerationNotIncreasing { .. })
        ));
        assert!(matches!(
            validate_restore(
                &checkpoint(5, &[("c", "k", 8)]),
                &policy,
                &ReplayCheckpointLimits::default()
            ),
            Err(RestorePolicyError::WatermarkRegression { .. })
        ));
    }

    #[test]
    fn rejects_removal_by_default_and_allows_explicit_pruning() {
        let current = BTreeMap::from([(principal("c", "k"), 9)]);
        let strict = ReplayCheckpointRestorePolicy {
            current_watermarks: current.clone(),
            ..ReplayCheckpointRestorePolicy::default()
        };
        let candidate = checkpoint(2, &[]);
        assert!(matches!(
            validate_restore(&candidate, &strict, &ReplayCheckpointLimits::default()),
            Err(RestorePolicyError::WatermarkRegression { proposed: None, .. })
        ));
        let pruning = ReplayCheckpointRestorePolicy {
            preserve_existing_principals: false,
            current_watermarks: current,
            ..ReplayCheckpointRestorePolicy::default()
        };
        let report = validate_restore(&candidate, &pruning, &ReplayCheckpointLimits::default())
            .expect("explicit pruning");
        assert_eq!(report.removed_principals, 1);
    }

    #[test]
    fn enforces_principal_and_sequence_ceilings() {
        let policy = ReplayCheckpointRestorePolicy {
            maximum_principals: 1,
            maximum_sequence: Some(10),
            ..ReplayCheckpointRestorePolicy::default()
        };
        assert!(matches!(
            validate_restore(
                &checkpoint(2, &[("a", "k", 1), ("b", "k", 2)]),
                &policy,
                &ReplayCheckpointLimits::default()
            ),
            Err(RestorePolicyError::TooManyPrincipals { .. })
        ));
        let sequence_policy = ReplayCheckpointRestorePolicy {
            maximum_sequence: Some(5),
            ..ReplayCheckpointRestorePolicy::default()
        };
        assert!(matches!(
            validate_restore(
                &checkpoint(2, &[("a", "k", 6)]),
                &sequence_policy,
                &ReplayCheckpointLimits::default()
            ),
            Err(RestorePolicyError::SequenceAboveMaximum { .. })
        ));
    }
}
