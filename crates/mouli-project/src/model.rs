//! Data model and the consistency rules around it.

use crate::schedule::KANBAN_COLUMNS;
use crate::ProjectError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn today() -> String {
    chrono::Utc::now().date_naive().to_string()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Organisation.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Organization {
    /// Id.
    pub id: String,
    /// Name.
    pub name: String,
}

/// Workspace inside an organisation.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Workspace {
    /// Id.
    pub id: String,
    /// Owning organisation.
    pub organization_id: String,
    /// Name.
    pub name: String,
}

/// A project.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Project {
    /// Id.
    pub id: String,
    /// Workspace.
    pub workspace_id: String,
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Status ("Active", "On Hold", "Done").
    pub status: String,
    /// Priority ("Low", "Medium", "High").
    pub priority: String,
    /// ISO start date.
    pub start_date: String,
    /// ISO due date (may be empty).
    pub due_date: String,
    /// Budget in money units.
    pub budget: f64,
    /// Actual spend in money units.
    pub actual_cost: f64,
    /// Owner.
    pub owner: String,
}

/// A task.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Task {
    /// Id.
    pub id: String,
    /// Project.
    pub project_id: String,
    /// Parent task for subtasks.
    pub parent_id: Option<String>,
    /// Title.
    pub title: String,
    /// Description.
    pub description: String,
    /// Type ("Task", "Bug", "Story").
    pub task_type: String,
    /// One of [`KANBAN_COLUMNS`].
    pub status: String,
    /// Priority.
    pub priority: String,
    /// Assignee.
    pub assignee: String,
    /// ISO start date.
    pub start_date: String,
    /// ISO due date.
    pub due_date: String,
    /// Estimate in hours (used by the critical path).
    pub estimated_hours: f64,
    /// Hours logged.
    pub actual_hours: f64,
    /// Completion 0..=100.
    pub progress: f64,
    /// Story points.
    pub story_points: f64,
    /// Ids of tasks that must finish first (same project).
    pub dependencies: Vec<String>,
    /// Labels.
    pub labels: Vec<String>,
    /// Blocked flag.
    pub blocked: bool,
}

/// Milestone.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Milestone {
    /// Id.
    pub id: String,
    /// Project.
    pub project_id: String,
    /// Name.
    pub name: String,
    /// ISO due date.
    pub due_date: String,
    /// Done.
    pub completed: bool,
}

/// Sprint.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Sprint {
    /// Id.
    pub id: String,
    /// Project.
    pub project_id: String,
    /// Name.
    pub name: String,
    /// ISO start.
    pub start_date: String,
    /// ISO end.
    pub end_date: String,
    /// Sprint goal.
    pub goal: String,
    /// Currently running.
    pub active: bool,
}

/// Project document (Markdown).
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Document {
    /// Id.
    pub id: String,
    /// Project.
    pub project_id: String,
    /// Title.
    pub title: String,
    /// Markdown content.
    pub content: String,
    /// Version, incremented on each edit.
    pub version: u64,
}

/// Objective and key result.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Goal {
    /// Id.
    pub id: String,
    /// Project.
    pub project_id: String,
    /// Objective.
    pub objective: String,
    /// Key result.
    pub key_result: String,
    /// Progress 0..=100.
    pub progress: f64,
}

/// A person or team that can do work.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Resource {
    /// Id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Role.
    pub role: String,
    /// Skills.
    pub skills: Vec<String>,
    /// Weekly capacity.
    pub capacity_hours: f64,
    /// Cost per hour in money units.
    pub hourly_rate: f64,
}

/// Logged work.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct TimeEntry {
    /// Id.
    pub id: String,
    /// Task.
    pub task_id: String,
    /// Who.
    pub user: String,
    /// Hours.
    pub hours: f64,
    /// Billable.
    pub billable: bool,
}

/// Comment on a task.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Comment {
    /// Id.
    pub id: String,
    /// Task.
    pub task_id: String,
    /// Author.
    pub author: String,
    /// Text.
    pub content: String,
    /// RFC 3339 time.
    pub created_at: String,
}

/// Project risk.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Risk {
    /// Id.
    pub id: String,
    /// Project.
    pub project_id: String,
    /// Description.
    pub description: String,
    /// Probability 0..=1.
    pub probability: f64,
    /// Impact 0..=1.
    pub impact: f64,
    /// Mitigation plan.
    pub mitigation: String,
    /// Status.
    pub status: String,
}

impl Risk {
    /// Exposure = probability x impact.
    #[must_use]
    pub fn exposure(&self) -> f64 {
        self.probability.clamp(0.0, 1.0) * self.impact.clamp(0.0, 1.0)
    }
}

