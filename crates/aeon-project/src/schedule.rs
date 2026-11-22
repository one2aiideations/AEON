//! Critical-path scheduling and kanban grouping.

use crate::model::Task;
use crate::ProjectError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Kanban columns in workflow order.
pub const KANBAN_COLUMNS: [&str; 5] = ["Backlog", "Todo", "In Progress", "Review", "Done"];

/// Column after `status`, if any.
#[must_use]
pub fn next_status(status: &str) -> Option<&'static str> {
    let i = KANBAN_COLUMNS.iter().position(|c| *c == status)?;
    KANBAN_COLUMNS.get(i + 1).copied()
}

/// Tasks per kanban column, in column order. Unknown statuses go to Backlog.
#[must_use]
pub fn kanban_groups(tasks: &[Task]) -> Vec<(String, Vec<Task>)> {
    KANBAN_COLUMNS
        .iter()
        .map(|col| {
            let in_col = tasks
                .iter()
                .filter(|t| {
                    t.status == *col || (*col == KANBAN_COLUMNS[0] && !KANBAN_COLUMNS.contains(&t.status.as_str()))
                })
                .cloned()
                .collect();
            ((*col).to_owned(), in_col)
        })
        .collect()
}

/// CPM result for one task (times in hours from project start).
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScheduleResult {
    /// Task id.
    pub task_id: String,
    /// Earliest start.
    pub earliest_start: f64,
    /// Earliest finish.
    pub earliest_finish: f64,
    /// Latest start without delaying the project.
    pub latest_start: f64,
    /// Latest finish without delaying the project.
    pub latest_finish: f64,
    /// Slack.
    pub total_float: f64,
    /// On the critical path (zero float).
    pub critical: bool,
}

/// Critical-path method over `tasks` (dependencies outside the slice are an error).
/// Results are in the same order as `tasks`.
pub fn critical_path(tasks: &[Task]) -> Result<Vec<ScheduleResult>, ProjectError> {
    let index: HashMap<&str, usize> = tasks.iter().enumerate().map(|(i, t)| (t.id.as_str(), i)).collect();
    let n = tasks.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, t) in tasks.iter().enumerate() {
        for d in &t.dependencies {
            let j = *index
                .get(d.as_str())
                .ok_or_else(|| ProjectError::NotFound(format!("dependency {d} of {}", t.id)))?;
            if !preds[i].contains(&j) {
                preds[i].push(j);
                succs[j].push(i);
            }
        }
    }
    // Kahn's algorithm for a topological order.
    let mut indeg: Vec<usize> = preds.iter().map(Vec::len).collect();
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = queue.pop_front() {
        order.push(i);
        for &s in &succs[i] {
            indeg[s] -= 1;
            if indeg[s] == 0 {
                queue.push_back(s);
            }
        }
    }
    if order.len() != n {
        return Err(ProjectError::Cycle);
    }
    let dur = |i: usize| tasks[i].estimated_hours.max(0.0);
    let mut es = vec![0.0f64; n];
    let mut ef = vec![0.0f64; n];
    for &i in &order {
        es[i] = preds[i].iter().map(|&p| ef[p]).fold(0.0, f64::max);
        ef[i] = es[i] + dur(i);
    }
    let project_end = ef.iter().copied().fold(0.0, f64::max);
    let mut lf = vec![project_end; n];
    let mut ls = vec![0.0f64; n];
    for &i in order.iter().rev() {
        if !succs[i].is_empty() {
            lf[i] = succs[i].iter().map(|&s| ls[s]).fold(f64::INFINITY, f64::min);
        }
        ls[i] = lf[i] - dur(i);
    }
    Ok((0..n)
        .map(|i| {
            let float = ls[i] - es[i];
            ScheduleResult {
                task_id: tasks[i].id.clone(),
                earliest_start: es[i],
                earliest_finish: ef[i],
                latest_start: ls[i],
                latest_finish: lf[i],
                total_float: float,
                critical: float.abs() < 1e-6,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::sample_store;

    #[test]
    fn chain_is_fully_critical() {
        let mut s = sample_store();
        let (a, b, c) = (s.tasks[0].id.clone(), s.tasks[1].id.clone(), s.tasks[2].id.clone());
        s.tasks[0].estimated_hours = 4.0;
        s.tasks[1].estimated_hours = 5.0;
        s.tasks[2].estimated_hours = 2.0;
        s.add_dependency(&b, &a).unwrap();
        s.add_dependency(&c, &b).unwrap();
        let r = critical_path(&s.tasks).unwrap();
        assert!((r[2].earliest_finish - 11.0).abs() < 1e-9);
        assert!(r.iter().all(|x| x.critical));
    }

    #[test]
    fn parallel_branch_has_float() {
        let mut s = sample_store();
        let (a, b, c) = (s.tasks[0].id.clone(), s.tasks[1].id.clone(), s.tasks[2].id.clone());
        // A(4) -> C(2), B(1) -> C: B has 3h float.
        s.tasks[0].estimated_hours = 4.0;
        s.tasks[1].estimated_hours = 1.0;
        s.tasks[2].estimated_hours = 2.0;
        s.add_dependency(&c, &a).unwrap();
        s.add_dependency(&c, &b).unwrap();
        let r = critical_path(&s.tasks).unwrap();
        assert!(r[0].critical && r[2].critical);
        assert!(!r[1].critical);
        assert!((r[1].total_float - 3.0).abs() < 1e-9);
        assert!((r[2].earliest_start - 4.0).abs() < 1e-9);
    }

    #[test]
    fn missing_dependency_and_cycle() {
        let mut s = sample_store();
        s.tasks[0].dependencies.push("ghost".into());
        assert!(matches!(critical_path(&s.tasks), Err(ProjectError::NotFound(_))));
        let mut s = sample_store();
        let (a, b) = (s.tasks[0].id.clone(), s.tasks[1].id.clone());
        s.tasks[0].dependencies.push(b);
        s.tasks[1].dependencies.push(a);
        assert_eq!(critical_path(&s.tasks), Err(ProjectError::Cycle));
        assert!(critical_path(&[]).unwrap().is_empty());
    }

    #[test]
    fn kanban() {
        let mut s = sample_store();
        let id = s.tasks[0].id.clone();
        s.update_task_status(&id, "In Progress").unwrap();
        s.tasks[1].status = "legacy".into();
        let g = kanban_groups(&s.tasks);
        assert_eq!(g.iter().find(|(n, _)| n == "In Progress").unwrap().1.len(), 1);
        assert_eq!(g[0].1.len(), 2, "unknown status shown in Backlog");
        assert_eq!(next_status("Review"), Some("Done"));
        assert_eq!(next_status("Done"), None);
    }
}
