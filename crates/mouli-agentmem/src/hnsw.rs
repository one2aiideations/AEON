//! M022 — bounded, tenant-isolated HNSW vector memory with per-vector SQ8.
//!
//! This module is deliberately self-contained and uses only the Rust standard
//! library. Each tenant owns a separate HNSW graph. Vectors are stored as
//! per-vector 8-bit scalar quantized values; search decodes them for cosine
//! distance evaluation. This trades memory for approximation error.
//!
//! The index is in-memory only. Persistence, embedding generation, metadata
//! filtering, distributed replication and an HTTP protocol are intentionally
//! left to higher-level AgentOS services.

use std::{
    cmp::{Ordering, Reverse},
    collections::{BinaryHeap, HashMap},
    error::Error,
    fmt,
};

const DEFAULT_DIMENSIONS: usize = 384;
const DEFAULT_MAX_TENANTS: usize = 10_000;
const DEFAULT_MAX_VECTORS_PER_TENANT: usize = 100_000;
const DEFAULT_M: usize = 12;
const DEFAULT_EF_CONSTRUCTION: usize = 96;
const DEFAULT_EF_SEARCH: usize = 64;
const MAX_DIMENSIONS: usize = 16_384;
const MAX_M: usize = 64;
const MAX_EF: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VectorError {
    InvalidTenant,
    InvalidDimensions,
    DimensionMismatch { expected: usize, actual: usize },
    InvalidConfiguration(String),
    CapacityExceeded,
    DuplicateId(u64),
    InvalidVector,
    NotFound(u64),
    SearchLimit,
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTenant => write!(f, "tenant ID must be non-zero"),
            Self::InvalidDimensions => write!(f, "dimensions must be within 1..={MAX_DIMENSIONS}"),
            Self::DimensionMismatch { expected, actual } => write!(
                f,
                "dimension mismatch: expected {expected}, received {actual}"
            ),
            Self::InvalidConfiguration(message) => write!(f, "invalid configuration: {message}"),
            Self::CapacityExceeded => write!(f, "configured index capacity exceeded"),
            Self::DuplicateId(id) => write!(f, "vector ID {id} already exists"),
            Self::InvalidVector => write!(f, "vector contains non-finite values or has zero norm"),
            Self::NotFound(id) => write!(f, "vector ID {id} not found"),
            Self::SearchLimit => write!(f, "search limit is outside the supported range"),
        }
    }
}

impl Error for VectorError {}

type VectorResult<T> = Result<T, VectorError>;

#[derive(Clone, Copy, Debug)]
struct Candidate {
    distance: f32,
    node: usize,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.distance.to_bits() == other.distance.to_bits() && self.node == other.node
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .total_cmp(&other.distance)
            .then_with(|| self.node.cmp(&other.node))
    }
}

#[derive(Clone, Debug)]
struct QuantizedVector {
    values: Vec<u8>,
    min: f32,
    scale: f32,
}

impl QuantizedVector {
    fn encode(vector: &[f32]) -> VectorResult<Self> {
        if vector.is_empty() || vector.iter().any(|value| !value.is_finite()) {
            return Err(VectorError::InvalidVector);
        }
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for value in vector {
            min = min.min(*value);
            max = max.max(*value);
        }
        let range = max - min;
        let scale = if range > f32::EPSILON {
            range / 255.0
        } else {
            0.0
        };
        let mut values = Vec::with_capacity(vector.len());
        for value in vector {
            let quantized = if scale == 0.0 {
                0
            } else {
                ((*value - min) / scale).round().clamp(0.0, 255.0) as u8
            };
            values.push(quantized);
        }
        Ok(Self { values, min, scale })
    }

