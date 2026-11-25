//! Working memory: a small, bounded store with priorities and expiry.
//!
//! Holds the handful of things the agent is attending to right now. Items carry a priority
//! and an optional time-to-live that restarts whenever the item is read, so what is used stays
//! and what is ignored fades. When the store is full the least important item makes room; an
//! incoming item that is *less* important than everything already held is refused instead.
//! Time is a caller-supplied tick count, so behaviour is deterministic.

use crate::CogError;

#[derive(Debug, Clone)]
struct Item<V> {
    key: String,
    value: V,
    priority: f32,
    ttl: Option<u64>,
    last_access: u64,
}

impl<V> Item<V> {
    fn expired(&self, now: u64) -> bool {
        self.ttl
            .is_some_and(|t| now.saturating_sub(self.last_access) > t)
    }
}

/// What happened to a `put`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Put {
    /// Stored (a same-key item was replaced, or there was room).
    Stored,
    /// Stored after evicting this key to make room.
    Evicted(String),
    /// Not stored: it was less important than every item already held.
    Refused,
}

/// Bounded working memory.
#[derive(Debug, Clone)]
pub struct WorkingMemory<V> {
    cap: usize,
    items: Vec<Item<V>>,
}

impl<V> WorkingMemory<V> {
    /// Memory holding at most `cap` items (at least 1).
    pub fn new(cap: usize) -> Result<Self, CogError> {
        if cap == 0 {
            return Err(CogError::BadValue);
        }
        Ok(WorkingMemory {
            cap,
            items: Vec::with_capacity(cap),
        })
    }

    /// Drop every item whose time-to-live has run out; returns how many.
    pub fn sweep(&mut self, now: u64) -> usize {
        let before = self.items.len();
        self.items.retain(|i| !i.expired(now));
        before - self.items.len()
    }

