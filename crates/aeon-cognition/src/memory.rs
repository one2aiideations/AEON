//! Ring-buffer episodic memory with elastic-weight-consolidation style updates.

use crate::{finite, non_negative, CogError};

/// One remembered value with its importance (Fisher information).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Episode {
    /// Caller id.
    pub id: u64,
    /// Stored value.
    pub value: f32,
    /// Importance; higher resists change during consolidation.
    pub fisher: f32,
}

/// Fixed-capacity episodic memory; the oldest entry is overwritten when full.
#[derive(Debug, Clone)]
pub struct EpisodicMemory {
    items: Vec<Episode>,
    cap: usize,
    cursor: usize,
}

impl EpisodicMemory {
    /// Memory with room for `cap` episodes.
    pub fn new(cap: usize) -> Result<Self, CogError> {
        if cap == 0 {
            return Err(CogError::BadValue);
        }
        Ok(Self {
            items: Vec::with_capacity(cap),
            cap,
            cursor: 0,
        })
    }

    /// Store an episode.
    pub fn record(&mut self, id: u64, value: f32, fisher: f32) -> Result<(), CogError> {
        let e = Episode {
            id,
            value: finite(value)?,
            fisher: non_negative(fisher)?,
        };
        if self.items.len() < self.cap {
            self.items.push(e);
        } else {
            self.items[self.cursor] = e;
        }
        self.cursor = (self.cursor + 1) % self.cap;
        Ok(())
    }

    /// `v += lr * (v - lambda * fisher * v)`: important values change less.
    pub fn consolidate(&mut self, learning_rate: f32, ewc_lambda: f32) -> Result<(), CogError> {
        let lr = non_negative(learning_rate)?;
        let lambda = non_negative(ewc_lambda)?;
        for e in &mut self.items {
            let penalty = lambda * e.fisher * e.value;
            e.value += lr * (e.value - penalty);
        }
        Ok(())
    }

    /// Stored episodes (insertion order until the ring wraps).
    #[must_use]
    pub fn episodes(&self) -> &[Episode] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_overwrites_oldest() {
        let mut m = EpisodicMemory::new(2).unwrap();
        for i in 0..3 {
            m.record(i, i as f32, 0.0).unwrap();
        }
        let ids: Vec<u64> = m.episodes().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![2, 1]);
        assert!(m.record(9, f32::NAN, 0.0).is_err());
    }

    #[test]
    fn consolidation_protects_important() {
        let mut m = EpisodicMemory::new(2).unwrap();
        m.record(0, 1.0, 0.0).unwrap();
        m.record(1, 1.0, 1.0).unwrap();
        m.consolidate(0.5, 1.0).unwrap();
        let v: Vec<f32> = m.episodes().iter().map(|e| e.value).collect();
        assert!((v[0] - 1.5).abs() < 1e-6);
        assert!((v[1] - 1.0).abs() < 1e-6);
    }
}
