#![forbid(unsafe_code)]
// AEON-SYSTEM: single-file, read-only system inventory foundation.
// All platform-sensitive changes remain disabled until safe adapters exist.

use chrono::{DateTime, Utc};
use eframe::egui;
use serde::Serialize;
use std::{fs::{self, OpenOptions}, io::Write, path::PathBuf, time::Duration};
use sysinfo::{Disks, Networks, System};

#[derive(Clone, Serialize)]
struct ProcessRow {
    pid: String,
    name: String,
    cpu_percent: f32,
    memory_bytes: u64,
    status: String,
}

#[derive(Clone, Serialize)]
struct DiskRow {
    name: String,
    filesystem: String,
    mount_point: String,
    total_bytes: u64,
    available_bytes: u64,
}

#[derive(Clone, Serialize)]
struct NetworkRow {
    name: String,
    received_bytes: u64,
    transmitted_bytes: u64,
}

#[derive(Serialize)]
struct HealthReport {
    generated_at: DateTime<Utc>,
    hostname: String,
    os_name: String,
    kernel: String,
    uptime_seconds: u64,
    cpu_count: usize,
    cpu_usage_percent: f32,
    total_memory_bytes: u64,
    used_memory_bytes: u64,
    processes: Vec<ProcessRow>,
    disks: Vec<DiskRow>,
    networks: Vec<NetworkRow>,
}

struct AeonSystem {
    system: System,
    disks: Disks,
    networks: Networks,
    processes: Vec<ProcessRow>,
    disk_rows: Vec<DiskRow>,
    network_rows: Vec<NetworkRow>,
    hostname: String,
    os_name: String,
    kernel: String,
    cpu_usage: f32,
    last_refresh: DateTime<Utc>,
    search: String,
    selected_pid: Option<String>,
    page: String,
    status: String,
    dark: bool,
    data_dir: PathBuf,
}

impl AeonSystem {
    fn new() -> Self {
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("aeon-system");
        let _ = fs::create_dir_all(&data_dir);
        let mut app = Self {
            system: System::new_all(),
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            processes: Vec::new(),
            disk_rows: Vec::new(),
            network_rows: Vec::new(),
            hostname: System::host_name().unwrap_or_else(|| "Unavailable".into()),
            os_name: System::long_os_version().unwrap_or_else(|| System::name().unwrap_or_else(|| "Unknown OS".into())),
            kernel: System::kernel_version().unwrap_or_else(|| "Unavailable".into()),
            cpu_usage: 0.0,
            last_refresh: Utc::now(),
            search: String::new(),
            selected_pid: None,
            page: "Home".into(),
            status: "Starting read-only inventory…".into(),
            dark: true,
            data_dir,
        };
        app.refresh(false);
        app
    }

