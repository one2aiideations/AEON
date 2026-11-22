use eframe::egui;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub assignee: String,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub estimate_hours: f64,
    pub progress: f64,
}

#[derive(Debug, Clone)]
pub struct Dependency {
    pub predecessor: String,
    pub successor: String,
    pub kind: String,
    pub lag_days: i64,
}

#[derive(Debug)]
pub enum AppError {
    Db(rusqlite::Error),
    Invalid(String),
}
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}
pub type AppResult<T> = Result<T, AppError>;

pub struct Store {
    conn: Connection,
}
impl Store {
    pub fn open(path: PathBuf) -> AppResult<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| AppError::Invalid(e.to_string()))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch("
          CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '');
          CREATE TABLE IF NOT EXISTS tasks(
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'Todo',
            priority TEXT NOT NULL DEFAULT 'Normal', assignee TEXT NOT NULL DEFAULT '',
            start_date TEXT, due_date TEXT, estimate_hours REAL NOT NULL DEFAULT 0,
            progress REAL NOT NULL DEFAULT 0 CHECK(progress >= 0 AND progress <= 100));
          CREATE TABLE IF NOT EXISTS dependencies(
            predecessor TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            successor TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            kind TEXT NOT NULL DEFAULT 'FS', lag_days INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(predecessor, successor), CHECK(predecessor <> successor));
          CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id);
          CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
        ")?;
        Ok(Self { conn })
    }
    pub fn ensure_default_project(&self) -> AppResult<Project> {
        let found: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT id,name,description FROM projects ORDER BY rowid LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((id, name, description)) = found {
            return Ok(Project {
                id,
                name,
                description,
            });
        }
        let p = Project {
            id: Uuid::new_v4().to_string(),
            name: "My Workspace".into(),
            description: "Local-first AEON-PROJECT workspace".into(),
        };
        self.conn.execute(
            "INSERT INTO projects(id,name,description) VALUES(?1,?2,?3)",
            params![p.id, p.name, p.description],
        )?;
        Ok(p)
    }
    pub fn projects(&self) -> AppResult<Vec<Project>> {
        let mut s = self
            .conn
            .prepare("SELECT id,name,description FROM projects ORDER BY name")?;
        let rows = s.query_map([], |r| {
            Ok(Project {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
    pub fn create_project(&self, name: &str, description: &str) -> AppResult<Project> {
        if name.trim().is_empty() {
            return Err(AppError::Invalid("Project name is required".into()));
        }
        let p = Project {
            id: Uuid::new_v4().to_string(),
            name: name.trim().into(),
            description: description.into(),
        };
        self.conn.execute(
            "INSERT INTO projects(id,name,description) VALUES(?1,?2,?3)",
            params![p.id, p.name, p.description],
        )?;
        Ok(p)
    }
    pub fn tasks(&self, project: &str) -> AppResult<Vec<Task>> {
        let mut s=self.conn.prepare("SELECT id,project_id,title,description,status,priority,assignee,start_date,due_date,estimate_hours,progress FROM tasks WHERE project_id=?1 ORDER BY COALESCE(due_date,'9999-12-31'),title")?;
        let rows = s.query_map([project], |r| {
            Ok(Task {
                id: r.get(0)?,
                project_id: r.get(1)?,
                title: r.get(2)?,
                description: r.get(3)?,
                status: r.get(4)?,
                priority: r.get(5)?,
                assignee: r.get(6)?,
                start_date: r.get(7)?,
                due_date: r.get(8)?,
                estimate_hours: r.get(9)?,
                progress: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
    pub fn create_task(&self, project: &str, title: &str) -> AppResult<Task> {
        if title.trim().is_empty() {
            return Err(AppError::Invalid("Task title is required".into()));
        }
        let t = Task {
            id: Uuid::new_v4().to_string(),
            project_id: project.into(),
            title: title.trim().into(),
            description: String::new(),
            status: "Todo".into(),
            priority: "Normal".into(),
            assignee: String::new(),
            start_date: None,
            due_date: None,
            estimate_hours: 0.0,
            progress: 0.0,
        };
        self.conn.execute("INSERT INTO tasks(id,project_id,title,description,status,priority,assignee,start_date,due_date,estimate_hours,progress) VALUES(?1,?2,?3,'','Todo','Normal','',NULL,NULL,0,0)",params![t.id,t.project_id,t.title])?;
        Ok(t)
    }
    pub fn update_status(&self, id: &str, status: &str) -> AppResult<()> {
        if !["Todo", "In Progress", "Blocked", "Done"].contains(&status) {
            return Err(AppError::Invalid("Unknown task status".into()));
        }
        self.conn.execute("UPDATE tasks SET status=?1,progress=CASE WHEN ?1='Done' THEN 100 WHEN ?1='Todo' THEN 0 ELSE progress END WHERE id=?2",params![status,id])?;
        Ok(())
    }
    pub fn add_dependency(
        &self,
        predecessor: &str,
        successor: &str,
        kind: &str,
        lag: i64,
    ) -> AppResult<()> {
        if predecessor == successor {
            return Err(AppError::Invalid("A task cannot depend on itself".into()));
        }
        if !["FS", "SS", "FF", "SF"].contains(&kind) {
            return Err(AppError::Invalid(
                "Dependency must be FS, SS, FF or SF".into(),
            ));
        }
        let exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE id IN (?1,?2)",
            params![predecessor, successor],
            |r| r.get(0),
        )?;
        if exists != 2 {
            return Err(AppError::Invalid("Both tasks must exist".into()));
        }
        let cycle:i64=self.conn.query_row(
          "WITH RECURSIVE reach(id) AS (SELECT successor FROM dependencies WHERE predecessor=?1 UNION SELECT d.successor FROM dependencies d JOIN reach r ON d.predecessor=r.id) SELECT COUNT(*) FROM reach WHERE id=?2",
          params![successor,predecessor],|r|r.get(0))?;
        if cycle > 0 {
            return Err(AppError::Invalid("Dependency would create a cycle".into()));
        }
        self.conn.execute(
            "INSERT INTO dependencies(predecessor,successor,kind,lag_days) VALUES(?1,?2,?3,?4)",
            params![predecessor, successor, kind, lag],
        )?;
        Ok(())
    }
    pub fn dependencies(&self, project: &str) -> AppResult<Vec<Dependency>> {
        let mut s=self.conn.prepare("SELECT d.predecessor,d.successor,d.kind,d.lag_days FROM dependencies d JOIN tasks a ON a.id=d.predecessor WHERE a.project_id=?1")?;
        let rows = s.query_map([project], |r| {
            Ok(Dependency {
                predecessor: r.get(0)?,
                successor: r.get(1)?,
                kind: r.get(2)?,
                lag_days: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[derive(PartialEq)]
enum View {
    Home,
    Board,
    Timeline,
    Projects,
}
struct AeonApp {
    store: Arc<Mutex<Store>>,
    project: Project,
    projects: Vec<Project>,
    tasks: Vec<Task>,
    view: View,
    new_task: String,
    new_project: String,
    error: String,
    notice: String,
}
impl AeonApp {
    fn refresh(&mut self) {
        if let Ok(s) = self.store.lock() {
            self.projects = s.projects().unwrap_or_default();
            self.tasks = s.tasks(&self.project.id).unwrap_or_default();
        }
    }
    fn new(store: Store) -> Self {
        let project = store
            .ensure_default_project()
            .expect("initialize workspace");
        let mut app = Self {
            store: Arc::new(Mutex::new(store)),
            project,
            projects: vec![],
            tasks: vec![],
            view: View::Home,
            new_task: String::new(),
            new_project: String::new(),
            error: String::new(),
            notice: String::new(),
        };
        app.refresh();
        app
    }
    fn set_status(&mut self, id: &str, status: &str) {
        match self
            .store
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())
            .and_then(|s| s.update_status(id, status).map_err(|e| format!("{e:?}")))
        {
            Ok(()) => {
                self.error.clear();
                self.refresh();
            }
            Err(e) => self.error = e,
        }
    }
}
impl eframe::App for AeonApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("AEON-PROJECT");
                ui.separator();
                ui.label(&self.project.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("Local-first • SQLite");
                });
            });
        });
        egui::SidePanel::left("navigation")
            .resizable(false)
            .default_width(150.0)
            .show(ctx, |ui| {
                ui.heading("Workspace");
                for (label, view) in [
                    ("Home", View::Home),
                    ("Kanban", View::Board),
                    ("Timeline", View::Timeline),
                    ("Projects", View::Projects),
                ] {
                    if ui.selectable_label(self.view == view, label).clicked() {
                        self.view = view;
                    }
                }
                ui.separator();
                ui.label(format!("{} tasks", self.tasks.len()));
            });
        egui::CentralPanel::default().show(ctx,|ui|{
            if !self.error.is_empty(){ui.colored_label(egui::Color32::LIGHT_RED,&self.error);}
            if !self.notice.is_empty(){ui.label(&self.notice);}
            match self.view {
                View::Home=>{
                    ui.heading("Project overview");
                    let done=self.tasks.iter().filter(|t|t.status=="Done").count();
                    let blocked=self.tasks.iter().filter(|t|t.status=="Blocked").count();
                    ui.horizontal(|ui|{ui.label(format!("Tasks: {}",self.tasks.len()));ui.separator();ui.label(format!("Completed: {done}"));ui.separator();ui.label(format!("Blocked: {blocked}"));});
                    ui.add_space(12.0);ui.heading("Quick capture");
                    ui.horizontal(|ui|{
                        ui.text_edit_singleline(&mut self.new_task);
                        if ui.button("Add task").clicked(){
                            let title=self.new_task.trim().to_string();
                            let store=self.store.clone();let guard=store.lock().ok();if let Some(s)=guard{let result=s.create_task(&self.project.id,&title);drop(s);match result{Ok(_)=>{self.new_task.clear();self.refresh();self.error.clear();},Err(e)=>self.error=format!("{e:?}")}}
                        }
                    });
                    ui.separator();ui.heading("Recently updated work");
                    for t in self.tasks.iter().take(8){ui.label(format!("{}  ·  {}  ·  {}",t.title,t.status,t.priority));}
                }
                View::Board=>{
                    ui.heading("Kanban board");
                    ui.horizontal_wrapped(|ui|{
                        for status in ["Todo","In Progress","Blocked","Done"]{
                            let cards: Vec<Task> = self.tasks.iter().filter(|t|t.status==status).cloned().collect();
                            ui.group(|ui|{
                                ui.heading(status);
                                for t in &cards{
                                    ui.label(&t.title);
                                    ui.horizontal(|ui|{
                                        if status!="Todo" && ui.small_button("←").clicked(){self.set_status(&t.id,"Todo");}
                                        if status!="Done" && ui.small_button("→").clicked(){let next=if status=="Todo"{"In Progress"}else{"Done"};self.set_status(&t.id,next);}
                                    });
                                    ui.separator();
                                }
                            });
                        }
                    });
                }
                View::Timeline=>{
                    ui.heading("Task timeline");
                    ui.label("Date-aware task register (Gantt scheduling and critical-path analysis are not yet implemented).");
                    egui::Grid::new("timeline").striped(true).show(ui,|ui|{
                        ui.strong("Task");ui.strong("Status");ui.strong("Start");ui.strong("Due");ui.end_row();
                        for t in &self.tasks{ui.label(&t.title);ui.label(&t.status);ui.label(t.start_date.as_deref().unwrap_or("—"));ui.label(t.due_date.as_deref().unwrap_or("—"));ui.end_row();}
                    });
                }
                View::Projects=>{
                    ui.heading("Projects");
                    ui.horizontal(|ui|{
                        ui.text_edit_singleline(&mut self.new_project);
                        if ui.button("Create project").clicked(){
                            let store=self.store.clone();let guard=store.lock().ok();if let Some(s)=guard{let result=s.create_project(&self.new_project,"");drop(s);match result{Ok(p)=>{self.new_project.clear();self.project=p;self.refresh();},Err(e)=>self.error=format!("{e:?}")}}
                        }
                    });
                    for p in self.projects.clone(){
                        if ui.selectable_label(p.id==self.project.id,&p.name).clicked(){self.project=p;self.refresh();}
                    }
                }
            }
        });
    }
}

fn database_path() -> PathBuf {
    std::env::var_os("AEON_PROJECT_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            p.push("data");
            p.push("aeon-project.sqlite3");
            p
        })
}
fn main() -> eframe::Result<()> {
    let store = Store::open(database_path()).expect("open AEON-PROJECT database");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([800.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "AEON-PROJECT",
        options,
        Box::new(move |_cc| Ok(Box::new(AeonApp::new(store)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> Store {
        Store::open(PathBuf::from(":memory:")).unwrap()
    }
    #[test]
    fn creates_workspace_and_tasks() {
        let s = store();
        let p = s.ensure_default_project().unwrap();
        let t = s.create_task(&p.id, "Design data model").unwrap();
        assert_eq!(s.tasks(&p.id).unwrap().len(), 1);
        assert_eq!(t.status, "Todo");
    }
    #[test]
    fn updates_task_status() {
        let s = store();
        let p = s.ensure_default_project().unwrap();
        let t = s.create_task(&p.id, "Ship").unwrap();
        s.update_status(&t.id, "Done").unwrap();
        assert_eq!(s.tasks(&p.id).unwrap()[0].progress, 100.0);
    }
    #[test]
    fn rejects_dependency_cycles() {
        let s = store();
        let p = s.ensure_default_project().unwrap();
        let a = s.create_task(&p.id, "A").unwrap();
        let b = s.create_task(&p.id, "B").unwrap();
        s.add_dependency(&a.id, &b.id, "FS", 0).unwrap();
        assert!(s.add_dependency(&b.id, &a.id, "FS", 0).is_err());
    }
    #[test]
    fn rejects_self_dependency() {
        let s = store();
        let p = s.ensure_default_project().unwrap();
        let a = s.create_task(&p.id, "A").unwrap();
        assert!(s.add_dependency(&a.id, &a.id, "FS", 0).is_err());
    }
}
