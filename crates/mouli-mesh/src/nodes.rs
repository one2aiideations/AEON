//! Mesh membership.

use std::collections::BTreeMap;

/// Node id.
pub type NodeId = u32;

/// A mesh member.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Id.
    pub id: NodeId,
    /// Display name.
    pub name: String,
    /// Has a usable GPU.
    pub gpu: bool,
    /// Tasks currently assigned.
    pub load: u32,
    /// Max concurrent tasks.
    pub slots: u32,
    /// Last heartbeat time.
    pub last_seen: u64,
}

/// Nodes and liveness.
#[derive(Debug, Clone)]
pub struct NodeTable {
    nodes: BTreeMap<NodeId, Node>,
    /// Seconds without a heartbeat before a node counts as down.
    pub timeout: u64,
}

impl NodeTable {
    /// Table with heartbeat timeout `timeout` seconds.
    #[must_use]
    pub fn new(timeout: u64) -> Self {
        Self {
            nodes: BTreeMap::new(),
            timeout,
        }
    }

    /// Add or refresh a node.
    pub fn join(&mut self, id: NodeId, name: &str, gpu: bool, slots: u32, now: u64) {
        let n = self.nodes.entry(id).or_insert_with(|| Node {
            id,
            name: name.to_owned(),
            gpu,
            load: 0,
            slots,
            last_seen: now,
        });
        n.gpu = gpu;
        n.slots = slots;
        n.last_seen = now;
    }

    /// Record a heartbeat. Returns false for unknown nodes.
    pub fn heartbeat(&mut self, id: NodeId, now: u64) -> bool {
        match self.nodes.get_mut(&id) {
            Some(n) => {
                n.last_seen = n.last_seen.max(now);
                true
            }
            None => false,
        }
    }

    /// Node is alive at `now`.
    #[must_use]
    pub fn alive(&self, id: NodeId, now: u64) -> bool {
        self.nodes
            .get(&id)
            .is_some_and(|n| now.saturating_sub(n.last_seen) <= self.timeout)
    }

    /// Ids of nodes that are down at `now`.
    #[must_use]
    pub fn down(&self, now: u64) -> Vec<NodeId> {
        self.nodes
            .keys()
            .copied()
            .filter(|&id| !self.alive(id, now))
            .collect()
    }

    /// Least-loaded live node with a free slot (and a GPU if required).
    #[must_use]
    pub fn pick(&self, need_gpu: bool, now: u64, exclude: &[NodeId]) -> Option<NodeId> {
        self.nodes
            .values()
            .filter(|n| {
                self.alive(n.id, now)
                    && n.load < n.slots
                    && (!need_gpu || n.gpu)
                    && !exclude.contains(&n.id)
            })
            .min_by(|a, b| {
                // Compare load / slots without floats: a.load * b.slots vs b.load * a.slots.
                (u64::from(a.load) * u64::from(b.slots))
                    .cmp(&(u64::from(b.load) * u64::from(a.slots)))
                    .then(a.id.cmp(&b.id))
            })
            .map(|n| n.id)
    }

    /// Change a node's load by `delta`.
    pub fn add_load(&mut self, id: NodeId, delta: i32) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.load = n.load.saturating_add_signed(delta);
        }
    }

    /// Look up a node.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// All live node ids at `now`.
    #[must_use]
    pub fn live(&self, now: u64) -> Vec<NodeId> {
        self.nodes
            .keys()
            .copied()
            .filter(|&id| self.alive(id, now))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_and_pick() {
        let mut t = NodeTable::new(10);
        t.join(1, "laptop", true, 2, 0);
        t.join(2, "phone", false, 4, 0);
        assert_eq!(t.pick(true, 5, &[]), Some(1));
        t.add_load(2, 1);
        assert_eq!(t.pick(false, 5, &[]), Some(1));
        t.add_load(1, 1);
        // laptop 1/2 = 0.5, phone 1/4 = 0.25
        assert_eq!(t.pick(false, 5, &[]), Some(2));
        assert_eq!(t.down(11), vec![1, 2]);
        assert!(t.heartbeat(1, 11));
        assert_eq!(t.down(11), vec![2]);
        assert!(!t.heartbeat(9, 11));
        assert_eq!(t.pick(false, 11, &[1]), None);
    }
}