    /// Store `value` under `key`. `priority` must be finite. `ttl` is in ticks since the last
    /// access (`None` never expires). Same-key items are replaced. Expired items are swept first.
    pub fn put(
        &mut self,
        key: &str,
        value: V,
        priority: f32,
        ttl: Option<u64>,
        now: u64,
    ) -> Result<Put, CogError> {
        if !priority.is_finite() {
            return Err(CogError::BadValue);
        }
        self.sweep(now);
        let item = Item {
            key: key.to_string(),
            value,
            priority,
            ttl,
            last_access: now,
        };
        if let Some(i) = self.items.iter().position(|e| e.key == key) {
            self.items[i] = item;
            return Ok(Put::Stored);
        }
        if self.items.len() < self.cap {
            self.items.push(item);
            return Ok(Put::Stored);
        }
        // Victim: lowest priority, then least recently used, then earliest inserted.
        let (vi, victim) = self
            .items
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.priority
                    .total_cmp(&b.priority)
                    .then(a.last_access.cmp(&b.last_access))
            })
            .map(|(i, v)| (i, v.priority))
            .ok_or(CogError::Empty)?;
        if priority < victim {
            return Ok(Put::Refused);
        }
        let gone = std::mem::replace(&mut self.items[vi], item);
        Ok(Put::Evicted(gone.key))
    }

    /// Read an item, restarting its time-to-live. An expired item is removed and not returned.
    pub fn get(&mut self, key: &str, now: u64) -> Option<&V> {
        let i = self.items.iter().position(|e| e.key == key)?;
        if self.items[i].expired(now) {
            self.items.remove(i);
            return None;
        }
        self.items[i].last_access = now;
        Some(&self.items[i].value)
    }

    /// Remove an item; `true` if it was present.
    pub fn remove(&mut self, key: &str) -> bool {
        match self.items.iter().position(|e| e.key == key) {
            Some(i) => {
                self.items.remove(i);
                true
            }
            None => false,
        }
    }

    /// Keys of live items, most important first (ties: most recently used first).
    pub fn keys_by_priority(&self, now: u64) -> Vec<&str> {
        let mut live: Vec<&Item<V>> = self.items.iter().filter(|i| !i.expired(now)).collect();
        live.sort_by(|a, b| {
            b.priority
                .total_cmp(&a.priority)
                .then(b.last_access.cmp(&a.last_access))
        });
        live.into_iter().map(|i| i.key.as_str()).collect()
    }

    /// Number of stored items (expired ones count until swept or read).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` if nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Remove everything.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_must_be_positive_and_priority_finite() {
        assert_eq!(WorkingMemory::<u8>::new(0).unwrap_err(), CogError::BadValue);
        let mut w = WorkingMemory::new(2).unwrap();
        assert_eq!(w.put("a", 1, f32::NAN, None, 0), Err(CogError::BadValue));
        assert_eq!(
            w.put("a", 1, f32::INFINITY, None, 0),
            Err(CogError::BadValue)
        );
        assert!(w.is_empty());
    }

    #[test]
    fn stores_reads_replaces_and_removes() {
        let mut w = WorkingMemory::new(3).unwrap();
        assert_eq!(
            w.put("goal", "find keys", 0.9, None, 0).unwrap(),
            Put::Stored
        );
        assert_eq!(w.get("goal", 1), Some(&"find keys"));
        assert_eq!(
            w.put("goal", "find phone", 0.9, None, 2).unwrap(),
            Put::Stored
        );
        assert_eq!(w.len(), 1);
        assert_eq!(w.get("goal", 3), Some(&"find phone"));
        assert!(w.remove("goal"));
        assert!(!w.remove("goal"));
        assert_eq!(w.get("goal", 4), None);
    }

    #[test]
    fn full_memory_evicts_the_least_important_item() {
        let mut w = WorkingMemory::new(2).unwrap();
        w.put("low", 1, 0.1, None, 0).unwrap();
        w.put("mid", 2, 0.5, None, 0).unwrap();
        assert_eq!(
            w.put("high", 3, 0.9, None, 1).unwrap(),
            Put::Evicted("low".into())
        );
        assert_eq!(w.keys_by_priority(1), ["high", "mid"]);
    }

    #[test]
    fn less_important_newcomers_are_refused() {
        let mut w = WorkingMemory::new(2).unwrap();
        w.put("a", 1, 0.6, None, 0).unwrap();
        w.put("b", 2, 0.7, None, 0).unwrap();
        assert_eq!(w.put("c", 3, 0.2, None, 1).unwrap(), Put::Refused);
        assert_eq!(w.len(), 2);
        // equal priority to the weakest item displaces it
        assert_eq!(
            w.put("d", 4, 0.6, None, 2).unwrap(),
            Put::Evicted("a".into())
        );
    }

    #[test]
    fn priority_ties_evict_the_least_recently_used() {
        let mut w = WorkingMemory::new(2).unwrap();
        w.put("old", 1, 0.5, None, 0).unwrap();
        w.put("new", 2, 0.5, None, 5).unwrap();
        w.get("old", 9); // touching "old" makes "new" the least recently used
        assert_eq!(
            w.put("x", 3, 0.5, None, 10).unwrap(),
            Put::Evicted("new".into())
        );
    }

    #[test]
    fn items_expire_after_their_ttl_and_reads_restart_it() {
        let mut w = WorkingMemory::new(4).unwrap();
        w.put("fade", 1, 0.5, Some(10), 0).unwrap();
        w.put("kept", 2, 0.5, Some(10), 0).unwrap();
        w.put("forever", 3, 0.5, None, 0).unwrap();
        assert_eq!(w.get("kept", 8), Some(&2)); // restarts the clock at 8
        assert_eq!(w.get("fade", 10), Some(&1)); // exactly ttl ticks: still alive
        assert_eq!(w.get("fade", 25), None); // idle for 15 ticks
        assert_eq!(w.get("kept", 18), Some(&2)); // 10 ticks since tick 8
        assert_eq!(w.get("kept", 40), None);
        assert_eq!(w.get("forever", 1_000_000), Some(&3));
    }

    #[test]
    fn sweep_and_put_clear_expired_items_first() {
        let mut w = WorkingMemory::new(2).unwrap();
        w.put("a", 1, 0.9, Some(5), 0).unwrap();
        w.put("b", 2, 0.9, Some(5), 0).unwrap();
        // both expired by tick 100: the new item finds room without evicting anything live
        assert_eq!(w.put("c", 3, 0.1, None, 100).unwrap(), Put::Stored);
        assert_eq!(w.len(), 1);
        w.put("d", 4, 0.5, Some(1), 100).unwrap();
        assert_eq!(w.sweep(200), 1);
        assert_eq!(w.keys_by_priority(200), ["c"]);
        w.clear();
        assert!(w.is_empty());
    }

    #[test]
    fn keys_by_priority_hides_expired_items() {
        let mut w = WorkingMemory::new(3).unwrap();
        w.put("a", 1, 0.9, Some(2), 0).unwrap();
        w.put("b", 2, 0.4, None, 0).unwrap();
        w.put("c", 3, 0.4, None, 1).unwrap();
        assert_eq!(w.keys_by_priority(1), ["a", "c", "b"]);
        assert_eq!(w.keys_by_priority(10), ["c", "b"]);
    }
}
