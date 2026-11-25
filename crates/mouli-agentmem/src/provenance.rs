//! Provenance and conflict-resolution primitives for agent memories.
//!
//! This crate does not decide whether a source is truthful. It records source
//! metadata and produces deterministic candidate precedence decisions for the
//! caller to review or apply.

use std::collections::BTreeMap;

pub type TenantId = u64;
pub type MemoryId = u128;
pub type SourceId = u128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub source_id: SourceId,
    pub source_kind: String,
    pub observed_at_ns: u64,
    pub confidence_milli: u16,
    pub authority_rank: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcedMemory {
    pub id: MemoryId,
    pub tenant_id: TenantId,
    pub fact_key: String,
    pub value: String,
    pub provenance: Provenance,
    pub supersedes: Vec<MemoryId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProvenanceError {
    EmptyFactKey,
    EmptyValue,
    MetadataTooLarge,
    ConfidenceOutOfRange,
    DuplicateMemory,
    TenantMismatch,
    UnknownSupersededMemory,
    ConflictingSupersession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictSet {
    pub tenant_id: TenantId,
    pub fact_key: String,
    pub candidates: Vec<MemoryId>,
    pub preferred: Option<MemoryId>,
}

#[derive(Default)]
pub struct ProvenanceLedger {
    memories: BTreeMap<(TenantId, MemoryId), SourcedMemory>,
    superseded_by: BTreeMap<(TenantId, MemoryId), MemoryId>,
    max_key_bytes: usize,
    max_value_bytes: usize,
    max_source_kind_bytes: usize,
    max_supersedes: usize,
}

impl ProvenanceLedger {
    pub fn new(
        max_key_bytes: usize,
        max_value_bytes: usize,
        max_source_kind_bytes: usize,
        max_supersedes: usize,
    ) -> Self {
        Self {
            memories: BTreeMap::new(),
            superseded_by: BTreeMap::new(),
            max_key_bytes,
            max_value_bytes,
            max_source_kind_bytes,
            max_supersedes,
        }
    }

    pub fn insert(&mut self, memory: SourcedMemory) -> Result<(), ProvenanceError> {
        if memory.fact_key.trim().is_empty() {
            return Err(ProvenanceError::EmptyFactKey);
        }
        if memory.value.trim().is_empty() {
            return Err(ProvenanceError::EmptyValue);
        }
        if memory.fact_key.len() > self.max_key_bytes
            || memory.value.len() > self.max_value_bytes
            || memory.provenance.source_kind.len() > self.max_source_kind_bytes
        {
            return Err(ProvenanceError::MetadataTooLarge);
        }
        if memory.provenance.confidence_milli > 1000 {
            return Err(ProvenanceError::ConfidenceOutOfRange);
        }
        let key = (memory.tenant_id, memory.id);
        if self.memories.contains_key(&key) {
            return Err(ProvenanceError::DuplicateMemory);
        }
        if memory.supersedes.len() > self.max_supersedes {
            return Err(ProvenanceError::MetadataTooLarge);
        }

        for old_id in &memory.supersedes {
            if !self.memories.contains_key(&(memory.tenant_id, *old_id)) {
                return Err(ProvenanceError::UnknownSupersededMemory);
            }
            if self
                .superseded_by
                .contains_key(&(memory.tenant_id, *old_id))
            {
                return Err(ProvenanceError::ConflictingSupersession);
            }
        }

        for old_id in &memory.supersedes {
            self.superseded_by
                .insert((memory.tenant_id, *old_id), memory.id);
        }
        self.memories.insert(key, memory);
        Ok(())
    }

    pub fn get(&self, tenant_id: TenantId, id: MemoryId) -> Option<&SourcedMemory> {
        self.memories.get(&(tenant_id, id))
    }

    pub fn is_superseded(&self, tenant_id: TenantId, id: MemoryId) -> bool {
        self.superseded_by.contains_key(&(tenant_id, id))
    }

    pub fn superseding_memory(&self, tenant_id: TenantId, id: MemoryId) -> Option<MemoryId> {
        self.superseded_by.get(&(tenant_id, id)).copied()
    }

    /// Return active competing facts, grouped by normalized fact key.
    /// Preferred candidate is selected by authority, confidence, observation
    /// time, then lower memory ID. This is ranking, not truth verification.
    pub fn conflicts(&self, tenant_id: TenantId) -> Vec<ConflictSet> {
        let mut groups: BTreeMap<String, Vec<&SourcedMemory>> = BTreeMap::new();
        for ((record_tenant, id), memory) in &self.memories {
            if *record_tenant != tenant_id || self.is_superseded(tenant_id, *id) {
                continue;
            }
            groups
                .entry(normalize_key(&memory.fact_key))
                .or_default()
                .push(memory);
        }

        let mut result = Vec::new();
        for (fact_key, mut candidates) in groups {
            if candidates.len() < 2 {
                continue;
            }
            candidates.sort_by(|a, b| precedence(b, a));
            let preferred = candidates.first().map(|memory| memory.id);
            result.push(ConflictSet {
                tenant_id,
                fact_key,
                candidates: candidates.iter().map(|memory| memory.id).collect(),
                preferred,
            });
        }
        result
    }

    pub fn len(&self) -> usize {
        self.memories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.memories.is_empty()
    }
}

fn precedence(a: &SourcedMemory, b: &SourcedMemory) -> std::cmp::Ordering {
    a.provenance
        .authority_rank
        .cmp(&b.provenance.authority_rank)
        .then_with(|| {
            a.provenance
                .confidence_milli
                .cmp(&b.provenance.confidence_milli)
        })
        .then_with(|| {
            a.provenance
                .observed_at_ns
                .cmp(&b.provenance.observed_at_ns)
        })
        .then_with(|| b.id.cmp(&a.id))
}

fn normalize_key(value: &str) -> String {
    value.trim().chars().flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: u128, tenant: u64, key: &str, value: &str, authority: u16) -> SourcedMemory {
        SourcedMemory {
            id,
            tenant_id: tenant,
            fact_key: key.to_owned(),
            value: value.to_owned(),
            provenance: Provenance {
                source_id: id + 100,
                source_kind: "operator".to_owned(),
                observed_at_ns: id as u64,
                confidence_milli: 900,
                authority_rank: authority,
            },
            supersedes: Vec::new(),
        }
    }

    fn ledger() -> ProvenanceLedger {
        ProvenanceLedger::new(128, 2048, 64, 16)
    }

    #[test]
    fn ranks_conflicts_and_isolates_tenants() {
        let mut ledger = ledger();
        assert!(ledger
            .insert(memory(1, 4, "account.status", "pending", 1))
            .is_ok());
        assert!(ledger
            .insert(memory(2, 4, " ACCOUNT.STATUS ", "active", 5))
            .is_ok());
        assert!(ledger
            .insert(memory(3, 5, "account.status", "closed", 9))
            .is_ok());
        let conflicts = ledger.conflicts(4);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].preferred, Some(2));
        assert_eq!(ledger.conflicts(5).len(), 0);
    }

    #[test]
    fn explicit_supersession_removes_old_fact_from_active_conflicts() {
        let mut ledger = ledger();
        assert!(ledger.insert(memory(10, 1, "status", "old", 1)).is_ok());
        let mut replacement = memory(11, 1, "status", "new", 2);
        replacement.supersedes.push(10);
        assert!(ledger.insert(replacement).is_ok());
        assert!(ledger.is_superseded(1, 10));
        assert_eq!(ledger.superseding_memory(1, 10), Some(11));
        assert!(ledger.conflicts(1).is_empty());
    }

    #[test]
    fn rejects_cross_tenant_or_unknown_supersession() {
        let mut ledger = ledger();
        let mut candidate = memory(2, 9, "k", "v", 1);
        candidate.supersedes.push(1);
        assert_eq!(
            ledger.insert(candidate),
            Err(ProvenanceError::UnknownSupersededMemory)
        );
    }
}
