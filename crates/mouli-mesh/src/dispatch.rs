//! Fault-tolerant task dispatch with checkpoints.

use crate::nodes::{NodeId, NodeTable};
use std::collections::BTreeMap;

/// Task id.
pub type TaskId = u64;

/// Task state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Waiting for a node.
    Pending,
    /// Running on a node.
    Running(NodeId),
    /// Finished with a result.
    Done(Vec<u8>),
    /// Gave up after too many attempts.
    Failed,
}

/// A unit of offloaded work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Id.
    pub id: TaskId,
    /// Needs a GPU node.
    pub gpu: bool,
    /// Opaque job description.
    pub payload: Vec<u8>,
    /// Latest checkpoint reported by a worker (resume point).
    pub checkpoint: Option<Vec<u8>>,
    /// Progress in percent at the checkpoint.
    pub progress: u8,
    /// Assignments made so far.
    pub attempts: u32,
    /// Nodes that already failed this task.
    pub failed_on: Vec<NodeId>,
    /// State.
    pub status: Status,
}

/// An assignment to send to a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// Task.
    pub task: TaskId,
    /// Node.
    pub node: NodeId,
    /// Resume from this checkpoint, if any.
    pub resume: Option<Vec<u8>>,
}

/// Dispatcher.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    tasks: BTreeMap<TaskId, Task>,
    next: TaskId,
    /// Attempts before a task is marked failed.
    pub max_attempts: u32,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new(3)
    }
}

impl Dispatcher {
    /// Dispatcher allowing `max_attempts` per task.
    #[must_use]
    pub fn new(max_attempts: u32) -> Self {
        Self {
            tasks: BTreeMap::new(),
            next: 1,
            max_attempts: max_attempts.max(1),
        }
    }

    /// Submit work.
    pub fn submit(&mut self, gpu: bool, payload: Vec<u8>) -> TaskId {
        let id = self.next;
        self.next += 1;
        self.tasks.insert(
            id,
            Task {
                id,
                gpu,
                payload,
                checkpoint: None,
                progress: 0,
                attempts: 0,
                failed_on: Vec::new(),
                status: Status::Pending,
            },
        );
        id
    }

    /// Requeue tasks on dead nodes, then assign pending tasks to live nodes.
    pub fn schedule(&mut self, nodes: &mut NodeTable, now: u64) -> Vec<Assignment> {
        for t in self.tasks.values_mut() {
            if let Status::Running(n) = t.status {
                if !nodes.alive(n, now) {
                    nodes.add_load(n, -1);
                    t.failed_on.push(n);
                    t.status = Status::Pending;
                }
            }
        }
        let mut out = Vec::new();
        for t in self.tasks.values_mut() {
            if t.status != Status::Pending {
                continue;
            }
            if t.attempts >= self.max_attempts {
                t.status = Status::Failed;
                continue;
            }
            let node = nodes
                .pick(t.gpu, now, &t.failed_on)
                .or_else(|| nodes.pick(t.gpu, now, &[]));
            if let Some(n) = node {
                nodes.add_load(n, 1);
                t.attempts += 1;
                t.status = Status::Running(n);
                out.push(Assignment {
                    task: t.id,
                    node: n,
                    resume: t.checkpoint.clone(),
                });
            }
        }
        out
    }

    /// A worker reports progress. Ignored unless `node` owns the task.
    pub fn checkpoint(&mut self, id: TaskId, node: NodeId, progress: u8, state: Vec<u8>) -> bool {
        match self.tasks.get_mut(&id) {
            Some(t) if t.status == Status::Running(node) => {
                t.progress = progress.min(100);
                t.checkpoint = Some(state);
                true
            }
            _ => false,
        }
    }

    /// A worker finished. Ignored unless `node` owns the task.
    pub fn complete(
        &mut self,
        nodes: &mut NodeTable,
        id: TaskId,
        node: NodeId,
        result: Vec<u8>,
    ) -> bool {
        match self.tasks.get_mut(&id) {
            Some(t) if t.status == Status::Running(node) => {
                nodes.add_load(node, -1);
                t.progress = 100;
                t.status = Status::Done(result);
                true
            }
            _ => false,
        }
    }

    /// A worker failed the task; it will be retried elsewhere.
    pub fn fail(&mut self, nodes: &mut NodeTable, id: TaskId, node: NodeId) -> bool {
        match self.tasks.get_mut(&id) {
            Some(t) if t.status == Status::Running(node) => {
                nodes.add_load(node, -1);
                t.failed_on.push(node);
                t.status = Status::Pending;
                true
            }
            _ => false,
        }
    }

    /// Look up a task.
    #[must_use]
    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassigns_from_dead_node_with_checkpoint() {
        let mut nodes = NodeTable::new(10);
        nodes.join(1, "laptop", true, 1, 0);
        nodes.join(2, "desktop", true, 1, 0);
        let mut d = Dispatcher::new(3);
        let t = d.submit(true, b"render".to_vec());
        let a = d.schedule(&mut nodes, 0);
        assert_eq!(a.len(), 1);
        let first = a[0].node;
        assert!(d.checkpoint(t, first, 40, b"frame-40".to_vec()));
        // First node stops sending heartbeats; the other keeps going.
        let other = if first == 1 { 2 } else { 1 };
        nodes.heartbeat(other, 20);
        let a = d.schedule(&mut nodes, 20);
        assert_eq!(a[0].node, other);
        assert_eq!(a[0].resume.as_deref(), Some(&b"frame-40"[..]));
        // A stale report from the dead node is ignored.
        assert!(!d.complete(&mut nodes, t, first, b"late".to_vec()));
        assert!(d.complete(&mut nodes, t, other, b"ok".to_vec()));
        assert_eq!(d.get(t).unwrap().status, Status::Done(b"ok".to_vec()));
        assert_eq!(nodes.get(other).unwrap().load, 0);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let mut nodes = NodeTable::new(100);
        nodes.join(1, "phone", false, 1, 0);
        let mut d = Dispatcher::new(2);
        let t = d.submit(false, vec![]);
        for _ in 0..2 {
            let a = d.schedule(&mut nodes, 0);
            assert!(d.fail(&mut nodes, t, a[0].node));
        }
        assert!(d.schedule(&mut nodes, 0).is_empty());
        assert_eq!(d.get(t).unwrap().status, Status::Failed);
        // GPU work waits when no GPU node exists.
        let g = d.submit(true, vec![]);
        assert!(d.schedule(&mut nodes, 0).is_empty());
        assert_eq!(d.get(g).unwrap().status, Status::Pending);
    }
}
