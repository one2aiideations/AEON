use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum WorkspaceKind { Document, Spreadsheet, Presentation, Pdf, Image, Vector, Video, Audio, Knowledge }

impl WorkspaceKind {
    const ALL: [Self; 9] = [Self::Document, Self::Spreadsheet, Self::Presentation, Self::Pdf, Self::Image, Self::Vector, Self::Video, Self::Audio, Self::Knowledge];
    fn label(self) -> &'static str { match self {
        Self::Document => "Documents", Self::Spreadsheet => "Spreadsheets", Self::Presentation => "Presentations",
        Self::Pdf => "PDF", Self::Image => "Images", Self::Vector => "Graphics", Self::Video => "Video",
        Self::Audio => "Audio", Self::Knowledge => "Knowledge",
    }}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StudioDocument {
    id: Uuid,
    title: String,
    kind: WorkspaceKind,
    body: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
impl StudioDocument {
    fn new(kind: WorkspaceKind) -> Self {
        let now = Utc::now();
        Self { id: Uuid::new_v4(), title: format!("Untitled {}", kind.label()), kind, body: String::new(), created_at: now, updated_at: now }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StudioProject {
    format_version: u32,
    product: String,
    document: StudioDocument,
}
impl StudioProject {
    fn new(kind: WorkspaceKind) -> Self { Self { format_version: 1, product: "AEON-STUDIO".into(), document: StudioDocument::new(kind) } }
}

struct AeonStudio {
    project: StudioProject,
    active: WorkspaceKind,
    path: Option<PathBuf>,
    status: String,
    show_ai: bool,
    ai_prompt: String,
}
impl Default for AeonStudio {
    fn default() -> Self { Self { project: StudioProject::new(WorkspaceKind::Document), active: WorkspaceKind::Document, path: None, status: "Local workspace ready".into(), show_ai: true, ai_prompt: String::new() } }
}
impl AeonStudio {
    fn new_document(&mut self, kind: WorkspaceKind) {
        self.project = StudioProject::new(kind); self.active = kind; self.path = None;
        self.status = format!("New {} workspace created", kind.label());
    }
    fn save(&mut self) -> Result<()> {
        let path = match self.path.clone() {
            Some(p) => p,
            None => rfd::FileDialog::new().add_filter("AEON Studio project", &["aeonstudio"]).set_file_name("untitled.aeonstudio").save_file().context("Save cancelled")?,
        };
        self.project.document.updated_at = Utc::now();
        let bytes = serde_json::to_vec_pretty(&self.project)?;
        fs::write(&path, bytes).with_context(|| format!("Unable to write {}", path.display()))?;
        self.path = Some(path.clone()); self.status = format!("Saved {}", path.display()); Ok(())
    }
    fn open(&mut self) -> Result<()> {
        let path = rfd::FileDialog::new().add_filter("AEON Studio project", &["aeonstudio"]).pick_file().context("Open cancelled")?;
        let bytes = fs::read(&path)?;
        let project: StudioProject = serde_json::from_slice(&bytes).context("Invalid AEON-STUDIO project file")?;
        if project.format_version != 1 { anyhow::bail!("Unsupported project format version {}", project.format_version); }
        self.active = project.document.kind; self.project = project; self.path = Some(path.clone());
        self.status = format!("Opened {}", path.display()); Ok(())
    }
    fn export_text(&mut self) -> Result<()> {
        let path = rfd::FileDialog::new().add_filter("Text", &["txt", "md"]).set_file_name("document.txt").save_file().context("Export cancelled")?;
        fs::write(&path, &self.project.document.body)?;
        self.status = format!("Exported text to {}", path.display()); Ok(())
    }
}

impl eframe::App for AeonStudio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("AEON-STUDIO");
                ui.separator();
                if ui.button("New").clicked() { self.new_document(self.active); }
                if ui.button("Open").clicked() { if let Err(e) = self.open() { self.status = e.to_string(); } }
                if ui.button("Save").clicked() { if let Err(e) = self.save() { self.status = e.to_string(); } }
                if ui.button("Export text").clicked() { if let Err(e) = self.export_text() { self.status = e.to_string(); } }
                ui.separator();
                ui.checkbox(&mut self.show_ai, "AI assistant");
            });
        });
        egui::SidePanel::left("navigation").resizable(true).default_width(165.0).show(ctx, |ui| {
            ui.heading("Workspace");
            for kind in WorkspaceKind::ALL {
                if ui.selectable_label(self.active == kind, kind.label()).clicked() {
                    self.active = kind;
                    if self.project.document.kind != kind { self.new_document(kind); }
                }
            }
            ui.separator();
            ui.label("Shared services");
            ui.label("• Project container");
            ui.label("• Local file persistence");
            ui.label("• Asset library (planned)");
            ui.label("• AI provider layer (planned)");
        });
        if self.show_ai {
            egui::SidePanel::right("assistant").resizable(true).default_width(245.0).show(ctx, |ui| {
                ui.heading("AI Studio");
                ui.label("Provider-independent assistant integration is a later implementation stage.");
                ui.add_space(8.0);
                ui.label("Prompt");
                ui.text_edit_multiline(&mut self.ai_prompt);
                ui.add_enabled(false, egui::Button::new("Run on active content"));
                ui.small("No remote provider is contacted by this starter build.");
            });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(self.active.label());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { ui.label(format!("Project v{}", self.project.format_version)); });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Title");
                ui.text_edit_singleline(&mut self.project.document.title);
            });
            ui.add_space(6.0);
            match self.active {
                WorkspaceKind::Document | WorkspaceKind::Knowledge => {
                    ui.label("Editable local text workspace");
                    ui.add_sized(ui.available_size(), egui::TextEdit::multiline(&mut self.project.document.body).font(egui::TextStyle::Body).desired_width(f32::INFINITY));
                }
                _ => {
                    ui.heading(format!("{} engine", self.active.label()));
                    ui.label("Workspace navigation and project persistence are active. This content engine is not implemented yet.");
                    ui.label("The master specification defines the required editing, import/export, AI and validation milestones.");
                }
            }
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| { ui.horizontal(|ui| { ui.label(&self.status); if let Some(p) = &self.path { ui.separator(); ui.weak(p.display().to_string()); } }); });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 900.0]).with_min_inner_size([900.0, 600.0]), ..Default::default() };
    eframe::run_native("AEON-STUDIO", options, Box::new(|_cc| Ok(Box::<AeonStudio>::default())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn project_round_trips_json() {
        let project = StudioProject::new(WorkspaceKind::Document);
        let encoded = serde_json::to_vec(&project).unwrap();
        let decoded: StudioProject = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.format_version, 1);
        assert_eq!(decoded.document.id, project.document.id);
    }
    #[test] fn all_workspace_kinds_have_labels() {
        for kind in WorkspaceKind::ALL { assert!(!kind.label().is_empty()); }
    }
}