    fn audit(&self, event: &str) {
        let path = self.data_dir.join("audit.log");
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{} {}", Utc::now().to_rfc3339(), event.replace('\n', " "));
        }
    }

    fn refresh(&mut self, record: bool) {
        self.system.refresh_all();
        self.disks.refresh_list();
        self.networks.refresh();
        self.cpu_usage = self.system.global_cpu_info().cpu_usage();
        self.processes = self.system.processes().iter().map(|(pid, p)| ProcessRow {
            pid: pid.to_string(),
            name: p.name().to_string_lossy().into_owned(),
            cpu_percent: p.cpu_usage(),
            memory_bytes: p.memory(),
            status: format!("{:?}", p.status()),
        }).collect();
        self.processes.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
        self.disk_rows = self.disks.iter().map(|d| DiskRow {
            name: d.name().to_string_lossy().into_owned(),
            filesystem: d.file_system().to_string_lossy().into_owned(),
            mount_point: d.mount_point().to_string_lossy().into_owned(),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
        }).collect();
        self.network_rows = self.networks.iter().map(|(name, n)| NetworkRow {
            name: name.clone(),
            received_bytes: n.total_received(),
            transmitted_bytes: n.total_transmitted(),
        }).collect();
        self.last_refresh = Utc::now();
        self.status = format!("Inventory refreshed at {}", self.last_refresh.format("%H:%M:%S UTC"));
        if record { self.audit("READ_ONLY inventory refresh"); }
    }

    fn report(&self) -> HealthReport {
        HealthReport {
            generated_at: Utc::now(),
            hostname: self.hostname.clone(),
            os_name: self.os_name.clone(),
            kernel: self.kernel.clone(),
            uptime_seconds: System::uptime(),
            cpu_count: self.system.cpus().len(),
            cpu_usage_percent: self.cpu_usage,
            total_memory_bytes: self.system.total_memory(),
            used_memory_bytes: self.system.used_memory(),
            processes: self.processes.clone(),
            disks: self.disk_rows.clone(),
            networks: self.network_rows.clone(),
        }
    }

    fn export_report(&mut self) {
        let path = self.data_dir.join(format!("system-report-{}.json", Utc::now().format("%Y%m%d-%H%M%S")));
        match serde_json::to_vec_pretty(&self.report()).and_then(|bytes| {
            fs::write(&path, bytes).map_err(serde_json::Error::io)
        }) {
            Ok(()) => {
                self.audit(&format!("READ_ONLY report export: {}", path.display()));
                self.status = format!("Report saved: {}", path.display());
            }
            Err(e) => self.status = format!("Report export failed: {e}"),
        }
    }

    fn bytes(value: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
        let mut n = value as f64;
        let mut i = 0;
        while n >= 1024.0 && i < UNITS.len() - 1 { n /= 1024.0; i += 1; }
        format!("{n:.1} {}", UNITS[i])
    }

    fn metric(ui: &mut egui::Ui, label: &str, value: String) {
        ui.group(|ui| {
            ui.label(egui::RichText::new(label).weak());
            ui.heading(value);
        });
    }

    fn dashboard(&mut self, ui: &mut egui::Ui) {
        ui.heading("System overview");
        ui.label(format!("{} · {} · kernel {}", self.hostname, self.os_name, self.kernel));
        ui.add_space(8.0);
        egui::Grid::new("system_metrics").num_columns(3).spacing([12.0, 12.0]).show(ui, |ui| {
            Self::metric(ui, "CPU utilization", format!("{:.1}%", self.cpu_usage));
            Self::metric(ui, "Logical CPUs", self.system.cpus().len().to_string());
            Self::metric(ui, "Uptime", format!("{} days", System::uptime() / 86_400));
            ui.end_row();
            Self::metric(ui, "Memory used", Self::bytes(self.system.used_memory()));
            Self::metric(ui, "Memory total", Self::bytes(self.system.total_memory()));
            Self::metric(ui, "Processes", self.processes.len().to_string());
            ui.end_row();
            Self::metric(ui, "Disks", self.disk_rows.len().to_string());
            Self::metric(ui, "Network interfaces", self.network_rows.len().to_string());
            Self::metric(ui, "Inventory mode", "READ ONLY".into());
            ui.end_row();
        });
        ui.add_space(12.0);
        ui.label("Highest CPU processes");
        self.process_table(ui, 8);
        ui.add_space(8.0);
        ui.label("Storage volumes");
        self.disk_table(ui);
    }

    fn process_table(&mut self, ui: &mut egui::Ui, limit: usize) {
        let query = self.search.to_lowercase();
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            egui::Grid::new("process_table").striped(true).show(ui, |ui| {
                ui.strong("PID"); ui.strong("Process"); ui.strong("CPU"); ui.strong("Memory"); ui.strong("State"); ui.end_row();
                let rows: Vec<_> = self.processes.iter()
                    .filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query) || p.pid.contains(&query))
                    .take(limit).cloned().collect();
                for p in rows {
                    let selected = self.selected_pid.as_deref() == Some(&p.pid);
                    if ui.selectable_label(selected, &p.pid).clicked() { self.selected_pid = Some(p.pid.clone()); }
                    ui.label(&p.name);
                    ui.label(format!("{:.1}%", p.cpu_percent));
                    ui.label(Self::bytes(p.memory_bytes));
                    ui.label(&p.status);
                    ui.end_row();
                }
            });
        });
    }

    fn disk_table(&self, ui: &mut egui::Ui) {
        egui::Grid::new("disk_table").striped(true).show(ui, |ui| {
            ui.strong("Volume"); ui.strong("Mount"); ui.strong("Filesystem"); ui.strong("Used / Total"); ui.end_row();
            for d in &self.disk_rows {
                let used = d.total_bytes.saturating_sub(d.available_bytes);
                ui.label(&d.name); ui.label(&d.mount_point); ui.label(&d.filesystem);
                ui.label(format!("{} / {}", Self::bytes(used), Self::bytes(d.total_bytes)));
                ui.end_row();
            }
        });
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        match self.page.as_str() {
            "Home" => self.dashboard(ui),
            "Performance" | "Processes" => {
                ui.heading("Process and performance monitor");
                ui.label("Live process inventory. Selecting a PID displays its identifier; termination is intentionally not enabled in this read-only foundation.");
                ui.horizontal(|ui| { ui.label("Filter"); ui.text_edit_singleline(&mut self.search); });
                self.process_table(ui, usize::MAX);
                if let Some(pid) = &self.selected_pid { ui.label(format!("Selected PID: {pid}")); }
            }
            "Storage" => { ui.heading("Storage inventory"); self.disk_table(ui); }
            "Network" => {
                ui.heading("Network interfaces");
                egui::Grid::new("network_table").striped(true).show(ui, |ui| {
                    ui.strong("Interface"); ui.strong("Received"); ui.strong("Transmitted"); ui.end_row();
                    for n in &self.network_rows {
                        ui.label(&n.name); ui.label(Self::bytes(n.received_bytes)); ui.label(Self::bytes(n.transmitted_bytes)); ui.end_row();
                    }
                });
            }
            "System" => {
                ui.heading("System information");
                ui.label(format!("Computer: {}", self.hostname));
                ui.label(format!("Operating system: {}", self.os_name));
                ui.label(format!("Kernel: {}", self.kernel));
                ui.label(format!("Architecture: {}", std::env::consts::ARCH));
                ui.label(format!("Family: {}", std::env::consts::FAMILY));
                ui.label(format!("Logical CPUs: {}", self.system.cpus().len()));
                ui.label(format!("Total memory: {}", Self::bytes(self.system.total_memory())));
            }
            "AI Assistant" => {
                ui.heading("AI System Assistant");
                ui.label("The assistant interface is reserved for a later bounded, approval-based capability runtime.");
                ui.label("Current safe queries: inspect CPU, memory, process, disk and network inventory using the navigation sections.");
                ui.label("No model connection or command execution is active in this foundation.");
            }
            "Security" | "Maintenance" | "Backup & Recovery" | "Automation" | "Devices" | "Applications" | "Advanced Tools" | "Settings" => {
                ui.heading(&self.page);
                ui.label("Capability not yet implemented. This section is intentionally marked unavailable rather than showing a non-functional control surface.");
            }
            _ => { ui.heading("AEON-SYSTEM"); ui.label("Choose a section from the navigation sidebar."); }
        }
    }
}

