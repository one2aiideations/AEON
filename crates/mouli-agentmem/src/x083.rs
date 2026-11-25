//! Policy-checked, atomic in-memory restoration from a durable replay checkpoint.
//!
//! The coordinator loads and decodes the M080 checkpoint through M081, binds
//! M082 policy validation to the exact live runtime state, and only then swaps
//! the complete watermark map and generation. Any failure leaves live state
//! untouched. Authentication of checkpoint bytes and exclusive ownership of
//! the checkpoint file remain caller responsibilities.

use crate::x080::{ReplayCheckpointLimits, ReplayPrincipal};
use crate::x081::{self, ReplayCheckpointMetadata, ReplayCheckpointStoreLimits, StoreError};
use crate::x082::{
    validate_restore, ReplayCheckpointRestorePolicy, ReplayCheckpointRestoreReport,
    RestorePolicyError,
};
use std::collections::BTreeMap;
use std::path::Path;

/// The mutable replay state owned by one runtime instance.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReplayRuntimeState {
    pub generation: Option<u64>,
    pub watermarks: BTreeMap<ReplayPrincipal, u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreOutcome {
    pub previous_generation: Option<u64>,
    pub installed_generation: u64,
    pub store_metadata: ReplayCheckpointMetadata,
    pub policy_report: ReplayCheckpointRestoreReport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestoreError {
    Store(StoreError),
    Policy(RestorePolicyError),
    LiveStateTooLarge { actual: usize, maximum: usize },
    InvalidLiveWatermark,
}
impl From<StoreError> for RestoreError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}
impl From<RestorePolicyError> for RestoreError {
    fn from(value: RestorePolicyError) -> Self {
        Self::Policy(value)
    }
}

/// Restores a checkpoint as one in-memory state transition.
///
/// The policy's generation and watermark baselines are replaced with values
/// read from `live`, preventing callers from accidentally validating against
/// stale baselines. All file I/O, decoding, and policy checks complete before
/// either live field is modified. The caller must serialize access to `live`
/// and prevent concurrent replacement of the checkpoint file during this call.
pub fn restore_into(
    checkpoint_path: impl AsRef<Path>,
    live: &mut ReplayRuntimeState,
    policy: &ReplayCheckpointRestorePolicy,
    limits: &ReplayCheckpointStoreLimits,
) -> Result<RestoreOutcome, RestoreError> {
    if live.watermarks.len() > policy.maximum_principals {
        return Err(RestoreError::LiveStateTooLarge {
            actual: live.watermarks.len(),
            maximum: policy.maximum_principals,
        });
    }
    if live.watermarks.values().any(|sequence| *sequence == 0) {
        return Err(RestoreError::InvalidLiveWatermark);
    }

    let (candidate, metadata) = x081::load(checkpoint_path, limits)?;
    let mut bound_policy = policy.clone();
    bound_policy.current_generation = live.generation;
    bound_policy.current_watermarks = live.watermarks.clone();
    let codec_limits: &ReplayCheckpointLimits = &limits.codec;
    let report = validate_restore(&candidate, &bound_policy, codec_limits)?;

    // No fallible operation occurs after this point: the transition is atomic
    // with respect to this function and its exclusively borrowed state value.
    let previous_generation = live.generation;
    let installed_generation = candidate.generation;
    let next_watermarks = candidate.watermarks;
    live.watermarks = next_watermarks;
    live.generation = Some(installed_generation);

    Ok(RestoreOutcome {
        previous_generation,
        installed_generation,
        store_metadata: metadata,
        policy_report: report,
    })
}

/// Exposes the checkpoint codec limits used by the coordinator without
/// requiring callers to construct a second, potentially inconsistent policy.
pub fn default_codec_limits() -> ReplayCheckpointLimits {
    ReplayCheckpointLimits::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x080::ReplayCheckpoint;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn temp_path(label: &str) -> std::path::PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "agentos-m083-{label}-{}-{id}.bin",
            std::process::id()
        ))
    }
    fn principal(consumer: &str, key: &str) -> ReplayPrincipal {
        ReplayPrincipal {
            consumer_id: consumer.to_owned(),
            key_id: key.to_owned(),
        }
    }
    fn write_checkpoint(path: &Path, generation: u64, entries: &[(&str, &str, u64)]) {
        let watermarks = entries
            .iter()
            .map(|(consumer, key, sequence)| (principal(consumer, key), *sequence))
            .collect();
        let checkpoint = ReplayCheckpoint::new(generation, watermarks);
        x081::create_new(path, &checkpoint, &ReplayCheckpointStoreLimits::default())
            .expect("checkpoint creation");
    }

    #[test]
    fn restores_only_after_policy_validation_and_reports_transition() {
        let path = temp_path("success");
        write_checkpoint(
            &path,
            5,
            &[("consumer-a", "key-a", 12), ("consumer-b", "key-b", 3)],
        );
        let mut live = ReplayRuntimeState {
            generation: Some(4),
            watermarks: BTreeMap::from([(principal("consumer-a", "key-a"), 10)]),
        };
        let outcome = restore_into(
            &path,
            &mut live,
            &ReplayCheckpointRestorePolicy::default(),
            &ReplayCheckpointStoreLimits::default(),
        )
        .expect("restore");
        assert_eq!(live.generation, Some(5));
        assert_eq!(
            live.watermarks.get(&principal("consumer-a", "key-a")),
            Some(&12)
        );
        assert_eq!(outcome.previous_generation, Some(4));
        assert_eq!(outcome.policy_report.added_principals, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn failed_policy_validation_leaves_live_state_unchanged() {
        let path = temp_path("regression");
        write_checkpoint(&path, 5, &[("consumer-a", "key-a", 8)]);
        let original = ReplayRuntimeState {
            generation: Some(4),
            watermarks: BTreeMap::from([(principal("consumer-a", "key-a"), 9)]),
        };
        let mut live = original.clone();
        let result = restore_into(
            &path,
            &mut live,
            &ReplayCheckpointRestorePolicy::default(),
            &ReplayCheckpointStoreLimits::default(),
        );
        assert!(matches!(
            result,
            Err(RestoreError::Policy(
                RestorePolicyError::WatermarkRegression { .. }
            ))
        ));
        assert_eq!(live, original);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn missing_checkpoint_does_not_change_runtime() {
        let path = temp_path("missing");
        let original = ReplayRuntimeState::default();
        let mut live = original.clone();
        let result = restore_into(
            &path,
            &mut live,
            &ReplayCheckpointRestorePolicy::default(),
            &ReplayCheckpointStoreLimits::default(),
        );
        assert!(matches!(
            result,
            Err(RestoreError::Store(StoreError::NotFound))
        ));
        assert_eq!(live, original);
    }

    #[test]
    fn rejects_live_state_over_policy_capacity_before_io() {
        let path = temp_path("capacity");
        let mut live = ReplayRuntimeState {
            generation: Some(1),
            watermarks: BTreeMap::from([(principal("a", "k"), 1), (principal("b", "k"), 2)]),
        };
        let policy = ReplayCheckpointRestorePolicy {
            maximum_principals: 1,
            ..ReplayCheckpointRestorePolicy::default()
        };
        let original = live.clone();
        let result = restore_into(
            &path,
            &mut live,
            &policy,
            &ReplayCheckpointStoreLimits::default(),
        );
        assert!(matches!(
            result,
            Err(RestoreError::LiveStateTooLarge { .. })
        ));
        assert_eq!(live, original);
    }
}
