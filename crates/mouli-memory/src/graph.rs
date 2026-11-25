//! Knowledge graph with spreading activation.
//!
//! Seeds (concepts named in a query) receive activation; each step every
//! node passes `tanh(activation) * weight` along its outgoing edges and all
//! activations decay. After a few steps the most active nodes are the
//! concepts most strongly connected to the seeds.

use std::collections::{BTreeMap, HashMap};

/// Node id.
pub type NodeId = usize;

/// A weighted, labelled edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Source.
    pub from: NodeId,
    /// Target.
    pub to: NodeId,
    /// Relation label ("causes", "part-of", ...).
    pub relation: String,
    /// Strength (0..=1 recommended).
    pub weight: f32,
}

/// The graph and its activation state.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeGraph {
    labels: Vec<String>,
    by_label: HashMap<String, NodeId>,
    edges: Vec<Edge>,
    activation: Vec<f32>,
    /// Retention per step (0..=1).
    pub decay: f32,
}

impl KnowledgeGraph {
    /// Empty graph with decay 0.9.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decay: 0.9,
            ..Self::default()
        }
    }

    /// Node for `label`, creating it if needed.
    pub fn node(&mut self, label: &str) -> NodeId {
        let key = label.trim().to_lowercase();
        if let Some(&id) = self.by_label.get(&key) {
            return id;
        }
        let id = self.labels.len();
        self.labels.push(label.trim().to_owned());
        self.by_label.insert(key, id);
        self.activation.push(0.0);
        id
    }

    /// Look up a node by label (case-insensitive).
    #[must_use]
    pub fn find(&self, label: &str) -> Option<NodeId> {
        self.by_label.get(&label.trim().to_lowercase()).copied()
    }

    /// Label of a node.
    #[must_use]
    pub fn label(&self, id: NodeId) -> Option<&str> {
        self.labels.get(id).map(String::as_str)
    }

    /// Add `from -[relation]-> to` with `weight`; with `both`, also the reverse.
    pub fn relate(&mut self, from: &str, relation: &str, to: &str, weight: f32, both: bool) {
        let (a, b) = (self.node(from), self.node(to));
        let w = if weight.is_finite() { weight } else { 0.0 };
        self.edges.push(Edge {
            from: a,
            to: b,
            relation: relation.to_owned(),
            weight: w,
        });
        if both {
            self.edges.push(Edge {
                from: b,
                to: a,
                relation: relation.to_owned(),
                weight: w,
            });
        }
    }

    /// Clear all activation.
    pub fn reset(&mut self) {
        self.activation.iter_mut().for_each(|a| *a = 0.0);
    }

    /// Add activation to a node.
    pub fn stimulate(&mut self, id: NodeId, amount: f32) {
        if let Some(a) = self.activation.get_mut(id) {
            if amount.is_finite() {
                *a += amount;
            }
        }
    }

    /// One spreading step.
    pub fn step(&mut self) {
        let mut inflow = vec![0.0f32; self.activation.len()];
        for e in &self.edges {
            inflow[e.to] += self.activation[e.from].tanh() * e.weight;
        }
        for (a, i) in self.activation.iter_mut().zip(inflow) {
            *a = (*a + i) * self.decay;
        }
    }

    /// Seed with `seeds`, spread `steps` times, return the top `k`
    /// non-seed nodes as `(id, activation)`.
    pub fn associate(&mut self, seeds: &[NodeId], steps: usize, k: usize) -> Vec<(NodeId, f32)> {
        self.reset();
        for &s in seeds {
            self.stimulate(s, 1.0);
        }
        for _ in 0..steps {
            self.step();
        }
        let mut v: Vec<(NodeId, f32)> = self
            .activation
            .iter()
            .copied()
            .enumerate()
            .filter(|(i, a)| *a > 1e-6 && !seeds.contains(i))
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(k);
        v
    }

    /// Outgoing relations of a node, grouped by relation label.
    #[must_use]
    pub fn neighbours(&self, id: NodeId) -> BTreeMap<&str, Vec<NodeId>> {
        let mut m: BTreeMap<&str, Vec<NodeId>> = BTreeMap::new();
        for e in self.edges.iter().filter(|e| e.from == id) {
            m.entry(e.relation.as_str()).or_default().push(e.to);
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_prefers_close_strong_links() {
        let mut g = KnowledgeGraph::new();
        g.relate("Sun", "pulls", "Earth", 0.9, true);
        g.relate("Earth", "has", "Moon", 0.8, true);
        g.relate("Sun", "is-a", "Star", 0.3, true);
        g.relate("Pasta", "is-a", "Food", 1.0, true);
        let sun = g.find("sun").unwrap();
        let top = g.associate(&[sun], 3, 10);
        let labels: Vec<&str> = top.iter().map(|(i, _)| g.label(*i).unwrap()).collect();
        assert_eq!(labels[0], "Earth");
        assert!(labels.contains(&"Moon"));
        assert!(!labels.contains(&"Pasta"));
        assert_eq!(g.neighbours(sun)["pulls"], vec![g.find("Earth").unwrap()]);
        assert_eq!(g.node("EARTH"), g.find("earth").unwrap());
    }
}
