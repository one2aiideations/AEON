//! Native desktop UI (feature `gui`). Every change goes through the same
//! validated [`Store`](crate::model::Store) methods as the API and is saved
//! immediately.

use crate::ai::{self, AiConfig};
use crate::api::{self, AppState};
use crate::model::{Store, Task};
use crate::schedule::{critical_path, kanban_groups, next_status};
use crate::{ProjectError, APP_NAME};
use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    Projects,
    Tasks,
    Kanban,
    Gantt,
    Documents,
    Resources,
    Reports,
    Ai,
    Administration,
}

struct App {
    state: AppState,
    runtime: tokio::runtime::Runtime,
    view: View,
    selected_project: Option<String>,
    project_name: String,
    task_name: String,
    document_title: String,
    document_content: String,
    ai_prompt: String,
    ai_answer: String,
    ai_config: AiConfig,
    ai_remote_consent: bool,
    status: String,
    dark_mode: bool,
}

impl App {
    fn snapshot(&self) -> Store {
        self.state
            .store
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Apply a change and save; reports the outcome in the status bar.
    fn change(&mut self, f: impl FnOnce(&mut Store) -> Result<(), ProjectError>) {
        let result = (|| -> Result<(), ProjectError> {
            let mut store = self
                .state
                .store
                .lock()
                .map_err(|_| ProjectError::Storage("lock poisoned".into()))?;
            let before = store.clone();
            f(&mut store)?;
            let db = self
                .state
                .db
                .lock()
                .map_err(|_| ProjectError::Storage("lock poisoned".into()))?;
            if let Err(e) = db.save(&store) {
                *store = before;
                return Err(e);
            }
            Ok(())
        })();
        self.status = match result {
            Ok(()) => "Saved".into(),
            Err(e) => e.to_string(),
        };
    }

    fn selected_tasks(&self) -> Vec<Task> {
        match &self.selected_project {
            Some(id) => self.snapshot().project_tasks(id),
            None => Vec::new(),
        }
    }

    fn heading(ui: &mut egui::Ui, title: &str) {
        ui.add_space(8.0);
        ui.heading(title);
        ui.separator();
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Overview");
        let s = self.snapshot();
        let done = s.tasks.iter().filter(|t| t.status == "Done").count();
        let blocked = s.tasks.iter().filter(|t| t.blocked).count();
        ui.columns(4, |c| {
            for (col, (label, n)) in c.iter_mut().zip([
                ("Projects", s.projects.len()),
                ("Tasks", s.tasks.len()),
                ("Completed", done),
                ("Blocked", blocked),
            ]) {
                col.group(|ui| {
                    ui.label(label);
                    ui.heading(n.to_string());
                });
            }
        });
        ui.add_space(16.0);
        for p in &s.projects {
            ui.horizontal(|ui| {
                ui.label(&p.name);
                ui.label(&p.status);
                ui.add(
                    egui::ProgressBar::new((s.calculate_progress(&p.id) / 100.0) as f32)
                        .desired_width(160.0),
                );
            });
        }
    }

    fn projects(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Projects");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.project_name);
            if ui.button("+ New Project").clicked() {
                let name = std::mem::take(&mut self.project_name);
                let mut created = None;
                self.change(|s| {
                    created = Some(s.add_project(&name)?);
                    Ok(())
                });
                if created.is_some() {
                    self.selected_project = created;
                }
            }
        });
        let s = self.snapshot();
        egui::Grid::new("projects").striped(true).show(ui, |ui| {
            for h in ["Project", "Status", "Priority", "Progress"] {
                ui.strong(h);
            }
            ui.end_row();
            for p in &s.projects {
                let selected = self.selected_project.as_deref() == Some(p.id.as_str());
                if ui.selectable_label(selected, &p.name).clicked() {
                    self.selected_project = Some(p.id.clone());
                }
                ui.label(&p.status);
                ui.label(&p.priority);
                ui.add(
                    egui::ProgressBar::new((s.calculate_progress(&p.id) / 100.0) as f32)
                        .desired_width(100.0),
                );
                ui.end_row();
            }
        });
    }

    fn tasks(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Tasks");
        let Some(pid) = self.selected_project.clone() else {
            ui.label("Select a project first.");
            return;
        };
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.task_name);
            if ui.button("+ Create Task").clicked() {
                let title = std::mem::take(&mut self.task_name);
                self.change(|s| s.add_task(&pid, &title).map(|_| ()));
            }
        });
        egui::Grid::new("tasks").striped(true).show(ui, |ui| {
            for h in ["Task", "Type", "Status", "Priority", "Assignee"] {
                ui.strong(h);
            }
            ui.end_row();
            for t in self.selected_tasks() {
                ui.label(&t.title);
                ui.label(&t.task_type);
                ui.label(&t.status);
                ui.label(&t.priority);
                ui.label(&t.assignee);
                ui.end_row();
            }
        });
    }

    fn kanban(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Kanban");
        if self.selected_project.is_none() {
            ui.label("Select a project first.");
            return;
        }
        let mut advance: Option<(String, &'static str)> = None;
        ui.horizontal_top(|ui| {
            for (column, tasks) in kanban_groups(&self.selected_tasks()) {
                ui.group(|ui| {
                    ui.set_min_width(150.0);
                    ui.heading(&column);
                    ui.label(format!("{} tasks", tasks.len()));
                    for t in tasks {
                        ui.group(|ui| {
                            ui.label(&t.title);
                            ui.small(&t.priority);
                            if t.blocked {
                                ui.colored_label(egui::Color32::RED, "BLOCKED");
                            }
                            if let Some(next) = next_status(&column) {
                                if ui.small_button(format!("Move to {next}")).clicked() {
                                    advance = Some((t.id.clone(), next));
                                }
                            }
                        });
                    }
                });
            }
        });
        if let Some((id, next)) = advance {
            self.change(|s| s.update_task_status(&id, next));
        }
    }

    fn gantt(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Gantt & Critical Path");
        let tasks = self.selected_tasks();
        match critical_path(&tasks) {
            Ok(results) => {
                let span = results
                    .iter()
                    .map(|r| r.earliest_finish)
                    .fold(1.0_f64, f64::max);
                let scale = 300.0 / span;
                for (r, t) in results.iter().zip(&tasks) {
                    ui.horizontal(|ui| {
                        ui.add_sized([180.0, 18.0], egui::Label::new(&t.title));
                        ui.add_space((r.earliest_start * scale) as f32);
                        let width =
                            (((r.earliest_finish - r.earliest_start) * scale) as f32).max(5.0);
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(width, 15.0), egui::Sense::hover());
                        let color = if r.critical {
                            egui::Color32::RED
                        } else {
                            egui::Color32::from_rgb(60, 130, 190)
                        };
                        ui.painter().rect_filled(rect, 3.0, color);
                        ui.label(if r.critical {
                            "critical".to_owned()
                        } else {
                            format!("float {:.1}h", r.total_float)
                        });
                    });
                }
            }
            Err(e) => {
                ui.colored_label(egui::Color32::RED, e.to_string());
            }
        }
    }

    fn documents(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Documents");
        ui.label("Title");
        ui.text_edit_singleline(&mut self.document_title);
        ui.label("Markdown");
        ui.add(
            egui::TextEdit::multiline(&mut self.document_content)
                .desired_rows(12)
                .desired_width(f32::INFINITY),
        );
        if ui.button("Save Document").clicked() {
            if let Some(pid) = self.selected_project.clone() {
                let (title, content) = (self.document_title.clone(), self.document_content.clone());
                self.change(|s| s.save_document(&pid, &title, &content).map(|_| ()));
            } else {
                self.status = "Select a project first.".into();
            }
        }
        ui.separator();
        let s = self.snapshot();
        for d in s
            .documents
            .iter()
            .filter(|d| self.selected_project.as_deref() == Some(d.project_id.as_str()))
        {
            ui.collapsing(format!("{} (v{})", d.title, d.version), |ui| {
                ui.label(&d.content);
            });
        }
    }

    fn resources(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Resources");
        for r in &self.snapshot().resources {
            ui.group(|ui| {
                ui.heading(&r.name);
                ui.label(&r.role);
                ui.label(format!(
                    "Capacity: {} h/week, rate {}",
                    r.capacity_hours, r.hourly_rate
                ));
                ui.label(format!("Skills: {}", r.skills.join(", ")));
            });
        }
    }

    fn reports(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Reports");
        let s = self.snapshot();
        for p in &s.projects {
            ui.group(|ui| {
                ui.heading(&p.name);
                ui.label(format!("Progress: {:.1}%", s.calculate_progress(&p.id)));
                ui.label(format!("Effort: {:.1} h", s.effort_hours(&p.id)));
                ui.label(format!(
                    "Cost: {:.2} of budget {:.2}",
                    s.calculate_cost(&p.id),
                    p.budget
                ));
            });
        }
    }

    fn ai(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "AI Assistant");
        ui.label("Endpoint (chat-completions URL)");
        ui.text_edit_singleline(&mut self.ai_config.endpoint);
        ui.label("Model");
        ui.text_edit_singleline(&mut self.ai_config.model);
        ui.label("API key");
        ui.add(egui::TextEdit::singleline(&mut self.ai_config.api_key).password(true));
        if !ai::is_local(&self.ai_config.endpoint) {
            ui.checkbox(
                &mut self.ai_remote_consent,
                "Allow sending my prompt to this remote service",
            );
        }
        ui.separator();
        ui.add(egui::TextEdit::multiline(&mut self.ai_prompt).desired_rows(5));
        if ui.button("Ask").clicked() {
            let result = self.runtime.block_on(ai::ask(
                &self.ai_config,
                &self.ai_prompt,
                self.ai_remote_consent,
            ));
            self.ai_answer = result.unwrap_or_else(|e| e.to_string());
        }
        ui.separator();
        ui.label(&self.ai_answer);
    }

    fn administration(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Administration");
        let s = self.snapshot();
        ui.label(format!("Organizations: {}", s.organizations.len()));
        ui.label(format!("Workspaces: {}", s.workspaces.len()));
        ui.label(format!("Audit entries: {}", s.audit.len()));
        if ui.button("Switch theme").clicked() {
            self.dark_mode = !self.dark_mode;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_visuals(if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(APP_NAME);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.status);
                });
            });
        });
        egui::SidePanel::left("nav")
            .min_width(180.0)
            .show(ctx, |ui| {
                for (label, view) in [
                    ("Home", View::Home),
                    ("Projects", View::Projects),
                    ("Tasks", View::Tasks),
                    ("Kanban", View::Kanban),
                    ("Gantt", View::Gantt),
                    ("Documents", View::Documents),
                    ("Resources", View::Resources),
                    ("Reports", View::Reports),
                    ("AI Assistant", View::Ai),
                    ("Administration", View::Administration),
                ] {
                    if ui.selectable_label(self.view == view, label).clicked() {
                        self.view = view;
                    }
                }
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| match self.view {
                View::Home => self.home(ui),
                View::Projects => self.projects(ui),
                View::Tasks => self.tasks(ui),
                View::Kanban => self.kanban(ui),
                View::Gantt => self.gantt(ui),
                View::Documents => self.documents(ui),
                View::Resources => self.resources(ui),
                View::Reports => self.reports(ui),
                View::Ai => self.ai(ui),
                View::Administration => self.administration(ui),
            });
        });
    }
}

/// Run the desktop UI; the REST API runs in the background on 127.0.0.1:8765.
pub fn run(state: AppState) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let api_state = state.clone();
    runtime.spawn(async move {
        if let Err(e) = api::serve(api_state, "127.0.0.1:8765").await {
            tracing::error!("API server failed: {e}");
        }
    });
    let app = App {
        state,
        runtime,
        view: View::Home,
        selected_project: None,
        project_name: String::new(),
        task_name: String::new(),
        document_title: String::new(),
        document_content: String::new(),
        ai_prompt: String::new(),
        ai_answer: String::new(),
        ai_config: AiConfig {
            endpoint: "http://127.0.0.1:11434/v1/chat/completions".into(),
            api_key: String::new(),
            model: "llama3.1".into(),
        },
        ai_remote_consent: false,
        status: "Ready".into(),
        dark_mode: true,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1000.0, 650.0])
            .with_title(APP_NAME),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|_cc| Ok(Box::new(app))))
        .map_err(|e| format!("desktop UI failed: {e}"))
}
