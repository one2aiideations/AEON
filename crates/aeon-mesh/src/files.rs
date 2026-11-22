//! Content-addressed file sync.
//!
//! Files are split into fixed-size chunks named by their SHA-256; a
//! manifest lists the chunk hashes and its own hash identifies the file
//! version. Version vectors (one counter per device) tell whether one
//! version descends from another or the two were edited concurrently.

use crate::nodes::NodeId;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// 32-byte content hash.
pub type Hash = [u8; 32];

/// Chunk list of one file version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Chunk size used.
    pub chunk_size: usize,
    /// Total length in bytes.
    pub len: u64,
    /// Chunk hashes in order.
    pub chunks: Vec<Hash>,
}

impl Manifest {
    /// Split `data` into chunks; returns the manifest and the chunks by hash.
    #[must_use]
    pub fn build(data: &[u8], chunk_size: usize) -> (Self, BTreeMap<Hash, Vec<u8>>) {
        let chunk_size = chunk_size.max(1);
        let mut store = BTreeMap::new();
        let chunks = data
            .chunks(chunk_size)
            .map(|c| {
                let h: Hash = Sha256::digest(c).into();
                store.entry(h).or_insert_with(|| c.to_vec());
                h
            })
            .collect();
        (
            Self {
                chunk_size,
                len: data.len() as u64,
                chunks,
            },
            store,
        )
    }

    /// Hash identifying this version.
    #[must_use]
    pub fn id(&self) -> Hash {
        let mut h = Sha256::new();
        h.update((self.chunk_size as u64).to_le_bytes());
        h.update(self.len.to_le_bytes());
        for c in &self.chunks {
            h.update(c);
        }
        h.finalize().into()
    }

    /// Chunks of `self` not present in `have` (what must be transferred).
    #[must_use]
    pub fn missing<'a>(&'a self, have: &BTreeMap<Hash, Vec<u8>>) -> Vec<&'a Hash> {
        let mut seen = std::collections::BTreeSet::new();
        self.chunks
            .iter()
            .filter(|h| !have.contains_key(*h) && seen.insert(**h))
            .collect()
    }

    /// Reassemble and verify. `None` if a chunk is missing or corrupt.
    #[must_use]
    pub fn assemble(&self, store: &BTreeMap<Hash, Vec<u8>>) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(usize::try_from(self.len).ok()?);
        for h in &self.chunks {
            let c = store.get(h)?;
            let actual: Hash = Sha256::digest(c).into();
            if &actual != h {
                return None;
            }
            out.extend_from_slice(c);
        }
        (out.len() as u64 == self.len).then_some(out)
    }
}

/// Per-device edit counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionVector(BTreeMap<NodeId, u64>);

/// Relation between two versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causality {
    /// Identical history.
    Equal,
    /// Left descends from right.
    After,
    /// Right descends from left.
    Before,
    /// Edited independently: a conflict to resolve.
    Concurrent,
}

impl VersionVector {
    /// Empty vector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an edit on `node`.
    pub fn bump(&mut self, node: NodeId) {
        *self.0.entry(node).or_insert(0) += 1;
    }

    /// Component-wise maximum (after resolving a conflict).
    pub fn merge(&mut self, other: &Self) {
        for (&n, &c) in &other.0 {
            let e = self.0.entry(n).or_insert(0);
            *e = (*e).max(c);
        }
    }

    /// Compare histories.
    #[must_use]
    pub fn compare(&self, other: &Self) -> Causality {
        let mut less = false;
        let mut greater = false;
        for n in self.0.keys().chain(other.0.keys()) {
            let a = self.0.get(n).copied().unwrap_or(0);
            let b = other.0.get(n).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Less => less = true,
                Ordering::Greater => greater = true,
                Ordering::Equal => {}
            }
        }
        match (less, greater) {
            (false, false) => Causality::Equal,
            (false, true) => Causality::After,
            (true, false) => Causality::Before,
            (true, true) => Causality::Concurrent,
        }
    }
}

/// Choose up to `copies` replica holders from `live` nodes, preferring
/// nodes that already hold the file, then by id. Deterministic per file.
#[must_use]
pub fn place_replicas(file: &Hash, live: &[NodeId], holders: &[NodeId], copies: usize) -> Vec<NodeId> {
    let mut v: Vec<NodeId> = live.to_vec();
    v.sort_by_key(|n| {
        // Rendezvous hashing: stable spread of files across nodes.
        let mut h = Sha256::new();
        h.update(file);
        h.update(n.to_le_bytes());
        let d: Hash = h.finalize().into();
        (!holders.contains(n), d)
    });
    v.dedup();
    v.truncate(copies);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_round_trip_and_dedup() {
        let data = b"abcdabcdabXY".to_vec();
        let (m, store) = Manifest::build(&data, 4);
        assert_eq!(m.chunks.len(), 3);
        assert_eq!(store.len(), 2, "repeated chunk stored once");
        assert_eq!(m.assemble(&store).unwrap(), data);
        let (m2, _) = Manifest::build(b"abcdabcdabXZ", 4);
        assert_ne!(m.id(), m2.id());
        assert_eq!(m2.missing(&store).len(), 1);
    }

    #[test]
    fn corrupt_chunk_rejected() {
        let (m, mut store) = Manifest::build(b"hello world", 4);
        let k = m.chunks[1];
        store.get_mut(&k).unwrap()[0] ^= 1;
        assert!(m.assemble(&store).is_none());
    }

    #[test]
    fn version_vectors() {
        let mut a = VersionVector::new();
        a.bump(1);
        let mut b = a.clone();
        assert_eq!(a.compare(&b), Causality::Equal);
        b.bump(2);
        assert_eq!(b.compare(&a), Causality::After);
        assert_eq!(a.compare(&b), Causality::Before);
        a.bump(1);
        assert_eq!(a.compare(&b), Causality::Concurrent);
        a.merge(&b);
        a.bump(1);
        assert_eq!(a.compare(&b), Causality::After);
    }

    #[test]
    fn replica_placement() {
        let f = [7u8; 32];
        let p = place_replicas(&f, &[1, 2, 3, 4], &[3], 2);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], 3, "existing holder kept first");
        assert_eq!(place_replicas(&f, &[1, 2, 3, 4], &[3], 2), p);
        assert!(place_replicas(&f, &[], &[], 3).is_empty());
    }
}
