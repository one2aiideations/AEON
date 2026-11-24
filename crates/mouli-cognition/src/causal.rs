//! Weighted causal graph with an interventional query.

use crate::{finite, CogError};

/// One cause -> effect edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    /// Caller id.
    pub id: u64,
    /// Cause node.
    pub cause: i32,
    /// Effect node.
    pub effect: i32,
    /// Signed strength (magnitude is the weight).
    pub strength: f32,
    /// Edge participates in queries.
    pub active: bool,
}

/// Bounded causal graph.
#[derive(Debug, Clone)]
pub struct CausalGraph {
    edges: Vec<Edge>,
    max: usize,
}

impl CausalGraph {
    /// Graph holding at most `max` edges.
    #[must_use]
    pub fn new(max: usize) -> Self {
        Self {
            edges: Vec::new(),
            max,
        }
    }

    /// Add an active edge.
    pub fn add(&mut self, id: u64, cause: i32, effect: i32, strength: f32) -> Result<(), CogError> {
        if self.edges.len() >= self.max {
            return Err(CogError::Full);
        }
        self.edges.push(Edge {
            id,
            cause,
            effect,
            strength: finite(strength)?,
            active: true,
        });
        Ok(())
    }

    /// Disable edge `id`. Returns whether it existed.
    pub fn deactivate(&mut self, id: u64) -> bool {
        self.edges
            .iter_mut()
            .filter(|e| e.id == id)
            .fold(false, |_, e| {
                e.active = false;
                true
            })
    }

    /// `P(target | do(intervention))`: share of the intervention's outgoing
    /// weight that reaches `target` directly. 0 when it has no edges.
    #[must_use]
    pub fn do_query(&self, intervention: i32, target: i32) -> f32 {
        let (total, hit) = self
            .edges
            .iter()
            .filter(|e| e.active && e.cause == intervention)
            .fold((0.0f32, 0.0f32), |(t, h), e| {
                let w = e.strength.abs();
                (t + w, if e.effect == target { h + w } else { h })
            });
        if total > 0.0 {
            hit / total
        } else {
            0.0
        }
    }

    /// All edges.
    #[must_use]
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn do_query_shares() {
        let mut g = CausalGraph::new(4);
        g.add(1, 0, 1, 3.0).unwrap();
        g.add(2, 0, 2, -1.0).unwrap();
        assert!((g.do_query(0, 1) - 0.75).abs() < 1e-6);
        assert_eq!(g.do_query(5, 1), 0.0);
        assert!(g.deactivate(1));
        assert!((g.do_query(0, 2) - 1.0).abs() < 1e-6);
        assert!(g.add(3, 0, 0, f32::INFINITY).is_err());
        g.add(3, 0, 0, 1.0).unwrap();
        g.add(4, 0, 0, 1.0).unwrap();
        assert_eq!(g.add(5, 0, 0, 1.0), Err(CogError::Full));
    }
}
