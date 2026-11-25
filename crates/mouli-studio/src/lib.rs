//! MOULI-STUDIO: one product, one executable.
//!
//! Status: scaffold. What works and is tested:
//! - the spreadsheet engine (from `mouli-sheet`: formulas, functions,
//!   dependency-ordered recalculation, cycle detection);
//! - the plain-text `.moulistudio` v0 container for documents and spreadsheets;
//! - the consent, local-only and approval gates for AI providers and agents.
//!
//! Every heavy engine (GUI, codecs, PDF, RAW, AI provider calls, agents'
//! actual operations) is a typed, fail-closed placeholder returning
//! [`MouliError::NotImplemented`]. Placeholders never fake artifacts.
#![forbid(unsafe_code)]
// The placeholder API surface is documented as each engine is implemented.
#![allow(missing_docs)]

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub use mouli_sheet::{CellValue, Workbook};

/// First line of every `.moulistudio` v0 file.
pub const FORMAT_MAGIC: &str = "MOULISTUDIO-1";
/// Application version.
pub const VERSION: &str = "0.1.0-scaffold";

/* ============================ 1. ERRORS & CAPABILITIES ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Implemented,
    Partial,
    Stubbed,
    Blocked,
}

/// Every failure the application can report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouliError {
    NotImplemented(&'static str),
    UnsupportedFormat(String),
    Validation(String),
    PermissionDenied(String),
    ExternalProviderUnavailable(String),
    ConsentRequired(&'static str),
    CredentialMissing(&'static str),
    NotFound(String),
    Io(String),
}

impl fmt::Display for MouliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented(s) => write!(f, "not implemented: {s}"),
            Self::UnsupportedFormat(s) => write!(f, "unsupported format: {s}"),
            Self::Validation(s) => write!(f, "validation failed: {s}"),
            Self::PermissionDenied(s) => write!(f, "permission denied: {s}"),
            Self::ExternalProviderUnavailable(s) => write!(f, "provider unavailable: {s}"),
            Self::ConsentRequired(s) => write!(f, "explicit consent required: {s}"),
            Self::CredentialMissing(s) => write!(f, "credential missing: {s}"),
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::Io(s) => write!(f, "io error: {s}"),
        }
    }
}

impl std::error::Error for MouliError {}

impl From<std::io::Error> for MouliError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

pub type MouliResult<T> = Result<T, MouliError>;

fn ni<T>(feature: &'static str) -> MouliResult<T> {
    Err(MouliError::NotImplemented(feature))
}

/// Implementation status of each area (what the UI should show honestly).
#[must_use]
pub fn capabilities() -> Vec<(&'static str, Capability)> {
    vec![
        (
            "spreadsheet formulas & recalculation",
            Capability::Implemented,
        ),
        (
            "document container (.moulistudio v0)",
            Capability::Implemented,
        ),
        (
            "spreadsheet container (.moulistudio v0)",
            Capability::Implemented,
        ),
        ("AI consent / local-only gates", Capability::Implemented),
        ("agent approval gates", Capability::Implemented),
        ("xlsx/docx/pptx/pdf import-export", Capability::Stubbed),
        ("image/video/audio engines", Capability::Stubbed),
        ("AI provider calls", Capability::Stubbed),
        ("desktop GUI", Capability::Stubbed),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    Document,
    Spreadsheet,
    Presentation,
    Pdf,
    Photo,
    Vector,
    Video,
    Audio,
    Animation,
    KnowledgePage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Document,
    Spreadsheet,
    Presentation,
    Pdf,
    Image,
    Video,
    Audio,
    Batch,
    AiCreation,
    HeadlessExport,
    Api,
    Desktop,
}

/* ============================ 2. OBSERVABILITY ============================ */

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub render_ms: u64,
    pub export_ms: u64,
    pub ai_tokens: u64,
    pub memory_bytes: u64,
    pub gpu_frame_ms: u64,
}