    fn decode(&self, output: &mut [f32]) -> VectorResult<()> {
        if output.len() != self.values.len() {
            return Err(VectorError::DimensionMismatch {
                expected: self.values.len(),
                actual: output.len(),
            });
        }
        for (target, encoded) in output.iter_mut().zip(self.values.iter()) {
            *target = self.min + f32::from(*encoded) * self.scale;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct Node {
    id: u64,
    vector: QuantizedVector,
    #[allow(dead_code)] // recorded for inspection; layering is carried by `links`
    level: usize,
    links: Vec<Vec<usize>>,
}

#[derive(Clone, Debug)]
pub struct HnswConfig {
    pub dimensions: usize,
    pub max_vectors: usize,
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub max_level: usize,
    pub seed: u64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            dimensions: DEFAULT_DIMENSIONS,
            max_vectors: DEFAULT_MAX_VECTORS_PER_TENANT,
            m: DEFAULT_M,
            ef_construction: DEFAULT_EF_CONSTRUCTION,
            ef_search: DEFAULT_EF_SEARCH,
            max_level: 16,
            seed: 0x9e37_79b9_7f4a_7c15,
        }
    }
}

impl HnswConfig {
    fn validate(&self) -> VectorResult<()> {
        if self.dimensions == 0 || self.dimensions > MAX_DIMENSIONS {
            return Err(VectorError::InvalidDimensions);
        }
        if self.max_vectors == 0 {
            return Err(VectorError::InvalidConfiguration(
                "max_vectors must be non-zero".into(),
            ));
        }
        if self.m < 2 || self.m > MAX_M {
            return Err(VectorError::InvalidConfiguration(format!(
                "m must be 2..={MAX_M}"
            )));
        }
        if self.ef_construction < self.m || self.ef_construction > MAX_EF {
            return Err(VectorError::InvalidConfiguration(format!(
                "ef_construction must be m..={MAX_EF}"
            )));
        }
        if self.ef_search == 0 || self.ef_search > MAX_EF {
            return Err(VectorError::InvalidConfiguration(format!(
                "ef_search must be 1..={MAX_EF}"
            )));
        }
        if self.max_level == 0 || self.max_level > 32 {
            return Err(VectorError::InvalidConfiguration(
                "max_level must be 1..=32".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub id: u64,
    /// Cosine distance: 0 is identical, 2 is opposite.
    pub distance: f32,
}

pub struct HnswIndex {
    config: HnswConfig,
    nodes: Vec<Node>,
    ids: HashMap<u64, usize>,
    entrypoint: Option<usize>,
    current_max_level: usize,
    rng_state: u64,
}

impl HnswIndex {
    pub fn new(config: HnswConfig) -> VectorResult<Self> {
        config.validate()?;
        let rng_state = if config.seed == 0 { 1 } else { config.seed };
        Ok(Self {
            config,
            nodes: Vec::new(),
            ids: HashMap::new(),
            entrypoint: None,
            current_max_level: 0,
            rng_state,
        })
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn dimensions(&self) -> usize {
        self.config.dimensions
    }

    fn random_u64(&mut self) -> u64 {
        // xorshift64*: deterministic, local PRNG for graph-level assignment.
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn random_level(&mut self) -> usize {
        let mut level = 0usize;
        let threshold = u64::MAX / u64::from(self.config.m as u32);
        while level < self.config.max_level && self.random_u64() > threshold {
            level = level.saturating_add(1);
        }
        level
    }

    fn decode_node(&self, node: usize, output: &mut [f32]) -> VectorResult<()> {
        let stored = self.nodes.get(node).ok_or(VectorError::CapacityExceeded)?;
        stored.vector.decode(output)
    }

    fn cosine_distance(&self, left: &[f32], right: &[f32]) -> f32 {
        let mut dot = 0.0f64;
        let mut left_norm = 0.0f64;
        let mut right_norm = 0.0f64;
        for (a, b) in left.iter().zip(right.iter()) {
            let a = f64::from(*a);
            let b = f64::from(*b);
            dot += a * b;
            left_norm += a * a;
            right_norm += b * b;
        }
        if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
            return 1.0;
        }
        let cosine = (dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0);
        (1.0 - cosine) as f32
    }

    fn distance_to_node(&self, query: &[f32], node: usize, scratch: &mut [f32]) -> f32 {
        if self.decode_node(node, scratch).is_err() {
            return f32::INFINITY;
        }
        self.cosine_distance(query, scratch)
    }

    fn greedy_search(
        &self,
        query: &[f32],
        mut current: usize,
        level: usize,
        scratch: &mut [f32],
    ) -> usize {
        let mut current_distance = self.distance_to_node(query, current, scratch);
        loop {
            let mut changed = false;
            let neighbors = match self
                .nodes
                .get(current)
                .and_then(|node| node.links.get(level))
            {
                Some(value) => value,
                None => break,
            };
            for neighbor in neighbors {
                let distance = self.distance_to_node(query, *neighbor, scratch);
                if distance < current_distance {
                    current = *neighbor;
                    current_distance = distance;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        current
    }

    fn search_layer(
        &self,
        query: &[f32],
        entry_points: &[usize],
        ef: usize,
        level: usize,
    ) -> Vec<Candidate> {
        let mut visited = vec![false; self.nodes.len()];
        let mut candidates: BinaryHeap<Reverse<Candidate>> = BinaryHeap::new();
        let mut best: BinaryHeap<Candidate> = BinaryHeap::new();
        let mut scratch = vec![0.0f32; self.config.dimensions];

        for entry in entry_points {
            if *entry >= self.nodes.len() || visited.get(*entry).copied().unwrap_or(true) {
                continue;
            }
            if let Some(slot) = visited.get_mut(*entry) {
                *slot = true;
            }
            let distance = self.distance_to_node(query, *entry, &mut scratch);
            let candidate = Candidate {
                distance,
                node: *entry,
            };
            candidates.push(Reverse(candidate));
            best.push(candidate);
        }

        while let Some(Reverse(candidate)) = candidates.pop() {
            let worst = best
                .peek()
                .map(|item| item.distance)
                .unwrap_or(f32::INFINITY);
            if best.len() >= ef && candidate.distance > worst {
                break;
            }
            let neighbors = match self
                .nodes
                .get(candidate.node)
                .and_then(|node| node.links.get(level))
            {
                Some(value) => value,
                None => continue,
            };
            for neighbor in neighbors {
                if *neighbor >= visited.len() || visited.get(*neighbor).copied().unwrap_or(true) {
                    continue;
                }
                if let Some(slot) = visited.get_mut(*neighbor) {
                    *slot = true;
                }
                let distance = self.distance_to_node(query, *neighbor, &mut scratch);
                let worst = best
                    .peek()
                    .map(|item| item.distance)
                    .unwrap_or(f32::INFINITY);
                if best.len() < ef || distance < worst {
                    let next = Candidate {
                        distance,
                        node: *neighbor,
                    };
                    candidates.push(Reverse(next));
                    best.push(next);
                    if best.len() > ef {
                        let _ = best.pop();
                    }
                }
            }
        }
        let mut results = best.into_vec();
        results.sort_by(|a, b| a.distance.total_cmp(&b.distance));
        results
    }

    fn select_neighbors(&self, candidates: &[Candidate], limit: usize) -> Vec<usize> {
        let mut selected = Vec::with_capacity(limit.min(candidates.len()));
        for candidate in candidates {
            if selected.len() >= limit {
                break;
            }
            if candidate.node < self.nodes.len() && !selected.contains(&candidate.node) {
                selected.push(candidate.node);
            }
        }
        selected
    }

    fn connect(&mut self, node_index: usize, neighbors: &[usize], level: usize) {
        let Some(node) = self.nodes.get_mut(node_index) else {
            return;
        };
        if let Some(links) = node.links.get_mut(level) {
            for neighbor in neighbors {
                if *neighbor != node_index && !links.contains(neighbor) {
                    links.push(*neighbor);
                }
            }
        }
        for neighbor in neighbors {
            if *neighbor == node_index {
                continue;
            }
            if let Some(other) = self.nodes.get_mut(*neighbor) {
                if let Some(other_links) = other.links.get_mut(level) {
                    if !other_links.contains(&node_index) {
                        other_links.push(node_index);
                    }
                    if other_links.len() > self.config.m.saturating_mul(2) {
                        other_links.truncate(self.config.m.saturating_mul(2));
                    }
                }
            }
        }
    }

    pub fn insert(&mut self, id: u64, vector: &[f32]) -> VectorResult<()> {
        if vector.len() != self.config.dimensions {
            return Err(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: vector.len(),
            });
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(VectorError::InvalidVector);
        }
        let norm = vector.iter().fold(0.0f64, |sum, value| {
            sum + f64::from(*value) * f64::from(*value)
        });
        if norm <= f64::EPSILON {
            return Err(VectorError::InvalidVector);
        }
        if self.ids.contains_key(&id) {
            return Err(VectorError::DuplicateId(id));
        }
        if self.nodes.len() >= self.config.max_vectors {
            return Err(VectorError::CapacityExceeded);
        }
        let quantized = QuantizedVector::encode(vector)?;
        let level = self.random_level();
        let new_index = self.nodes.len();
        let mut node_links = Vec::with_capacity(level.saturating_add(1));
        for _ in 0..=level {
            node_links.push(Vec::new());
        }
        self.nodes.push(Node {
            id,
            vector: quantized,
            level,
            links: node_links,
        });
        self.ids.insert(id, new_index);

        let Some(mut entry) = self.entrypoint else {
            self.entrypoint = Some(new_index);
            self.current_max_level = level;
            return Ok(());
        };

        let mut scratch = vec![0.0f32; self.config.dimensions];
        if self.current_max_level > level {
            for layer in ((level + 1)..=self.current_max_level).rev() {
                entry = self.greedy_search(vector, entry, layer, &mut scratch);
            }
        }

        let upper = level.min(self.current_max_level);
        for layer in (0..=upper).rev() {
            let candidates =
                self.search_layer(vector, &[entry], self.config.ef_construction, layer);
            let selected = self.select_neighbors(&candidates, self.config.m);
            self.connect(new_index, &selected, layer);
            if let Some(best) = candidates.first() {
                entry = best.node;
            }
        }

        if level > self.current_max_level {
            self.entrypoint = Some(new_index);
            self.current_max_level = level;
        }
        Ok(())
    }

    pub fn search(
        &self,
        query: &[f32],
        limit: usize,
        ef_search: Option<usize>,
    ) -> VectorResult<Vec<SearchHit>> {
        if query.len() != self.config.dimensions {
            return Err(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: query.len(),
            });
        }
        if query.iter().any(|value| !value.is_finite()) {
            return Err(VectorError::InvalidVector);
        }
        let norm = query.iter().fold(0.0f64, |sum, value| {
            sum + f64::from(*value) * f64::from(*value)
        });
        if norm <= f64::EPSILON {
            return Err(VectorError::InvalidVector);
        }
        if limit == 0 || limit > self.config.max_vectors {
            return Err(VectorError::SearchLimit);
        }
        let Some(mut entry) = self.entrypoint else {
            return Ok(Vec::new());
        };
        let mut scratch = vec![0.0f32; self.config.dimensions];
        for layer in (1..=self.current_max_level).rev() {
            entry = self.greedy_search(query, entry, layer, &mut scratch);
        }
        let ef = ef_search.unwrap_or(self.config.ef_search).max(limit);
        if ef > MAX_EF {
            return Err(VectorError::SearchLimit);
        }
        let candidates = self.search_layer(query, &[entry], ef, 0);
        let mut hits = Vec::with_capacity(limit.min(candidates.len()));
        for candidate in candidates.into_iter().take(limit) {
            let id = self
                .nodes
                .get(candidate.node)
                .map(|node| node.id)
                .ok_or(VectorError::CapacityExceeded)?;
            hits.push(SearchHit {
                id,
                distance: candidate.distance,
            });
        }
        Ok(hits)
    }

    pub fn contains_id(&self, id: u64) -> bool {
        self.ids.contains_key(&id)
    }

    pub fn memory_bytes_estimate(&self) -> usize {
        self.nodes.iter().fold(0usize, |total, node| {
            let vector_bytes = node.vector.values.len();
            let links_bytes = node.links.iter().fold(0usize, |sum, links| {
                sum.saturating_add(links.len().saturating_mul(std::mem::size_of::<usize>()))
            });
            total
                .saturating_add(vector_bytes)
                .saturating_add(links_bytes)
                .saturating_add(std::mem::size_of::<Node>())
        })
    }
}

pub struct TenantVectorMemory {
    config: HnswConfig,
    max_tenants: usize,
    indexes: HashMap<u64, HnswIndex>,
}

impl TenantVectorMemory {
    pub fn new(config: HnswConfig, max_tenants: usize) -> VectorResult<Self> {
        config.validate()?;
        if max_tenants == 0 || max_tenants > DEFAULT_MAX_TENANTS {
            return Err(VectorError::InvalidConfiguration(format!(
                "max_tenants must be 1..={DEFAULT_MAX_TENANTS}"
            )));
        }
        Ok(Self {
            config,
            max_tenants,
            indexes: HashMap::new(),
        })
    }

    pub fn insert(&mut self, tenant_id: u64, vector_id: u64, vector: &[f32]) -> VectorResult<()> {
        if tenant_id == 0 {
            return Err(VectorError::InvalidTenant);
        }
        if !self.indexes.contains_key(&tenant_id) && self.indexes.len() >= self.max_tenants {
            return Err(VectorError::CapacityExceeded);
        }
        if !self.indexes.contains_key(&tenant_id) {
            let index = HnswIndex::new(self.config.clone())?;
            self.indexes.insert(tenant_id, index);
        }
        let index = self
            .indexes
            .get_mut(&tenant_id)
            .ok_or(VectorError::InvalidTenant)?;
        index.insert(vector_id, vector)
    }

    pub fn search(
        &self,
        tenant_id: u64,
        query: &[f32],
        limit: usize,
        ef_search: Option<usize>,
    ) -> VectorResult<Vec<SearchHit>> {
        if tenant_id == 0 {
            return Err(VectorError::InvalidTenant);
        }
        match self.indexes.get(&tenant_id) {
            Some(index) => index.search(query, limit, ef_search),
            None => Ok(Vec::new()),
        }
    }

    pub fn tenant_count(&self) -> usize {
        self.indexes.len()
    }

    pub fn tenant_vector_count(&self, tenant_id: u64) -> usize {
        self.indexes
            .get(&tenant_id)
            .map(HnswIndex::len)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dimensions: usize) -> HnswConfig {
        HnswConfig {
            dimensions,
            max_vectors: 128,
            m: 4,
            ef_construction: 24,
            ef_search: 24,
            max_level: 8,
            seed: 42,
        }
    }

    #[test]
    fn sq8_round_trip_is_bounded() {
        let input = [-1.0, -0.25, 0.0, 0.5, 1.0];
        let encoded = QuantizedVector::encode(&input).expect("valid vector");
        let mut decoded = [0.0f32; 5];
        encoded.decode(&mut decoded).expect("matching dimensions");
        for (original, restored) in input.iter().zip(decoded.iter()) {
            assert!((original - restored).abs() <= 0.01);
        }
    }

    #[test]
    fn inserts_and_searches_nearest_vector() {
        let mut index = HnswIndex::new(config(3)).expect("valid config");
        index.insert(10, &[1.0, 0.0, 0.0]).expect("insert");
        index.insert(11, &[0.0, 1.0, 0.0]).expect("insert");
        index.insert(12, &[0.9, 0.1, 0.0]).expect("insert");
        let hits = index.search(&[1.0, 0.0, 0.0], 2, Some(16)).expect("search");
        assert_eq!(hits.first().map(|hit| hit.id), Some(10));
        assert_eq!(hits.get(1).map(|hit| hit.id), Some(12));
    }

    #[test]
    fn tenant_indexes_are_isolated() {
        let mut memory = TenantVectorMemory::new(config(2), 4).expect("valid config");
        memory.insert(1, 7, &[1.0, 0.0]).expect("tenant one insert");
        memory.insert(2, 7, &[0.0, 1.0]).expect("tenant two insert");
        let tenant_one = memory.search(1, &[1.0, 0.0], 1, None).expect("search");
        let tenant_two = memory.search(2, &[1.0, 0.0], 1, None).expect("search");
        assert_eq!(tenant_one.first().map(|hit| hit.distance), Some(0.0));
        assert!(tenant_two.first().map(|hit| hit.distance).unwrap_or(0.0) > 0.5);
        assert_eq!(memory.tenant_vector_count(1), 1);
        assert_eq!(memory.tenant_vector_count(2), 1);
    }

    #[test]
    fn rejects_invalid_vectors_and_duplicate_ids() {
        let mut index = HnswIndex::new(config(2)).expect("valid config");
        assert_eq!(
            index.insert(1, &[0.0, 0.0]),
            Err(VectorError::InvalidVector)
        );
        assert!(index.insert(1, &[1.0, 0.0]).is_ok());
        assert_eq!(
            index.insert(1, &[0.0, 1.0]),
            Err(VectorError::DuplicateId(1))
        );
        assert_eq!(
            index.insert(2, &[f32::NAN, 1.0]),
            Err(VectorError::InvalidVector)
        );
    }

    #[test]
    fn capacity_is_enforced() {
        let mut cfg = config(2);
        cfg.max_vectors = 1;
        let mut index = HnswIndex::new(cfg).expect("valid config");
        index.insert(1, &[1.0, 0.0]).expect("first insert");
        assert_eq!(
            index.insert(2, &[0.0, 1.0]),
            Err(VectorError::CapacityExceeded)
        );
    }

    #[test]
    fn empty_tenant_search_is_empty() {
        let memory = TenantVectorMemory::new(config(2), 2).expect("valid config");
        assert!(memory
            .search(1, &[1.0, 0.0], 4, None)
            .expect("search")
            .is_empty());
    }
}