/// Audit trail entry.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AuditEntry {
    /// Id.
    pub id: String,
    /// Who.
    pub actor: String,
    /// What.
    pub action: String,
    /// On what.
    pub target: String,
    /// RFC 3339 time.
    pub timestamp: String,
}

/// Automation rule (stored; not executed yet).
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Automation {
    /// Id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Trigger description.
    pub trigger: String,
    /// Action description.
    pub action: String,
    /// Enabled.
    pub enabled: bool,
}

/// All data, kept consistent by the methods below.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Store {
    /// Organisations.
    pub organizations: Vec<Organization>,
    /// Workspaces.
    pub workspaces: Vec<Workspace>,
    /// Projects.
    pub projects: Vec<Project>,
    /// Tasks.
    pub tasks: Vec<Task>,
    /// Milestones.
    pub milestones: Vec<Milestone>,
    /// Sprints.
    pub sprints: Vec<Sprint>,
    /// Documents.
    pub documents: Vec<Document>,
    /// Goals.
    pub goals: Vec<Goal>,
    /// Resources.
    pub resources: Vec<Resource>,
    /// Time entries.
    pub time_entries: Vec<TimeEntry>,
    /// Comments.
    pub comments: Vec<Comment>,
    /// Risks.
    pub risks: Vec<Risk>,
    /// Audit trail (append-only).
    pub audit: Vec<AuditEntry>,
    /// Automations.
    pub automations: Vec<Automation>,
}

fn non_empty(s: &str, what: &str) -> Result<String, ProjectError> {
    let t = s.trim();
    if t.is_empty() {
        Err(ProjectError::Invalid(format!("{what} must not be empty")))
    } else {
        Ok(t.to_owned())
    }
}

impl Store {
    /// Append to the audit trail.
    pub fn audit(&mut self, actor: &str, action: &str, target: &str) {
        self.audit.push(AuditEntry {
            id: new_id(),
            actor: actor.to_owned(),
            action: action.to_owned(),
            target: target.to_owned(),
            timestamp: now(),
        });
    }

    /// Create a project; returns its id.
    pub fn add_project(&mut self, name: &str) -> Result<String, ProjectError> {
        let name = non_empty(name, "project name")?;
        let id = new_id();
        self.projects.push(Project {
            id: id.clone(),
            name: name.clone(),
            status: "Active".into(),
            priority: "Medium".into(),
            start_date: today(),
            ..Project::default()
        });
        self.audit("local-user", "CREATE_PROJECT", &name);
        Ok(id)
    }

    /// Create a task in `project_id`; returns its id.
    pub fn add_task(&mut self, project_id: &str, title: &str) -> Result<String, ProjectError> {
        let title = non_empty(title, "task title")?;
        if !self.projects.iter().any(|p| p.id == project_id) {
            return Err(ProjectError::NotFound(format!("project {project_id}")));
        }
        let id = new_id();
        self.tasks.push(Task {
            id: id.clone(),
            project_id: project_id.to_owned(),
            title: title.clone(),
            status: KANBAN_COLUMNS[0].into(),
            priority: "Medium".into(),
            task_type: "Task".into(),
            start_date: today(),
            ..Task::default()
        });
        self.audit("local-user", "CREATE_TASK", &title);
        Ok(id)
    }

