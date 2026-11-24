//! Deterministic temporal scoring and expiry planning for agent memories.
//!
//! Pure policy logic. It does not delete records, schedule jobs, or access a
//! clock; callers provide the evaluation timestamp and execute returned plans.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalMemory {
    pub id: u128,
    pub created_at_ns: u64,
    pub last_accessed_ns: u64,
    pub base_importance: u16,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalScore {
    pub id: u128,
    pub score: u32,
    pub age_ns: u64,
    pub idle_ns: u64,
    pub expired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemporalError {
    ZeroHalfLife,
    ZeroExpiry,
    ZeroLimit,
    FutureTimestamp,
}

#[derive(Clone, Debug)]
pub struct TemporalConfig {
    /// Time after which recency contribution halves.
    pub half_life_ns: u64,
    /// Idle duration after which an unpinned memory is eligible for expiry.
    pub expiry_idle_ns: u64,
    /// Maximum number of returned records.
    pub max_results: usize,
}

impl Default for TemporalConfig {
    fn default() -> Self {
        Self {
            half_life_ns: 7 * 24 * 60 * 60 * 1_000_000_000,
            expiry_idle_ns: 90 * 24 * 60 * 60 * 1_000_000_000,
            max_results: 10_000,
        }
    }
}

/// Score importance plus exponentially decaying recency.
///
/// The recency component starts at 1000 and is halved once per half-life.
/// Arithmetic is integer-only and saturating for deterministic behavior.
pub fn score_memory(
    memory: &TemporalMemory,
    now_ns: u64,
    config: &TemporalConfig,
) -> Result<TemporalScore, TemporalError> {
    validate(config)?;
    if memory.created_at_ns > now_ns || memory.last_accessed_ns > now_ns {
        return Err(TemporalError::FutureTimestamp);
    }
    let age_ns = now_ns.saturating_sub(memory.created_at_ns);
    let idle_ns = now_ns.saturating_sub(memory.last_accessed_ns);
    let recency = decay_score(idle_ns, config.half_life_ns);
    let importance = u32::from(memory.base_importance).min(1000);
    let score = importance.saturating_add(recency).min(2000);
    Ok(TemporalScore {
        id: memory.id,
        score,
        age_ns,
        idle_ns,
        expired: !memory.pinned && idle_ns >= config.expiry_idle_ns,
    })
}

/// Rank a bounded collection by score, then recency, then stable ID.
pub fn rank_memories(
    memories: &[TemporalMemory],
    now_ns: u64,
    config: &TemporalConfig,
) -> Result<Vec<TemporalScore>, TemporalError> {
    validate(config)?;
    if memories.len() > config.max_results {
        return Err(TemporalError::ZeroLimit);
    }
    let mut scores = Vec::with_capacity(memories.len());
    for memory in memories {
        scores.push(score_memory(memory, now_ns, config)?);
    }
    scores.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.idle_ns.cmp(&b.idle_ns))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(scores)
}

/// Return IDs eligible for expiry. Pinned records are never selected.
pub fn expiry_candidates(
    memories: &[TemporalMemory],
    now_ns: u64,
    config: &TemporalConfig,
) -> Result<Vec<u128>, TemporalError> {
    let ranked = rank_memories(memories, now_ns, config)?;
    Ok(ranked
        .into_iter()
        .filter(|score| score.expired)
        .map(|score| score.id)
        .collect())
}

fn decay_score(idle_ns: u64, half_life_ns: u64) -> u32 {
    let periods = idle_ns / half_life_ns;
    if periods >= 11 {
        return 0;
    }
    1000u32 >> u32::try_from(periods).unwrap_or(11)
}

fn validate(config: &TemporalConfig) -> Result<(), TemporalError> {
    if config.half_life_ns == 0 {
        return Err(TemporalError::ZeroHalfLife);
    }
    if config.expiry_idle_ns == 0 {
        return Err(TemporalError::ZeroExpiry);
    }
    if config.max_results == 0 {
        return Err(TemporalError::ZeroLimit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TemporalConfig {
        TemporalConfig {
            half_life_ns: 100,
            expiry_idle_ns: 250,
            max_results: 20,
        }
    }

    #[test]
    fn recency_decays_by_half_life() {
        let memory = TemporalMemory {
            id: 1,
            created_at_ns: 0,
            last_accessed_ns: 100,
            base_importance: 0,
            pinned: false,
        };
        let score = score_memory(&memory, 200, &config());
        assert_eq!(score.map(|value| value.score), Ok(500));
    }

    #[test]
    fn expiry_excludes_pinned_memories() {
        let memories = vec![
            TemporalMemory {
                id: 1,
                created_at_ns: 0,
                last_accessed_ns: 0,
                base_importance: 10,
                pinned: false,
            },
            TemporalMemory {
                id: 2,
                created_at_ns: 0,
                last_accessed_ns: 0,
                base_importance: 10,
                pinned: true,
            },
        ];
        assert_eq!(expiry_candidates(&memories, 300, &config()), Ok(vec![1]));
    }

    #[test]
    fn rejects_future_timestamps_and_oversized_input() {
        let future = TemporalMemory {
            id: 1,
            created_at_ns: 101,
            last_accessed_ns: 101,
            base_importance: 1,
            pinned: false,
        };
        assert_eq!(
            score_memory(&future, 100, &config()),
            Err(TemporalError::FutureTimestamp)
        );
        let mut cfg = config();
        cfg.max_results = 1;
        assert_eq!(
            rank_memories(&[future.clone(), future], 200, &cfg),
            Err(TemporalError::ZeroLimit)
        );
    }
}
