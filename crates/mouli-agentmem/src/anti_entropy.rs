//! Bounded anti-entropy comparison primitives for replicated agent memories.
//!
//! Builds deterministic tenant-scoped range fingerprints and identifies
//! missing or divergent record IDs between peer inventories. The FNV digest
//! is for accidental mismatch detection only, not cryptographic integrity.

use std::collections::{BTreeMap, BTreeSet};

pub type TenantId = u64;
pub type MemoryId = u128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryEntry {
    pub memory_id: MemoryId,
    pub version: u64,
    pub content_digest: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeFingerprint {
    pub tenant_id: TenantId,
    pub start_id: MemoryId,
    pub end_id: MemoryId,
    pub entry_count: usize,
    pub digest: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcilePlan {
    pub request_from_peer: Vec<MemoryId>,
    pub send_to_peer: Vec<MemoryId>,
    pub divergent: Vec<MemoryId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AntiEntropyError {
    ZeroRangeLimit,
    TooManyEntries,
    DuplicateMemoryId,
    InvalidRange,
}

#[derive(Clone, Debug)]
pub struct AntiEntropyConfig {
    pub max_entries: usize,
    pub max_ranges: usize,
}

impl Default for AntiEntropyConfig {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_ranges: 100_000,
        }
    }
}

/// Sort and validate an inventory before computing fingerprints.
pub fn normalize_inventory(
    entries: &[InventoryEntry],
    config: &AntiEntropyConfig,
) -> Result<Vec<InventoryEntry>, AntiEntropyError> {
    if entries.len() > config.max_entries {
        return Err(AntiEntropyError::TooManyEntries);
    }
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.memory_id);
    for pair in sorted.windows(2) {
        if pair[0].memory_id == pair[1].memory_id {
            return Err(AntiEntropyError::DuplicateMemoryId);
        }
    }
    Ok(sorted)
}

/// Partition a tenant inventory into consecutive ID-ordered ranges.
pub fn fingerprint_ranges(
    tenant_id: TenantId,
    entries: &[InventoryEntry],
    range_size: usize,
    config: &AntiEntropyConfig,
) -> Result<Vec<RangeFingerprint>, AntiEntropyError> {
    if range_size == 0 {
        return Err(AntiEntropyError::ZeroRangeLimit);
    }
    let sorted = normalize_inventory(entries, config)?;
    let range_count = sorted.len().saturating_add(range_size - 1) / range_size;
    if range_count > config.max_ranges {
        return Err(AntiEntropyError::TooManyEntries);
    }

    let mut result = Vec::with_capacity(range_count);
    for chunk in sorted.chunks(range_size) {
        let Some(first) = chunk.first() else {
            continue;
        };
        let Some(last) = chunk.last() else {
            continue;
        };
        result.push(RangeFingerprint {
            tenant_id,
            start_id: first.memory_id,
            end_id: last.memory_id,
            entry_count: chunk.len(),
            digest: digest_entries(chunk),
        });
    }
    Ok(result)
}

/// Compare two inventories for one tenant.
///
/// `request_from_peer` contains IDs present locally but absent remotely;
/// `send_to_peer` contains IDs absent locally but present remotely;
/// `divergent` contains IDs present on both sides with different version or
/// content digest.
pub fn reconcile(
    local: &[InventoryEntry],
    remote: &[InventoryEntry],
    config: &AntiEntropyConfig,
) -> Result<ReconcilePlan, AntiEntropyError> {
    let local = normalize_inventory(local, config)?;
    let remote = normalize_inventory(remote, config)?;
    let local_map: BTreeMap<MemoryId, &InventoryEntry> =
        local.iter().map(|entry| (entry.memory_id, entry)).collect();
    let remote_map: BTreeMap<MemoryId, &InventoryEntry> = remote
        .iter()
        .map(|entry| (entry.memory_id, entry))
        .collect();

    let mut plan = ReconcilePlan {
        request_from_peer: Vec::new(),
        send_to_peer: Vec::new(),
        divergent: Vec::new(),
    };

    let all_ids: BTreeSet<MemoryId> = local_map.keys().chain(remote_map.keys()).copied().collect();

    for id in all_ids {
        match (local_map.get(&id), remote_map.get(&id)) {
            (Some(_), None) => plan.request_from_peer.push(id),
            (None, Some(_)) => plan.send_to_peer.push(id),
            (Some(local_entry), Some(remote_entry))
                if local_entry.version != remote_entry.version
                    || local_entry.content_digest != remote_entry.content_digest =>
            {
                plan.divergent.push(id);
            }
            _ => {}
        }
    }
    Ok(plan)
}

fn digest_entries(entries: &[InventoryEntry]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for entry in entries {
        for byte in entry
            .memory_id
            .to_le_bytes()
            .into_iter()
            .chain(entry.version.to_le_bytes())
            .chain(entry.content_digest.to_le_bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u128, version: u64, digest: u64) -> InventoryEntry {
        InventoryEntry {
            memory_id: id,
            version,
            content_digest: digest,
        }
    }

    #[test]
    fn range_fingerprints_are_stable_and_bounded() {
        let entries = vec![entry(3, 1, 30), entry(1, 1, 10), entry(2, 2, 20)];
        let config = AntiEntropyConfig::default();
        let ranges = fingerprint_ranges(8, &entries, 2, &config);
        assert!(ranges.is_ok());
        let ranges = ranges.unwrap_or_default();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start_id, 1);
        assert_eq!(ranges[0].end_id, 2);
        assert_eq!(
            ranges[0].digest,
            digest_entries(&[entry(1, 1, 10), entry(2, 2, 20)])
        );
    }

    #[test]
    fn reconciliation_finds_missing_and_divergent_ids() {
        let local = vec![entry(1, 1, 10), entry(2, 1, 20)];
        let remote = vec![entry(2, 2, 20), entry(3, 1, 30)];
        let plan = reconcile(&local, &remote, &AntiEntropyConfig::default());
        assert_eq!(
            plan,
            Ok(ReconcilePlan {
                request_from_peer: vec![1],
                send_to_peer: vec![3],
                divergent: vec![2],
            })
        );
    }

    #[test]
    fn rejects_duplicate_ids_and_zero_range_size() {
        let config = AntiEntropyConfig::default();
        assert_eq!(
            normalize_inventory(&[entry(1, 1, 1), entry(1, 2, 2)], &config),
            Err(AntiEntropyError::DuplicateMemoryId)
        );
        assert_eq!(
            fingerprint_ranges(1, &[entry(1, 1, 1)], 0, &config),
            Err(AntiEntropyError::ZeroRangeLimit)
        );
    }
}
