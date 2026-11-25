//! Byte-budgeted LRU cache of materialised (fully loaded) tensors.
//!
//! Entries are keyed by tensor name and own their bytes. A load that hits
//! the cache with a stale size is counted, dropped and re-read.

use crate::weights::binding::TensorReader;
use crate::weights::index::ModelIndex;
use crate::weights::source::{ModelSource, ModelStream};
use crate::weights::ModelError;
use std::sync::Arc;

/// Counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Lookups served from cache.
    pub hits: u64,
    /// Lookups that missed.
    pub misses: u64,
    /// Entries inserted.
    pub inserts: u64,
    /// Entries evicted to make room.
    pub evictions: u64,
    /// Hits dropped because they no longer matched the tensor.
    pub stale: u64,
    /// Bytes resident now.
    pub bytes_used: usize,
    /// Highest `bytes_used` seen.
    pub peak_bytes: usize,
}

#[derive(Debug)]
struct Entry {
    name: String,
    data: Arc<[u8]>,
    age: u64,
}

/// LRU tensor cache with an entry limit and a byte budget.
#[derive(Debug)]
pub struct TensorCache {
    entries: Vec<Entry>,
    max_entries: usize,
    budget: usize,
    clock: u64,
    stats: CacheStats,
}

impl TensorCache {
    /// Cache holding at most `max_entries` tensors and `budget` bytes.
    pub fn new(max_entries: usize, budget: usize) -> Result<Self, ModelError> {
        if max_entries == 0 || budget == 0 {
            return Err(ModelError::BadConfig);
        }
        Ok(Self {
            entries: Vec::with_capacity(max_entries),
            max_entries,
            budget,
            clock: 0,
            stats: CacheStats::default(),
        })
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn remove_at(&mut self, i: usize) -> Entry {
        let e = self.entries.swap_remove(i);
        self.stats.bytes_used -= e.data.len();
        e
    }

    fn lru(&self) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| e.age)
            .map(|(i, _)| i)
    }

    /// Look up `name`, refreshing its age.
    pub fn get(&mut self, name: &str) -> Option<Arc<[u8]>> {
        match self.entries.iter().position(|e| e.name == name) {
            Some(i) => {
                self.stats.hits += 1;
                let age = self.tick();
                self.entries[i].age = age;
                Some(Arc::clone(&self.entries[i].data))
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    /// Insert, evicting least-recently-used entries until it fits.
    pub fn put(&mut self, name: &str, data: Arc<[u8]>) -> Result<(), ModelError> {
        if data.is_empty() || data.len() > self.budget {
            return Err(ModelError::BadConfig);
        }
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.remove_at(i);
        }
        while self.entries.len() >= self.max_entries
            || self.stats.bytes_used > self.budget - data.len()
        {
            let i = self.lru().ok_or(ModelError::NoSlot)?;
            self.remove_at(i);
            self.stats.evictions += 1;
        }
        let age = self.tick();
        self.stats.bytes_used += data.len();
        self.stats.peak_bytes = self.stats.peak_bytes.max(self.stats.bytes_used);
        self.stats.inserts += 1;
        self.entries.push(Entry {
            name: name.to_owned(),
            data,
            age,
        });
        Ok(())
    }

    /// Check the cache's internal invariants (the Rust form of the C `aeon_tensor_cache_validate`):
    /// resident bytes within budget, entry count within the limit, every entry non-empty with a
    /// distinct non-zero age and a unique name, and `bytes_used` equal to the sum of entry sizes.
    ///
    /// Meant for tests and debug assertions; a healthy cache always passes.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.entries.len() > self.max_entries {
            return Err(ModelError::Inconsistent("more entries than the limit"));
        }
        if self.stats.bytes_used > self.budget {
            return Err(ModelError::Inconsistent("bytes used exceed the budget"));
        }
        let mut resident = 0usize;
        for (i, e) in self.entries.iter().enumerate() {
            if e.data.is_empty() {
                return Err(ModelError::Inconsistent("empty entry"));
            }
            if e.age == 0 || e.age > self.clock {
                return Err(ModelError::Inconsistent("entry age out of range"));
            }
            resident = resident
                .checked_add(e.data.len())
                .ok_or(ModelError::Inconsistent("resident bytes overflow"))?;
            for other in &self.entries[i + 1..] {
                if other.name == e.name {
                    return Err(ModelError::Inconsistent("duplicate entry name"));
                }
                if other.age == e.age {
                    return Err(ModelError::Inconsistent("duplicate entry age"));
                }
            }
        }
        if resident != self.stats.bytes_used {
            return Err(ModelError::Inconsistent("bytes used do not match entries"));
        }
        Ok(())
    }

    /// Drop one entry. Returns whether it was present.
    pub fn invalidate(&mut self, name: &str) -> bool {
        match self.entries.iter().position(|e| e.name == name) {
            Some(i) => {
                self.remove_at(i);
                true
            }
            None => false,
        }
    }

    /// Drop everything (stats other than `bytes_used` are kept).
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
        self.stats.bytes_used = 0;
    }

    /// Change the budget; fails if current residency exceeds it.
    pub fn set_budget(&mut self, budget: usize) -> Result<(), ModelError> {
        if budget == 0 || budget < self.stats.bytes_used {
            return Err(ModelError::BadConfig);
        }
        self.budget = budget;
        Ok(())
    }

    /// Current budget.
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Return the tensor's bytes from cache, or read and cache them.
    pub fn materialize<S: ModelSource>(
        &mut self,
        stream: &mut ModelStream<S>,
        index: &ModelIndex,
        name: &str,
    ) -> Result<Arc<[u8]>, ModelError> {
        let meta = index.find(name).ok_or(ModelError::NotFound)?;
        if let Some(hit) = self.get(name) {
            if hit.len() as u64 == meta.bytes {
                return Ok(hit);
            }
            self.stats.stale += 1;
            self.invalidate(name);
        }
        let data: Arc<[u8]> = TensorReader::bind(stream, index, name)?.read_all()?.into();
        self.put(name, Arc::clone(&data))?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weights::manifest;

    fn bytes(n: usize) -> Arc<[u8]> {
        vec![1u8; n].into()
    }

    #[test]
    fn lru_by_count_and_budget() {
        let mut c = TensorCache::new(2, 10).unwrap();
        c.put("a", bytes(4)).unwrap();
        c.put("b", bytes(4)).unwrap();
        assert!(c.get("a").is_some());
        c.put("c", bytes(4)).unwrap(); // evicts b (count)
        assert!(c.get("b").is_none());
        c.put("d", bytes(8)).unwrap(); // evicts a and c (budget)
        assert_eq!(c.stats().bytes_used, 8);
        assert_eq!(c.stats().evictions, 3);
        assert_eq!(c.stats().peak_bytes, 8);
        assert!(c.put("e", bytes(11)).is_err());
        assert_eq!(c.set_budget(7), Err(ModelError::BadConfig));
    }

    #[test]
    fn validate_passes_through_normal_use() {
        let mut c = TensorCache::new(3, 12).unwrap();
        c.validate().unwrap();
        for (n, size) in [("a", 4), ("b", 4), ("c", 4), ("d", 8), ("a", 3)] {
            c.put(n, bytes(size)).unwrap();
            c.validate().unwrap();
            let _ = c.get("b");
            c.validate().unwrap();
        }
        c.invalidate("a");
        c.validate().unwrap();
        c.invalidate_all();
        c.validate().unwrap();
        assert_eq!(c.stats().bytes_used, 0);
    }

    #[test]
    fn validate_detects_corruption() {
        let mut c = TensorCache::new(4, 100).unwrap();
        c.put("a", bytes(10)).unwrap();
        c.put("b", bytes(10)).unwrap();

        let mut bad = TensorCache::new(4, 100).unwrap();
        bad.put("a", bytes(10)).unwrap();
        bad.stats.bytes_used = 11;
        assert_eq!(
            bad.validate(),
            Err(ModelError::Inconsistent("bytes used do not match entries"))
        );
        bad.stats.bytes_used = 101;
        assert_eq!(
            bad.validate(),
            Err(ModelError::Inconsistent("bytes used exceed the budget"))
        );

        c.entries[1].name = "a".into();
        assert_eq!(
            c.validate(),
            Err(ModelError::Inconsistent("duplicate entry name"))
        );
        c.entries[1].name = "b".into();
        c.entries[1].age = c.entries[0].age;
        assert_eq!(
            c.validate(),
            Err(ModelError::Inconsistent("duplicate entry age"))
        );
        c.entries[1].age = 0;
        assert_eq!(
            c.validate(),
            Err(ModelError::Inconsistent("entry age out of range"))
        );
        c.entries[1].age = c.clock + 1;
        assert_eq!(
            c.validate(),
            Err(ModelError::Inconsistent("entry age out of range"))
        );
        c.entries[1].age = 2;
        c.entries[1].data = Arc::from(Vec::new());
        assert_eq!(c.validate(), Err(ModelError::Inconsistent("empty entry")));
        c.entries.push(Entry {
            name: "x".into(),
            data: bytes(1),
            age: 9,
        });
        c.entries.push(Entry {
            name: "y".into(),
            data: bytes(1),
            age: 10,
        });
        c.entries.push(Entry {
            name: "z".into(),
            data: bytes(1),
            age: 11,
        });
        assert_eq!(
            c.validate(),
            Err(ModelError::Inconsistent("more entries than the limit"))
        );
    }

    #[test]
    fn inconsistent_error_displays_its_reason() {
        assert_eq!(
            ModelError::Inconsistent("x").to_string(),
            "inconsistent state: x"
        );
    }

    #[test]
    fn materialize_reads_once() {
        let ix = manifest::parse("w | 2 | 4 | I8 | 1 | 4\n", 8).unwrap();
        let mut s = ModelStream::new((0u8..8).collect::<Vec<u8>>()).unwrap();
        let mut c = TensorCache::new(4, 64).unwrap();
        let a = c.materialize(&mut s, &ix, "w").unwrap();
        assert_eq!(&a[..], &[2, 3, 4, 5]);
        let b = c.materialize(&mut s, &ix, "w").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(c.stats().hits, 1);
        assert!(c.invalidate("w"));
        assert!(!c.invalidate("w"));
    }

    #[test]
    fn stale_entry_is_replaced() {
        let ix = manifest::parse("w | 0 | 4 | I8 | 1 | 4\n", 4).unwrap();
        let mut s = ModelStream::new(vec![9u8; 4]).unwrap();
        let mut c = TensorCache::new(4, 64).unwrap();
        c.put("w", bytes(2)).unwrap();
        let got = c.materialize(&mut s, &ix, "w").unwrap();
        assert_eq!(&got[..], &[9, 9, 9, 9]);
        assert_eq!(c.stats().stale, 1);
    }
}
