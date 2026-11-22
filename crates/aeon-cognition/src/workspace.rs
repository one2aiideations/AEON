//! Global workspace: a small set of candidate contents competing by salience.

use crate::{non_negative, CogError};

/// Bounded salience-ranked workspace. When full, a new item replaces the
/// least salient one if it is more salient.
#[derive(Debug, Clone)]
pub struct Workspace {
    items: Vec<(u64, f32)>,
    cap: usize,
}

impl Workspace {
    /// Workspace with `cap` slots.
    pub fn new(cap: usize) -> Result<Self, CogError> {
        if cap == 0 {
            return Err(CogError::BadValue);
        }
        Ok(Self {
            items: Vec::with_capacity(cap),
            cap,
        })
    }

    /// Offer `id` with `salience`. Returns whether it entered.
    pub fn publish(&mut self, id: u64, salience: f32) -> Result<bool, CogError> {
        let s = non_negative(salience)?;
        if self.items.len() < self.cap {
            self.items.push((id, s));
            return Ok(true);
        }
        let (low, low_s) = self
            .items
            .iter()
            .enumerate()
            .min_by(|a, b| a.1 .1.total_cmp(&b.1 .1))
            .map(|(i, x)| (i, x.1))
            .ok_or(CogError::Empty)?;
        if s > low_s {
            self.items[low] = (id, s);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// The most salient item (the current broadcast).
    pub fn select(&self) -> Result<(u64, f32), CogError> {
        self.items
            .iter()
            .copied()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .ok_or(CogError::Empty)
    }

    /// Number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn competition() {
        let mut w = Workspace::new(2).unwrap();
        assert_eq!(w.select(), Err(CogError::Empty));
        w.publish(1, 0.2).unwrap();
        w.publish(2, 0.9).unwrap();
        assert!(!w.publish(3, 0.1).unwrap());
        assert!(w.publish(4, 0.5).unwrap());
        assert_eq!(w.select().unwrap().0, 2);
        assert_eq!(w.len(), 2);
        assert!(w.publish(5, -1.0).is_err());
    }
}