/* ============================ 3. WORKSPACE / TABS / NAV ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NavItem {
    Home,
    Recent,
    Documents,
    Spreadsheets,
    Presentations,
    Pdfs,
    Images,
    Graphics,
    Video,
    Audio,
    Animation,
    Design,
    Templates,
    Projects,
    Assets,
    AiStudio,
    Automation,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Theme {
    Light,
    Dark,
}

#[derive(Debug, Clone)]
pub struct DocumentTab {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub kind: ContentKind,
    pub dirty: bool,
}

#[derive(Debug)]
pub struct Workspace {
    pub tabs: Vec<DocumentTab>,
    pub active: Option<u64>,
    pub theme: Theme,
    pub nav: NavItem,
    pub command_palette_open: bool,
    pub metrics: Metrics,
    next_tab: u64,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            theme: Theme::Dark,
            nav: NavItem::Home,
            command_palette_open: false,
            metrics: Metrics::default(),
            next_tab: 1,
        }
    }
}

impl Workspace {
    /// Open a tab and make it active. Ids are never reused.
    pub fn open_tab(&mut self, kind: ContentKind, path: Option<PathBuf>) -> u64 {
        let id = self.next_tab;
        self.next_tab += 1;
        self.tabs.push(DocumentTab {
            id,
            path,
            kind,
            dirty: false,
        });
        self.active = Some(id);
        id
    }

    /// Close a tab; activates the last remaining tab. Returns whether it existed.
    pub fn close_tab(&mut self, id: u64) -> bool {
        let before = self.tabs.len();
        self.tabs.retain(|t| t.id != id);
        if self.active == Some(id) {
            self.active = self.tabs.last().map(|t| t.id);
        }
        self.tabs.len() != before
    }

    #[must_use]
    pub fn command_palette_entries(&self) -> Vec<&'static str> {
        vec![
            "save",
            "open",
            "new document",
            "new spreadsheet",
            "new presentation",
            "export",
            "ai rewrite selection",
            "convert active",
            "toggle theme",
            "headless export",
        ]
    }

    pub fn toggle_theme(&mut self) {
        self.theme = match self.theme {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        };
    }
}

/* ============================ 4. .moulistudio CONTAINER (plain-text v0) ============================ */

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unesc(s: &str) -> MouliResult<String> {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(o) => return Err(MouliError::Validation(format!("bad escape \\{o}"))),
                None => return Err(MouliError::Validation("trailing backslash".into())),
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn read_header<'a>(input: &'a str, kind: &str) -> MouliResult<std::str::Lines<'a>> {
    let mut lines = input.lines();
    if lines.next() != Some(FORMAT_MAGIC) {
        return Err(MouliError::Validation("bad magic".into()));
    }
    if lines.next() != Some(&format!("kind={kind}")[..]) {
        return Err(MouliError::UnsupportedFormat(format!(
            "expected kind={kind}"
        )));
    }
    Ok(lines)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(String),
    Paragraph(String),
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    #[must_use]
    pub fn to_container(&self) -> String {
        let mut s = format!("{FORMAT_MAGIC}\nkind=document\n");
        for b in &self.blocks {
            match b {
                Block::Heading(t) => s.push_str(&format!("H1:{}\n", esc(t))),
                Block::Paragraph(t) => s.push_str(&format!("P:{}\n", esc(t))),
            }
        }
        s
    }

    pub fn from_container(input: &str) -> MouliResult<Self> {
        let mut d = Self::default();
        for line in read_header(input, "document")? {
            if let Some(t) = line.strip_prefix("H1:") {
                d.blocks.push(Block::Heading(unesc(t)?));
            } else if let Some(t) = line.strip_prefix("P:") {
                d.blocks.push(Block::Paragraph(unesc(t)?));
            } else if !line.trim().is_empty() {
                return Err(MouliError::Validation(format!("unknown line: {line}")));
            }
        }
        Ok(d)
    }

    #[must_use]
    pub fn render_plain(&self) -> String {
        self.blocks
            .iter()
            .map(|b| match b {
                Block::Heading(t) => format!("# {t}"),
                Block::Paragraph(t) => t.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn save(&self, p: &Path) -> MouliResult<()> {
        Ok(fs::write(p, self.to_container())?)
    }

    pub fn load(p: &Path) -> MouliResult<Self> {
        Self::from_container(&fs::read_to_string(p)?)
    }
}

/// Load either kind of `.moulistudio` v0 file.
#[derive(Debug)]
pub enum Opened {
    Document(Document),
    Spreadsheet(SpreadsheetDoc),
}

pub fn open_any(p: &Path) -> MouliResult<Opened> {
    let text = fs::read_to_string(p)?;
    match text.lines().nth(1) {
        Some("kind=document") => Document::from_container(&text).map(Opened::Document),
        Some("kind=spreadsheet") => SpreadsheetDoc::from_container(&text).map(Opened::Spreadsheet),
        other => Err(MouliError::UnsupportedFormat(format!(
            "unknown kind line {other:?}"
        ))),
    }
}

// Zip-backed `.moulistudio` with version history and embeds: fail-closed until implemented.
pub fn open_container(_path: &Path) -> MouliResult<Vec<(String, Vec<u8>)>> {
    ni("zip .moulistudio container")
}
pub fn save_container(_path: &Path, _parts: &[(String, Vec<u8>)]) -> MouliResult<()> {
    ni("zip .moulistudio container")
}
pub fn autosave(_doc: &Document, _path: &Path) -> MouliResult<PathBuf> {
    ni("autosave")
}
pub fn restore(_recovery_id: &str) -> MouliResult<Vec<u8>> {
    ni("recovery restore")
}
pub fn snapshot_version(_path: &Path) -> MouliResult<String> {
    ni("version snapshot")
}
pub fn list_versions(_path: &Path) -> MouliResult<Vec<String>> {
    ni("version list")
}

/* ============================ 5. ASSET LIBRARY ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Image,
    Video,
    Audio,
    Font,
    Icon,
    Illustration,
    Shape,
    Template,
    Brand,
    Document,
    Chart,
    Component,
}

#[derive(Debug, Clone)]
pub struct AssetRecord {
    pub id: String,
    pub kind: AssetKind,
    pub path: PathBuf,
    pub hash: String,
    pub tags: Vec<String>,
    pub collections: Vec<String>,
    pub version: u32,
}

pub trait ProjectStore {
    fn upsert_asset(&mut self, a: &AssetRecord) -> MouliResult<String>;
    fn find_assets(&self, query: &str) -> MouliResult<Vec<AssetRecord>>;
    fn record_operation(&mut self, op: &str, target: Option<&str>) -> MouliResult<String>;
    fn list_history(&self, doc_id: &str) -> MouliResult<Vec<String>>;
    fn append_audit(&mut self, agent: &str, tool: &str, approved: bool) -> MouliResult<()>;
}

/// Local-first fallback store: never destructive.
#[derive(Debug, Default)]
pub struct NullStore;

impl ProjectStore for NullStore {
    fn upsert_asset(&mut self, _a: &AssetRecord) -> MouliResult<String> {
        ni("asset upsert (store not wired)")
    }
    fn find_assets(&self, _q: &str) -> MouliResult<Vec<AssetRecord>> {
        Ok(Vec::new())
    }
    fn record_operation(&mut self, _op: &str, _t: Option<&str>) -> MouliResult<String> {
        ni("operation log")
    }
    fn list_history(&self, _d: &str) -> MouliResult<Vec<String>> {
        Ok(Vec::new())
    }
    fn append_audit(&mut self, _a: &str, _t: &str, _ok: bool) -> MouliResult<()> {
        Ok(())
    }
}

/// Report duplicate assets by hash (never deletes anything).
#[must_use]
pub fn duplicates_by_hash(assets: &[AssetRecord]) -> Vec<Vec<String>> {
    let mut by: HashMap<&str, Vec<String>> = HashMap::new();
    for a in assets {
        by.entry(a.hash.as_str()).or_default().push(a.id.clone());
    }
    let mut groups: Vec<Vec<String>> = by.into_values().filter(|g| g.len() > 1).collect();
    groups.iter_mut().for_each(|g| g.sort());
    groups.sort();
    groups
}

/* ============================ 6. SPREADSHEET (engine: mouli-sheet) ============================ */

/// Evaluate a one-off formula (with or without a leading `=`).
#[must_use]
pub fn calc(formula: &str) -> CellValue {
    let mut wb = Workbook::new();
    let raw = format!("={}", formula.trim().trim_start_matches('='));
    match wb.set_cell(0, 0, 0, &raw) {
        Ok(()) => wb.get_cell(0, 0, 0),
        Err(e) => CellValue::Error(e),
    }
}

/// A spreadsheet document: an `mouli-sheet` workbook plus file metadata.
#[derive(Debug, Default)]
pub struct SpreadsheetDoc {
    pub workbook: Workbook,
}

impl SpreadsheetDoc {
    /// Set a cell by name (`B7`) on the first sheet.
    pub fn set(&mut self, cell: &str, raw: &str) -> MouliResult<()> {
        let (r, c) = mouli_sheet::parse_cell_name(cell)
            .ok_or_else(|| MouliError::Validation(format!("bad cell {cell}")))?;
        self.workbook
            .set_cell(0, r, c, raw)
            .map_err(MouliError::Validation)
    }

    /// Value of a cell by name on the first sheet.
    pub fn get(&self, cell: &str) -> MouliResult<CellValue> {
        let (r, c) = mouli_sheet::parse_cell_name(cell)
            .ok_or_else(|| MouliError::Validation(format!("bad cell {cell}")))?;
        Ok(self.workbook.get_cell(0, r, c))
    }

    /// Serialise raw inputs (values are recomputed on load). One line per
    /// cell: `C:<sheet>:<CELL>:<raw>`, sheets as `S:<name>`.
    #[must_use]
    pub fn to_container(&self) -> String {
        let mut s = format!("{FORMAT_MAGIC}\nkind=spreadsheet\n");
        for (i, sh) in self.workbook.sheets.iter().enumerate() {
            s.push_str(&format!("S:{}\n", esc(&sh.name)));
            let mut keys: Vec<&(u32, u32)> = sh.cells.keys().collect();
            keys.sort();
            for k in keys {
                let raw = &sh.cells[k].raw;
                s.push_str(&format!(
                    "C:{i}:{}:{}\n",
                    mouli_sheet::cell_name(k.0, k.1),
                    esc(raw)
                ));
            }
        }
        s
    }

    pub fn from_container(input: &str) -> MouliResult<Self> {
        let mut wb = Workbook::new();
        wb.sheets.clear();
        for line in read_header(input, "spreadsheet")? {
            if let Some(name) = line.strip_prefix("S:") {
                wb.sheets.push(mouli_sheet::Sheet::new(&unesc(name)?));
            } else if let Some(rest) = line.strip_prefix("C:") {
                let mut parts = rest.splitn(3, ':');
                let (Some(si), Some(cell), Some(raw)) = (parts.next(), parts.next(), parts.next())
                else {
                    return Err(MouliError::Validation(format!("bad cell line: {line}")));
                };
                let si: usize = si
                    .parse()
                    .map_err(|_| MouliError::Validation(format!("bad sheet index: {line}")))?;
                if si >= wb.sheets.len() {
                    return Err(MouliError::Validation(format!(
                        "cell before its sheet: {line}"
                    )));
                }
                let (r, c) = mouli_sheet::parse_cell_name(cell)
                    .ok_or_else(|| MouliError::Validation(format!("bad cell {cell}")))?;
                wb.set_cell(si, r, c, &unesc(raw)?)
                    .map_err(MouliError::Validation)?;
            } else if !line.trim().is_empty() {
                return Err(MouliError::Validation(format!("unknown line: {line}")));
            }
        }
        if wb.sheets.is_empty() {
            wb.sheets.push(mouli_sheet::Sheet::new("Sheet1"));
        }
        Ok(Self { workbook: wb })
    }

    pub fn save(&self, p: &Path) -> MouliResult<()> {
        Ok(fs::write(p, self.to_container())?)
    }

    pub fn load(p: &Path) -> MouliResult<Self> {
        Self::from_container(&fs::read_to_string(p)?)
    }

    pub fn xlsx_import(&mut self, _p: &Path) -> MouliResult<()> {
        ni("xlsx import")
    }
    pub fn xlsx_export(&self, _p: &Path) -> MouliResult<()> {
        ni("xlsx export")
    }
    pub fn pivot(&self, _src: &str) -> MouliResult<String> {
        ni("pivot tables")
    }
    pub fn chart_recommend(&self, _range: &str) -> MouliResult<String> {
        ni("chart recommendation")
    }
}

/* ============================ 7. WORD PROCESSOR ============================ */

#[derive(Debug, Default)]
pub struct RichDocument {
    pub sections: Vec<String>,
    pub styles: Vec<String>,
}

impl RichDocument {
    pub fn paginate(&self) -> MouliResult<Vec<String>> {
        ni("pagination/layout engine")
    }
    pub fn docx_import(&mut self, _p: &Path) -> MouliResult<()> {
        ni("docx import")
    }
    pub fn docx_export(&self, _p: &Path) -> MouliResult<()> {
        ni("docx export")
    }
    pub fn track_changes(&self, _a: &str, _b: &str) -> MouliResult<String> {
        ni("track changes")
    }
    pub fn mail_merge(&self, _ds: &str) -> MouliResult<String> {
        ni("mail merge")
    }
    pub fn spellcheck(&self, _text: &str) -> MouliResult<Vec<String>> {
        ni("spellcheck dictionary")
    }
    pub fn toc(&self) -> MouliResult<String> {
        ni("table of contents generation")
    }
}

/* ============================ 8. PRESENTATION ENGINE ============================ */

#[derive(Debug, Default)]
pub struct Presentation {
    pub slides: Vec<String>,
    pub master: String,
    pub theme: String,
}

impl Presentation {
    pub fn render_slide(&self, _idx: usize) -> MouliResult<Vec<u8>> {
        ni("slide raster render")
    }
    pub fn pptx_import(&mut self, _p: &Path) -> MouliResult<()> {
        ni("pptx import")
    }
    pub fn pptx_export(&self, _p: &Path) -> MouliResult<()> {
        ni("pptx export")
    }
    pub fn presenter_view(&self) -> MouliResult<()> {
        ni("presenter view")
    }
    pub fn generate_from_prompt(&self, _prompt: &str) -> MouliResult<Self> {
        ni("AI slide generation")
    }
}

/* ============================ 9. PDF ENGINE ============================ */

#[derive(Debug, Default)]
pub struct PdfDocument {
    pub pages: u32,
    pub annotations: Vec<String>,
    pub redactions: Vec<String>,
}

impl PdfDocument {
    pub fn extract_text(&self) -> MouliResult<String> {
        ni("pdf text extraction")
    }
    pub fn merge(&self, _others: Vec<Self>) -> MouliResult<Self> {
        ni("pdf merge")
    }
    pub fn redact_permanent(&mut self, _rect: (f32, f32, f32, f32), _page: u32) -> MouliResult<()> {
        ni("permanent redaction (content destruction), fail closed")
    }
    pub fn sign_verify(&self, _sig: &[u8]) -> MouliResult<bool> {
        ni("cryptographic signature verification")
    }
    pub fn export_pdfa(&self, _p: &Path) -> MouliResult<()> {
        ni("PDF/A export")
    }
    pub fn ocr(&self) -> MouliResult<String> {
        ni("OCR")
    }
}

/* ============================ 10. PHOTO / IMAGE ENGINE ============================ */

#[derive(Debug, Default)]
pub struct ImageDocument {
    pub layers: Vec<String>,
    pub active: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOp {
    Crop,
    Curves,
    Levels,
    Exposure,
    GaussianBlur,
    Sharpen,
    NoiseReduce,
    Clone,
    Heal,
    Inpaint,
}

impl ImageDocument {
    pub fn composite(&self) -> MouliResult<Vec<u8>> {
        ni("layer compositing")
    }
    pub fn apply_op(&mut self, _op: ImageOp) -> MouliResult<()> {
        ni("image operation")
    }
    pub fn raw_decode(&mut self, _p: &Path) -> MouliResult<()> {
        ni("RAW decode")
    }
    pub fn export_png(&self, _p: &Path) -> MouliResult<()> {
        ni("png export")
    }
    pub fn generative_fill(&mut self, _prompt: &str, consent: bool) -> MouliResult<()> {
        if !consent {
            return Err(MouliError::ConsentRequired(
                "external/local generative model",
            ));
        }
        ni("generative fill")
    }
}

/* ============================ 11. VECTOR / DESIGN ENGINE ============================ */

#[derive(Debug, Default)]
pub struct VectorDocument {
    pub artboards: Vec<String>,
    pub paths: Vec<String>,
    pub symbols: HashMap<String, String>,
}

impl VectorDocument {
    pub fn svg_export(&self, _p: &Path) -> MouliResult<()> {
        ni("SVG export")
    }
    pub fn svg_import(&mut self, _p: &Path) -> MouliResult<()> {
        ni("SVG import")
    }
    pub fn boolean_union(&mut self) -> MouliResult<()> {
        ni("boolean path ops")
    }
    pub fn text_to_vector(&mut self, _prompt: &str) -> MouliResult<()> {
        ni("text-to-vector (needs provider; editable paths required)")
    }
}

/* ============================ 12. VIDEO / TIMELINE ENGINE ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackKind {
    #[default]
    Video,
    Audio,
    Overlay,
}

#[derive(Debug, Clone)]
pub struct Marker {
    pub tick: u64,
    pub label: String,
}

#[derive(Debug, Default)]
pub struct Track {
    pub kind: TrackKind,
    pub clips: Vec<String>,
    pub locked: bool,
}

#[derive(Debug)]
pub struct Timeline {
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub fps: f64,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            markers: Vec::new(),
            fps: 30.0,
        }
    }
}

impl Timeline {
    pub fn render_export(&self, _out: &Path) -> MouliResult<()> {
        ni("video render/export")
    }
    pub fn decode_frame(&self, _t: u64) -> MouliResult<Vec<u8>> {
        ni("frame decode")
    }
    pub fn scene_detect(&self) -> MouliResult<Vec<u64>> {
        ni("scene detection")
    }
    pub fn transcribe(&self) -> MouliResult<String> {
        ni("speech transcription")
    }
    pub fn generate_from_recording(&self, _hours: f32, _target_min: f32) -> MouliResult<Self> {
        ni("AI auto-cut (needs provider + approval)")
    }
}

/* ============================ 13. AUDIO ENGINE ============================ */

#[derive(Debug, Default)]
pub struct AudioProject {
    pub tracks: Vec<String>,
    pub sample_rate: u32,
}

impl AudioProject {
    pub fn record(&mut self) -> MouliResult<()> {
        ni("audio capture")
    }
    pub fn mix(&self) -> MouliResult<Vec<u8>> {
        ni("mixing engine")
    }
    pub fn denoise(&mut self) -> MouliResult<()> {
        ni("noise reduction")
    }
    pub fn transcribe(&self) -> MouliResult<String> {
        ni("speech-to-text")
    }
    pub fn tts(&self, _text: &str, consent: bool) -> MouliResult<Vec<u8>> {
        if !consent {
            return Err(MouliError::ConsentRequired("voice synthesis"));
        }
        ni("text-to-speech provider")
    }
}

/* ============================ 14. ANIMATION / MOTION ENGINE ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimProperty {
    Position,
    Rotation,
    Scale,
    Opacity,
}

#[derive(Debug, Clone)]
pub struct Keyframe {
    pub tick: u64,
    pub property: AnimProperty,
    pub value: f64,
}

#[derive(Debug, Default)]
pub struct AnimationScene {
    pub keyframes: Vec<Keyframe>,
}

impl AnimationScene {
    /// Linearly interpolated value of `property` at `tick` (holds the first
    /// and last keyframe values outside their range).
    #[must_use]
    pub fn sample(&self, property: AnimProperty, tick: u64) -> Option<f64> {
        let mut ks: Vec<&Keyframe> = self
            .keyframes
            .iter()
            .filter(|k| k.property == property)
            .collect();
        ks.sort_by_key(|k| k.tick);
        let first = ks.first()?;
        if tick <= first.tick {
            return Some(first.value);
        }
        for w in ks.windows(2) {
            let (a, b) = (w[0], w[1]);
            if tick <= b.tick {
                let span = (b.tick - a.tick) as f64;
                let t = if span > 0.0 {
                    (tick - a.tick) as f64 / span
                } else {
                    1.0
                };
                return Some(a.value + (b.value - a.value) * t);
            }
        }
        ks.last().map(|k| k.value)
    }
    pub fn export_gif(&self, _p: &Path) -> MouliResult<()> {
        ni("GIF export")
    }
    pub fn apply_preset(&mut self, _name: &str) -> MouliResult<()> {
        ni("motion preset")
    }
}

/* ============================ 15. KNOWLEDGE ENGINE ============================ */

#[derive(Debug, Default)]
pub struct KnowledgePage {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub backlinks: Vec<String>,
}

impl KnowledgePage {
    pub fn semantic_search(&self, _q: &str) -> MouliResult<Vec<String>> {
        ni("embeddings search")
    }
    /// Case-insensitive match on title, body and tags.
    #[must_use]
    pub fn lexical_match(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        !q.is_empty()
            && (self.title.to_lowercase().contains(&q)
                || self.body.to_lowercase().contains(&q)
                || self.tags.iter().any(|t| t.to_lowercase() == q))
    }
    pub fn meeting_notes(&self, _transcript: &str) -> MouliResult<String> {
        ni("meeting note synthesis")
    }
}

/* ============================ 16. CONVERSION ENGINE ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionKind {
    DocxToPdf,
    DocxToPptx,
    PdfToDocx,
    PdfTablesToXlsx,
    SpreadsheetToPptx,
    VideoToTranscript,
    AudioToMinutes,
    ImageToVector,
    SlidesToVideo,
    ResearchToReport,
}

#[derive(Debug, Default)]
pub struct ConversionReport {
    pub produced: Vec<PathBuf>,
    pub lossy_notes: Vec<String>,
    pub unsupported: Vec<String>,
}

pub fn convert(
    _kind: ConversionKind,
    _input: &Path,
    _output: &Path,
) -> MouliResult<ConversionReport> {
    ni("conversion engine")
}

/* ============================ 17. AI PROVIDER LAYER ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProviderKind {
    OpenAiCompatible,
    AnthropicCompatible,
    Ollama,
    LocalServer,
    VideoGen,
    Speech,
}

impl AiProviderKind {
    /// Runs on this machine.
    #[must_use]
    pub fn is_local(self) -> bool {
        matches!(self, Self::Ollama | Self::LocalServer)
    }
}

#[derive(Debug, Clone)]
pub struct AiMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct AiRequest {
    pub system: String,
    pub messages: Vec<AiMessage>,
    pub max_tokens: u32,
    pub local_only: bool,
    pub external_consent: bool,
}

#[derive(Debug, Clone)]
pub struct AiResponse {
    pub text: String,
}

pub trait AiProvider {
    fn name(&self) -> &str;
    fn kind(&self) -> AiProviderKind;
    fn chat(&self, req: &AiRequest) -> MouliResult<AiResponse>;
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: AiProviderKind,
    pub endpoint: String,
    pub local_only: bool,
}

/// Gate checks, in order: local-only policy, consent for remote providers,
/// then whether network providers are built. The call itself is not wired yet.
pub fn chat(cfg: &ProviderConfig, req: &AiRequest) -> MouliResult<AiResponse> {
    let remote = !cfg.kind.is_local();
    if remote && (req.local_only || cfg.local_only) {
        return Err(MouliError::PermissionDenied(
            "local_only forbids remote providers".into(),
        ));
    }
    if remote && !req.external_consent {
        return Err(MouliError::ConsentRequired(
            "sending document context to an external AI",
        ));
    }
    if remote && !cfg!(feature = "net") {
        return Err(MouliError::ExternalProviderUnavailable(
            "network providers not built (enable feature `net`)".into(),
        ));
    }
    ni("AI provider call")
}

/* ============================ 18. AGENT FRAMEWORK ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Document,
    Spreadsheet,
    Presentation,
    Pdf,
    Image,
    Design,
    Video,
    Audio,
    Research,
    Conversion,
    Publishing,
    Qa,
}

#[derive(Debug, Clone)]
pub struct AgentIdentity {
    pub id: String,
    pub role: AgentRole,
    pub can_write: bool,
    pub approval_required: bool,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ToolCall {
    Document(String),
    Spreadsheet(String),
    Presentation(String),
    Pdf(String),
    Image(String),
    Design(String),
    Video(String),
    Audio(String),
    Convert(ConversionKind),
}

impl ToolCall {
    /// Scope an agent needs for this call.
    #[must_use]
    pub fn scope(&self) -> &'static str {
        match self {
            Self::Document(_) => "document",
            Self::Spreadsheet(_) => "spreadsheet",
            Self::Presentation(_) => "presentation",
            Self::Pdf(_) => "pdf",
            Self::Image(_) => "image",
            Self::Design(_) => "design",
            Self::Video(_) => "video",
            Self::Audio(_) => "audio",
            Self::Convert(_) => "conversion",
        }
    }
}

/// Run a tool for an agent. Checks: scope, write permission, human approval.
/// Spreadsheet calls (`set A1 <raw>` / `get A1`) run against `sheet`.
pub fn execute_tool(
    agent: &AgentIdentity,
    tool: &ToolCall,
    human_approved: bool,
    sheet: Option<&mut SpreadsheetDoc>,
) -> MouliResult<String> {
    if !agent.scopes.iter().any(|s| s == tool.scope()) {
        return Err(MouliError::PermissionDenied(format!(
            "agent lacks scope {}",
            tool.scope()
        )));
    }
    let is_read = matches!(tool, ToolCall::Spreadsheet(op) if op.starts_with("get "));
    if !is_read && !agent.can_write {
        return Err(MouliError::PermissionDenied("read-only agent".into()));
    }
    if !is_read && agent.approval_required && !human_approved {
        return Err(MouliError::PermissionDenied(
            "human approval required".into(),
        ));
    }
    match tool {
        ToolCall::Spreadsheet(op) => {
            let sheet = sheet.ok_or_else(|| MouliError::NotFound("no open spreadsheet".into()))?;
            let parts: Vec<&str> = op.splitn(3, ' ').collect();
            match parts.as_slice() {
                ["get", cell] => Ok(sheet.get(cell)?.display()),
                ["set", cell, raw] => {
                    sheet.set(cell, raw)?;
                    Ok(sheet.get(cell)?.display())
                }
                _ => Err(MouliError::Validation(format!(
                    "unknown spreadsheet op {op}"
                ))),
            }
        }
        ToolCall::Document(_) => ni("document agent op"),
        ToolCall::Presentation(_) => ni("presentation agent op"),
        ToolCall::Pdf(_) => ni("pdf agent op"),
        ToolCall::Image(_) => ni("image agent op"),
        ToolCall::Design(_) => ni("design agent op"),
        ToolCall::Video(_) => ni("video agent op"),
        ToolCall::Audio(_) => ni("audio agent op"),
        ToolCall::Convert(_) => ni("conversion agent op"),
    }
}

/* ============================ 19. UI SHELL ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiPanels {
    pub properties: bool,
    pub layers: bool,
    pub timeline: bool,
    pub history: bool,
    pub ai: bool,
}

impl Default for UiPanels {
    fn default() -> Self {
        Self {
            properties: true,
            layers: false,
            timeline: false,
            history: true,
            ai: true,
        }
    }
}

pub fn launch_gui() -> MouliResult<()> {
    ni("desktop GUI (eframe not linked in the scaffold)")
}

/* ============================ 20. HEADLESS / API ============================ */

pub const MODES: &[Mode] = &[
    Mode::Desktop,
    Mode::Document,
    Mode::Spreadsheet,
    Mode::Presentation,
    Mode::Pdf,
    Mode::Image,
    Mode::Video,
    Mode::Audio,
    Mode::Batch,
    Mode::AiCreation,
    Mode::HeadlessExport,
    Mode::Api,
];

pub fn run_api(_port: u16) -> MouliResult<()> {
    ni("embedded API")
}

pub fn run_batch(_dir: &Path, _op: &str) -> MouliResult<()> {
    ni("batch processing")
}

/* ============================ 21. TESTS ============================ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_uses_full_sheet_engine() {
        assert_eq!(calc("1+2*3"), CellValue::Number(7.0));
        assert_eq!(
            calc("=IF(2>1,\"yes\",\"no\")"),
            CellValue::Text("yes".into())
        );
        assert_eq!(calc("1/0"), CellValue::Error("#DIV/0!".into()));
    }

    #[test]
    fn spreadsheet_container_roundtrip() {
        let mut s = SpreadsheetDoc::default();
        s.set("A1", "2").unwrap();
        s.set("A2", "=A1*10").unwrap();
        s.set("B1", "multi\nline \\ text").unwrap();
        let text = s.to_container();
        let back = SpreadsheetDoc::from_container(&text).unwrap();
        assert_eq!(back.get("A2").unwrap(), CellValue::Number(20.0));
        assert_eq!(
            back.get("B1").unwrap(),
            CellValue::Text("multi\nline \\ text".into())
        );
        assert_eq!(back.to_container(), text);
        assert!(SpreadsheetDoc::from_container("MOULISTUDIO-1\nkind=document\n").is_err());
        assert!(
            SpreadsheetDoc::from_container("MOULISTUDIO-1\nkind=spreadsheet\nC:0:A1:1\n").is_err()
        );
    }

    #[test]
    fn document_roundtrip_and_open_any() {
        let d = Document {
            blocks: vec![
                Block::Heading("T".into()),
                Block::Paragraph("a\nb\\c".into()),
            ],
        };
        assert_eq!(Document::from_container(&d.to_container()).unwrap(), d);
        let dir = std::env::temp_dir().join(format!("mouli-studio-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("doc.moulistudio");
        d.save(&p).unwrap();
        assert!(matches!(open_any(&p).unwrap(), Opened::Document(_)));
        let q = dir.join("sheet.moulistudio");
        SpreadsheetDoc::default().save(&q).unwrap();
        assert!(matches!(open_any(&q).unwrap(), Opened::Spreadsheet(_)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stubs_fail_closed() {
        let mut d = PdfDocument::default();
        assert!(matches!(
            d.redact_permanent((0.0, 0.0, 1.0, 1.0), 0),
            Err(MouliError::NotImplemented(_))
        ));
        assert!(matches!(
            ImageDocument::default().generative_fill("sky", false),
            Err(MouliError::ConsentRequired(_))
        ));
        assert!(launch_gui().is_err());
    }

    #[test]
    fn ai_gates() {
        let remote = ProviderConfig {
            kind: AiProviderKind::OpenAiCompatible,
            endpoint: "https://example".into(),
            local_only: false,
        };
        let mut req = AiRequest {
            system: String::new(),
            messages: vec![],
            max_tokens: 16,
            local_only: true,
            external_consent: true,
        };
        assert!(matches!(
            chat(&remote, &req),
            Err(MouliError::PermissionDenied(_))
        ));
        req.local_only = false;
        req.external_consent = false;
        assert!(matches!(
            chat(&remote, &req),
            Err(MouliError::ConsentRequired(_))
        ));
        let local = ProviderConfig {
            kind: AiProviderKind::Ollama,
            endpoint: "http://127.0.0.1:11434".into(),
            local_only: true,
        };
        req.local_only = true;
        assert!(matches!(
            chat(&local, &req),
            Err(MouliError::NotImplemented(_))
        ));
    }

    #[test]
    fn agent_gates_and_spreadsheet_tool() {
        let reader = AgentIdentity {
            id: "qa".into(),
            role: AgentRole::Qa,
            can_write: false,
            approval_required: true,
            scopes: vec!["spreadsheet".into()],
        };
        let mut sheet = SpreadsheetDoc::default();
        sheet.set("A1", "5").unwrap();
        assert_eq!(
            execute_tool(
                &reader,
                &ToolCall::Spreadsheet("get A1".into()),
                false,
                Some(&mut sheet)
            )
            .unwrap(),
            "5"
        );
        assert!(execute_tool(
            &reader,
            &ToolCall::Spreadsheet("set A1 9".into()),
            true,
            Some(&mut sheet)
        )
        .is_err());
        assert!(execute_tool(&reader, &ToolCall::Document("create".into()), true, None).is_err());

        let writer = AgentIdentity {
            id: "calc".into(),
            role: AgentRole::Spreadsheet,
            can_write: true,
            approval_required: true,
            scopes: vec!["spreadsheet".into()],
        };
        let op = ToolCall::Spreadsheet("set A2 =A1*2".into());
        assert!(matches!(
            execute_tool(&writer, &op, false, Some(&mut sheet)),
            Err(MouliError::PermissionDenied(_))
        ));
        assert_eq!(
            execute_tool(&writer, &op, true, Some(&mut sheet)).unwrap(),
            "10"
        );
    }

    #[test]
    fn workspace_tabs_animation_and_assets() {
        let mut w = Workspace::default();
        let a = w.open_tab(ContentKind::Document, None);
        let b = w.open_tab(ContentKind::Spreadsheet, None);
        assert!(w.close_tab(b));
        assert_eq!(w.active, Some(a));
        let c = w.open_tab(ContentKind::Video, None);
        assert_ne!(c, b, "tab ids are not reused");
        w.toggle_theme();
        assert_eq!(w.theme, Theme::Light);

        let scene = AnimationScene {
            keyframes: vec![
                Keyframe {
                    tick: 0,
                    property: AnimProperty::Opacity,
                    value: 0.0,
                },
                Keyframe {
                    tick: 10,
                    property: AnimProperty::Opacity,
                    value: 1.0,
                },
            ],
        };
        assert_eq!(scene.sample(AnimProperty::Opacity, 5), Some(0.5));
        assert_eq!(scene.sample(AnimProperty::Opacity, 99), Some(1.0));
        assert_eq!(scene.sample(AnimProperty::Scale, 5), None);

        let asset = |id: &str, hash: &str| AssetRecord {
            id: id.into(),
            kind: AssetKind::Image,
            path: PathBuf::from(id),
            hash: hash.into(),
            tags: vec![],
            collections: vec![],
            version: 1,
        };
        let groups = duplicates_by_hash(&[asset("a", "h1"), asset("b", "h2"), asset("c", "h1")]);
        assert_eq!(groups, vec![vec!["a".to_string(), "c".to_string()]]);
        assert_eq!(Timeline::default().fps, 30.0);
    }
}