    fn task_mut(&mut self, id: &str) -> Result<&mut Task, ProjectError> {
        self.tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| ProjectError::NotFound(format!("task {id}")))
    }

    /// Move a task to a kanban column. `Done` sets progress to 100.
    pub fn update_task_status(&mut self, id: &str, status: &str) -> Result<(), ProjectError> {
        if !KANBAN_COLUMNS.contains(&status) {
            return Err(ProjectError::Invalid(format!(
                "status {status:?}; expected one of {KANBAN_COLUMNS:?}"
            )));
        }
        let task = self.task_mut(id)?;
        task.status = status.to_owned();
        if status == "Done" {
            task.progress = 100.0;
        }
        self.audit(
            "local-user",
            "UPDATE_TASK_STATUS",
            &format!("{id} -> {status}"),
        );
        Ok(())
    }

    /// Whether `to` is reachable from `from` following dependency edges
    /// (task -> its dependencies).
    fn depends_on(&self, from: &str, to: &str) -> bool {
        let deps: HashMap<&str, &Vec<String>> = self
            .tasks
            .iter()
            .map(|t| (t.id.as_str(), &t.dependencies))
            .collect();
        let mut stack = vec![from];
        let mut seen = HashSet::new();
        while let Some(cur) = stack.pop() {
            if cur == to {
                return true;
            }
            if !seen.insert(cur) {
                continue;
            }
            if let Some(ds) = deps.get(cur) {
                stack.extend(ds.iter().map(String::as_str));
            }
        }
        false
    }

    /// Make `task_id` depend on `dependency_id` (same project, no cycles).
    pub fn add_dependency(
        &mut self,
        task_id: &str,
        dependency_id: &str,
    ) -> Result<(), ProjectError> {
        if task_id == dependency_id {
            return Err(ProjectError::Invalid(
                "a task cannot depend on itself".into(),
            ));
        }
        let project = |id: &str| {
            self.tasks
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.project_id.clone())
                .ok_or_else(|| ProjectError::NotFound(format!("task {id}")))
        };
        if project(task_id)? != project(dependency_id)? {
            return Err(ProjectError::Invalid("cross-project dependency".into()));
        }
        // Adding task -> dep closes a cycle iff dep already reaches task.
        if self.depends_on(dependency_id, task_id) {
            return Err(ProjectError::Cycle);
        }
        let task = self.task_mut(task_id)?;
        if !task.dependencies.iter().any(|d| d == dependency_id) {
            task.dependencies.push(dependency_id.to_owned());
        }
        self.audit(
            "local-user",
            "ADD_DEPENDENCY",
            &format!("{task_id} after {dependency_id}"),
        );
        Ok(())
    }

    /// Log time on a task (adds to its actual hours).
    pub fn log_time(
        &mut self,
        task_id: &str,
        user: &str,
        hours: f64,
        billable: bool,
    ) -> Result<(), ProjectError> {
        if !hours.is_finite() || hours <= 0.0 || hours > 24.0 {
            return Err(ProjectError::Invalid("hours must be in (0, 24]".into()));
        }
        self.task_mut(task_id)?.actual_hours += hours;
        self.time_entries.push(TimeEntry {
            id: new_id(),
            task_id: task_id.to_owned(),
            user: user.to_owned(),
            hours,
            billable,
        });
        self.audit(user, "LOG_TIME", task_id);
        Ok(())
    }

    /// Add or replace a project document (replacing bumps the version).
    pub fn save_document(
        &mut self,
        project_id: &str,
        title: &str,
        content: &str,
    ) -> Result<u64, ProjectError> {
        let title = non_empty(title, "document title")?;
        if !self.projects.iter().any(|p| p.id == project_id) {
            return Err(ProjectError::NotFound(format!("project {project_id}")));
        }
        let version = match self
            .documents
            .iter_mut()
            .find(|d| d.project_id == project_id && d.title == title)
        {
            Some(d) => {
                d.content = content.to_owned();
                d.version += 1;
                d.version
            }
            None => {
                self.documents.push(Document {
                    id: new_id(),
                    project_id: project_id.to_owned(),
                    title: title.clone(),
                    content: content.to_owned(),
                    version: 1,
                });
                1
            }
        };
        self.audit("local-user", "SAVE_DOCUMENT", &title);
        Ok(version)
    }

    /// Tasks of one project.
    #[must_use]
    pub fn project_tasks(&self, project_id: &str) -> Vec<Task> {
        self.tasks
            .iter()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect()
    }

    /// Progress 0..=100, weighted by estimate (equal weights when no estimates).
    #[must_use]
    pub fn calculate_progress(&self, project_id: &str) -> f64 {
        let tasks: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.project_id == project_id)
            .collect();
        if tasks.is_empty() {
            return 0.0;
        }
        let total_est: f64 = tasks.iter().map(|t| t.estimated_hours.max(0.0)).sum();
        if total_est > 0.0 {
            tasks
                .iter()
                .map(|t| t.progress.clamp(0.0, 100.0) * t.estimated_hours.max(0.0))
                .sum::<f64>()
                / total_est
        } else {
            tasks
                .iter()
                .map(|t| t.progress.clamp(0.0, 100.0))
                .sum::<f64>()
                / tasks.len() as f64
        }
    }

    /// Hours logged on a project's tasks.
    #[must_use]
    pub fn effort_hours(&self, project_id: &str) -> f64 {
        self.tasks
            .iter()
            .filter(|t| t.project_id == project_id)
            .map(|t| t.actual_hours)
            .sum()
    }

    /// Money spent: recorded actual cost plus logged hours priced at the
    /// logging user's resource rate (unknown users cost 0).
    #[must_use]
    pub fn calculate_cost(&self, project_id: &str) -> f64 {
        let rates: HashMap<&str, f64> = self
            .resources
            .iter()
            .map(|r| (r.name.as_str(), r.hourly_rate))
            .collect();
        let tasks: HashSet<&str> = self
            .tasks
            .iter()
            .filter(|t| t.project_id == project_id)
            .map(|t| t.id.as_str())
            .collect();
        let labour: f64 = self
            .time_entries
            .iter()
            .filter(|e| tasks.contains(e.task_id.as_str()))
            .map(|e| e.hours * rates.get(e.user.as_str()).copied().unwrap_or(0.0))
            .sum();
        self.projects
            .iter()
            .find(|p| p.id == project_id)
            .map_or(0.0, |p| p.actual_cost)
            + labour
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample_store() -> Store {
        let mut store = Store::default();
        let p = store.add_project("Test Project").unwrap();
        for t in ["Task A", "Task B", "Task C"] {
            store.add_task(&p, t).unwrap();
        }
        store
    }

    #[test]
    fn create_and_validate() {
        let mut s = Store::default();
        let id = s.add_project("MOULI").unwrap();
        assert_eq!(s.projects[0].id, id);
        assert!(s.add_project("   ").is_err());
        assert!(s.add_task("invalid", "Task").is_err());
        assert!(s.add_task(&id, "").is_err());
        assert!(!s.audit.is_empty());
    }

    #[test]
    fn status_rules() {
        let mut s = sample_store();
        let id = s.tasks[0].id.clone();
        s.update_task_status(&id, "In Progress").unwrap();
        assert_eq!(s.tasks[0].status, "In Progress");
        assert!(s.update_task_status(&id, "Whatever").is_err());
        s.update_task_status(&id, "Done").unwrap();
        assert_eq!(s.tasks[0].progress, 100.0);
    }

    #[test]
    fn dependency_rules() {
        let mut s = sample_store();
        let (a, b, c) = (
            s.tasks[0].id.clone(),
            s.tasks[1].id.clone(),
            s.tasks[2].id.clone(),
        );
        s.add_dependency(&b, &a).unwrap();
        s.add_dependency(&b, &a).unwrap();
        assert_eq!(s.tasks[1].dependencies.len(), 1, "no duplicates");
        s.add_dependency(&c, &b).unwrap();
        assert_eq!(
            s.add_dependency(&a, &c),
            Err(ProjectError::Cycle),
            "indirect cycle"
        );
        assert!(s.add_dependency(&a, &a).is_err());
        let other = s.add_project("Other").unwrap();
        let x = s.add_task(&other, "X").unwrap();
        assert!(s.add_dependency(&x, &a).is_err(), "cross-project");
    }

    #[test]
    fn progress_cost_and_documents() {
        let mut s = sample_store();
        let pid = s.projects[0].id.clone();
        s.tasks[0].progress = 100.0;
        s.tasks[1].progress = 50.0;
        assert!((s.calculate_progress(&pid) - 50.0).abs() < 1e-9);
        s.tasks[0].estimated_hours = 3.0;
        s.tasks[1].estimated_hours = 1.0;
        assert!(
            (s.calculate_progress(&pid) - 87.5).abs() < 1e-9,
            "estimate-weighted"
        );

        s.resources.push(Resource {
            name: "asha".into(),
            hourly_rate: 40.0,
            ..Resource::default()
        });
        let t = s.tasks[0].id.clone();
        s.log_time(&t, "asha", 2.0, true).unwrap();
        s.log_time(&t, "guest", 1.0, false).unwrap();
        assert!(s.log_time(&t, "asha", 0.0, true).is_err());
        s.projects[0].actual_cost = 10.0;
        assert!((s.effort_hours(&pid) - 3.0).abs() < 1e-9);
        assert!((s.calculate_cost(&pid) - 90.0).abs() < 1e-9);

        assert_eq!(s.save_document(&pid, "Plan", "v1").unwrap(), 1);
        assert_eq!(s.save_document(&pid, "Plan", "v2").unwrap(), 2);
        assert_eq!(s.documents.len(), 1);
        assert!(s.save_document("nope", "Plan", "x").is_err());

        let r = Risk {
            probability: 0.5,
            impact: 0.4,
            ..Risk::default()
        };
        assert!((r.exposure() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn serialization_tolerates_missing_fields() {
        let s = sample_store();
        let back: Store = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let old: Store = serde_json::from_str(r#"{"projects":[{"id":"p","name":"Old"}]}"#).unwrap();
        assert_eq!(old.projects[0].name, "Old");
    }
}
