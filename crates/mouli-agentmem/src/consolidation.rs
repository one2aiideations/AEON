//! Deterministic memory consolidation and retention planning.
//!
//! Pure logic only: callers persist records and execute deletion/compaction.
//! The planner groups related records by tenant, agent, and normalized topic,
//! then selects bounded candidates without crossing isolation boundaries.

use std::collections::BTreeMap;

pub type TenantId = u64;
pub type AgentId = u64;
pub type MemoryId = u128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryItem {
    pub id: MemoryId,
    pub tenant_id: TenantId,
    pub agent_id: AgentId,
    pub topic: String,
    pub created_at_ns: u64,
    pub last_accessed_ns: u64,
    pub importance: u8,
    pub text_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsolidationGroup {
    pub tenant_id: TenantId,
    pub agent_id: AgentId,
    pub topic: String,
    pub memory_ids: Vec<MemoryId>,
    pub estimated_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionDecision {
    pub keep: Vec<MemoryId>,
    pub archive_candidates: Vec<MemoryId>,
    pub delete_candidates: Vec<MemoryId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsolidationError {
    EmptyTopic,
    TopicTooLarge,
    ZeroGroupLimit,
    ZeroKeepLimit,
}

#[derive(Clone, Debug)]
pub struct ConsolidationConfig {
    pub max_topic_bytes: usize,
    pub max_group_items: usize,
    pub max_groups: usize,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            max_topic_bytes: 256,
            max_group_items: 256,
            max_groups: 10_000,
        }
    }
}

pub fn plan_groups(
    items: &[MemoryItem],
    config: &ConsolidationConfig,
) -> Result<Vec<ConsolidationGroup>, ConsolidationError> {
    if config.max_group_items == 0 || config.max_groups == 0 {
        return Err(ConsolidationError::ZeroGroupLimit);
    }

    let mut grouped: BTreeMap<(TenantId, AgentId, String), Vec<&MemoryItem>> = BTreeMap::new();

    for item in items {
        let topic = normalize_topic(&item.topic);
        if topic.is_empty() {
            return Err(ConsolidationError::EmptyTopic);
        }
        if topic.len() > config.max_topic_bytes {
            return Err(ConsolidationError::TopicTooLarge);
        }
        grouped
            .entry((item.tenant_id, item.agent_id, topic))
            .or_default()
            .push(item);
    }

    let mut result = Vec::new();
    for ((tenant_id, agent_id, topic), mut group) in grouped {
        group.sort_by(|a, b| {
            b.importance
                .cmp(&a.importance)
                .then_with(|| b.last_accessed_ns.cmp(&a.last_accessed_ns))
                .then_with(|| b.created_at_ns.cmp(&a.created_at_ns))
                .then_with(|| a.id.cmp(&b.id))
        });
        for chunk in group.chunks(config.max_group_items) {
            let mut memory_ids = Vec::with_capacity(chunk.len());
            let mut estimated_bytes = 0usize;
            for item in chunk {
                memory_ids.push(item.id);
                estimated_bytes = estimated_bytes.saturating_add(item.text_bytes);
            }
            result.push(ConsolidationGroup {
                tenant_id,
                agent_id,
                topic: topic.clone(),
                memory_ids,
                estimated_bytes,
            });
            if result.len() >= config.max_groups {
                return Ok(result);
            }
        }
    }
    Ok(result)
}

/// Produce a retention plan. Importance is the primary keep criterion;
/// recent access and creation timestamps break ties deterministically.
/// Items older than `archive_before_ns` are archive candidates unless kept.
pub fn plan_retention(
    items: &[MemoryItem],
    keep_limit: usize,
    archive_before_ns: u64,
) -> Result<RetentionDecision, ConsolidationError> {
    if keep_limit == 0 {
        return Err(ConsolidationError::ZeroKeepLimit);
    }

    let mut ranked: Vec<&MemoryItem> = items.iter().collect();
    ranked.sort_by(|a, b| {
        b.importance
            .cmp(&a.importance)
            .then_with(|| b.last_accessed_ns.cmp(&a.last_accessed_ns))
            .then_with(|| b.created_at_ns.cmp(&a.created_at_ns))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut keep = Vec::new();
    let mut archive_candidates = Vec::new();
    let mut delete_candidates = Vec::new();

    for (index, item) in ranked.into_iter().enumerate() {
        if index < keep_limit {
            keep.push(item.id);
        } else if item.created_at_ns < archive_before_ns {
            archive_candidates.push(item.id);
        } else {
            delete_candidates.push(item.id);
        }
    }

    Ok(RetentionDecision {
        keep,
        archive_candidates,
        delete_candidates,
    })
}

fn normalize_topic(topic: &str) -> String {
    topic.trim().chars().flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u128, tenant: u64, agent: u64, topic: &str, importance: u8) -> MemoryItem {
        MemoryItem {
            id,
            tenant_id: tenant,
            agent_id: agent,
            topic: topic.to_owned(),
            created_at_ns: id as u64,
            last_accessed_ns: id as u64,
            importance,
            text_bytes: 100,
        }
    }

    #[test]
    fn groups_normalize_topics_without_crossing_tenants_or_agents() {
        let items = vec![
            item(1, 7, 2, "Project Alpha", 2),
            item(2, 7, 2, " project alpha ", 5),
            item(3, 8, 2, "project alpha", 3),
            item(4, 7, 3, "project alpha", 3),
        ];
        let groups = plan_groups(&items, &ConsolidationConfig::default());
        assert!(groups.is_ok());
        let groups = groups.unwrap_or_default();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].memory_ids, vec![2, 1]);
    }

    #[test]
    fn retention_keeps_high_importance_and_splits_archive_from_delete() {
        let items = vec![
            item(1, 1, 1, "a", 1),
            item(2, 1, 1, "a", 9),
            item(3, 1, 1, "a", 2),
        ];
        let decision = plan_retention(&items, 1, 3);
        assert!(decision.is_ok());
        let decision = decision.unwrap_or(RetentionDecision {
            keep: Vec::new(),
            archive_candidates: Vec::new(),
            delete_candidates: Vec::new(),
        });
        assert_eq!(decision.keep, vec![2]);
        // archive_before_ns = 3 is exclusive: item 1 (created 1) is archived, item 3 is not older.
        assert_eq!(decision.archive_candidates, vec![1]);
        assert_eq!(decision.delete_candidates, vec![3]);
    }

    #[test]
    fn group_size_and_topic_limits_are_enforced() {
        let config = ConsolidationConfig {
            max_topic_bytes: 3,
            ..ConsolidationConfig::default()
        };
        let result = plan_groups(&[item(1, 1, 1, "long", 1)], &config);
        assert_eq!(result, Err(ConsolidationError::TopicTooLarge));
    }
}