impl eframe::App for AeonSystem {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_secs(2));
        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("AEON-SYSTEM");
                ui.separator();
                ui.label("Universal OS Control Centre");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Export JSON report").clicked() { self.export_report(); }
                    if ui.button("Refresh").clicked() { self.refresh(true); }
                    if ui.button(if self.dark { "Light theme" } else { "Dark theme" }).clicked() {
                        self.dark = !self.dark;
                        ctx.set_visuals(if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    }
                });
            });
        });
        egui::SidePanel::left("navigation").resizable(true).default_width(190.0).show(ctx, |ui| {
            ui.heading("Navigation");
            for page in ["Home", "System", "Performance", "Storage", "Network", "Devices", "Applications", "Security", "Maintenance", "Backup & Recovery", "Automation", "AI Assistant", "Advanced Tools", "Settings"] {
                if ui.selectable_label(self.page == page, page).clicked() { self.page = page.into(); }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.separator();
                ui.small("Local · Offline-first · Read-only");
                ui.small("AEON runtime integration: not connected");
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            self.content(ui);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.small(&self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.small(format!("Updated {}", self.last_refresh.format("%H:%M:%S UTC")));
                });
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AEON-SYSTEM")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("AEON-SYSTEM", options, Box::new(|_cc| Ok(Box::new(AeonSystem::new()))))
}
