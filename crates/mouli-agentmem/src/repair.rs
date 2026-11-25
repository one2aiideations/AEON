//! Bounded repair planning for divergent replicated memory inventories.
//!
//! Produces explicit transfer instructions. It never performs network I/O or
//! overwrites records. When versions tie but digests differ, it reports a
//! conflict rather than guessing which peer is authoritative.

use std::collections::BTreeMap;

pub type TenantId = u64;
pub type MemoryId = u128;
pub type ReplicaId = u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairEntry {
    pub memory_id: MemoryId,
    pub version: u64,
    pub content_digest: u64,
    pub estimated_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairAction {
    FetchFromPeer,
    SendToPeer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferInstruction {
    pub tenant_id: TenantId,
    pub memory_id: MemoryId,
    pub action: RepairAction,
    pub expected_version: u64,
    pub estimated_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedConflict {
    pub memory_id: MemoryId,
    pub local_version: u64,
    pub remote_version: u64,
    pub local_digest: u64,
    pub remote_digest: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairPlan {
    pub tenant_id: TenantId,
    pub local_replica: ReplicaId,
    pub remote_replica: ReplicaId,
    pub transfers: Vec<TransferInstruction>,
    pub conflicts: Vec<UnresolvedConflict>,
    pub total_estimated_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepairError {
    ZeroOperationLimit,
    DuplicateMemoryId,
    InventoryTooLarge,
    TransferBudgetExceeded,
}

#[derive(Clone, Debug)]
pub struct RepairConfig {
    pub max_inventory_entries: usize,
    pub max_operations: usize,
    pub max_transfer_bytes: u64,
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self {
            max_inventory_entries: 1_000_000,
            max_operations: 10_000,
            max_transfer_bytes: 1024 * 1024 * 1024,
        }
    }
}

pub fn plan_repair(
    tenant_id: TenantId,
    local_replica: ReplicaId,
    remote_replica: ReplicaId,
    local: &[RepairEntry],
    remote: &[RepairEntry],
    config: &RepairConfig,
) -> Result<RepairPlan, RepairError> {
    if config.max_operations == 0 {
        return Err(RepairError::ZeroOperationLimit);
    }
    if local.len() > config.max_inventory_entries || remote.len() > config.max_inventory_entries {
        return Err(RepairError::InventoryTooLarge);
    }

    let local_map = index_inventory(local)?;
    let remote_map = index_inventory(remote)?;
    let mut ids: Vec<MemoryId> = local_map.keys().chain(remote_map.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();

    let mut plan = RepairPlan {
        tenant_id,
        local_replica,
        remote_replica,
        transfers: Vec::new(),
        conflicts: Vec::new(),
        total_estimated_bytes: 0,
        truncated: false,
    };

    for id in ids {
        let instruction = match (local_map.get(&id), remote_map.get(&id)) {
            (None, Some(remote_entry)) => Some(TransferInstruction {
                tenant_id,
                memory_id: id,
                action: RepairAction::FetchFromPeer,
                expected_version: remote_entry.version,
                estimated_bytes: remote_entry.estimated_bytes,
            }),
            (Some(local_entry), None) => Some(TransferInstruction {
                tenant_id,
                memory_id: id,
                action: RepairAction::SendToPeer,
                expected_version: local_entry.version,
                estimated_bytes: local_entry.estimated_bytes,
            }),
            (Some(local_entry), Some(remote_entry))
                if local_entry.version > remote_entry.version =>
            {
                Some(TransferInstruction {
                    tenant_id,
                    memory_id: id,
                    action: RepairAction::SendToPeer,
                    expected_version: local_entry.version,
                    estimated_bytes: local_entry.estimated_bytes,
                })
            }
            (Some(local_entry), Some(remote_entry))
                if remote_entry.version > local_entry.version =>
            {
                Some(TransferInstruction {
                    tenant_id,
                    memory_id: id,
                    action: RepairAction::FetchFromPeer,
                    expected_version: remote_entry.version,
                    estimated_bytes: remote_entry.estimated_bytes,
                })
            }
            (Some(local_entry), Some(remote_entry))
                if local_entry.content_digest != remote_entry.content_digest =>
            {
                plan.conflicts.push(UnresolvedConflict {
                    memory_id: id,
                    local_version: local_entry.version,
                    remote_version: remote_entry.version,
                    local_digest: local_entry.content_digest,
                    remote_digest: remote_entry.content_digest,
                });
                None
            }
            _ => None,
        };

        if let Some(instruction) = instruction {
            if plan.transfers.len() >= config.max_operations {
                plan.truncated = true;
                break;
            }
            let next_total = plan
                .total_estimated_bytes
                .checked_add(instruction.estimated_bytes)
                .ok_or(RepairError::TransferBudgetExceeded)?;
            if next_total > config.max_transfer_bytes {
                return Err(RepairError::TransferBudgetExceeded);
            }
            plan.total_estimated_bytes = next_total;
            plan.transfers.push(instruction);
        }
    }
    Ok(plan)
}

fn index_inventory(
    entries: &[RepairEntry],
) -> Result<BTreeMap<MemoryId, &RepairEntry>, RepairError> {
    let mut map = BTreeMap::new();
    for entry in entries {
        if map.insert(entry.memory_id, entry).is_some() {
            return Err(RepairError::DuplicateMemoryId);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u128, version: u64, digest: u64, bytes: u64) -> RepairEntry {
        RepairEntry {
            memory_id: id,
            version,
            content_digest: digest,
            estimated_bytes: bytes,
        }
    }

    fn config() -> RepairConfig {
        RepairConfig {
            max_inventory_entries: 100,
            max_operations: 10,
            max_transfer_bytes: 1000,
        }
    }

    #[test]
    fn plans_missing_and_newer_version_transfers() {
        let local = vec![entry(1, 2, 10, 100), entry(2, 1, 20, 50)];
        let remote = vec![entry(1, 1, 10, 90), entry(3, 4, 30, 70)];
        let plan = plan_repair(8, 1, 2, &local, &remote, &config());
        assert!(plan.is_ok());
        let plan = plan.unwrap_or_else(|_| unreachable!());
        assert_eq!(plan.transfers.len(), 3);
        assert_eq!(plan.transfers[0].action, RepairAction::SendToPeer);
        assert_eq!(plan.transfers[1].action, RepairAction::SendToPeer);
        assert_eq!(plan.transfers[2].action, RepairAction::FetchFromPeer);
    }

    #[test]
    fn equal_version_digest_mismatch_is_not_guessed() {
        let plan = plan_repair(
            8,
            1,
            2,
            &[entry(7, 3, 10, 10)],
            &[entry(7, 3, 11, 10)],
            &config(),
        );
        assert!(plan.is_ok());
        let plan = plan.unwrap_or_else(|_| unreachable!());
        assert!(plan.transfers.is_empty());
        assert_eq!(plan.conflicts.len(), 1);
    }

    #[test]
    fn enforces_inventory_operation_and_byte_limits() {
        let mut cfg = config();
        cfg.max_operations = 1;
        let plan = plan_repair(
            1,
            1,
            2,
            &[entry(1, 1, 1, 10), entry(2, 1, 2, 10)],
            &[],
            &cfg,
        );
        assert_eq!(
            plan.map(|p| (p.transfers.len(), p.truncated)),
            Ok((1, true))
        );

        cfg.max_transfer_bytes = 5;
        assert_eq!(
            plan_repair(1, 1, 2, &[entry(1, 1, 1, 10)], &[], &cfg),
            Err(RepairError::TransferBudgetExceeded)
        );
    }
}
