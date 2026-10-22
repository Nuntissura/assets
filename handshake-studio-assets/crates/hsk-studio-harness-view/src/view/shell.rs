//! Desktop shell of the viewer (lane SMB-HARNESS-SHELL-1): a menu bar (FILE, EDIT, VIEW, WORKSPACE),
//! keyboard shortcuts, the in-crate file dialog ([`super::file_dialog`]) for open / save / export,
//! a recent-files list, and the WORKSPACE panel toggles that host every coded surface in the one
//! window: layers, properties, develop, the layout editor, program monitor, source monitor, timeline
//! (lane SMB-TIMELINE-UI-1), mixer, waveform editor, spectral display and prototype player.
//!
//! Names follow the Studio menu law (spec 14.30, [STU-SHL-008] / [STU-SHL-010] / [STU-SHL-023] /
//! [STU-SHL-024] / [STU-SHL-036]): menu titles FILE, EDIT, VIEW, WORKSPACE (panel show/hide toggles
//! live under WORKSPACE > Panels); command ids reuse the spec's `studio.document.*`,
//! `studio.history.*`, `studio.interop.export`, `studio.view.*` and `studio.workspace.panel` roots
//! (the leaf suffixes are harness ids). Every entry calls an existing Viewer method; nothing here
//! adds engine behaviour. A surface whose cargo feature is not in the build stays in the menu,
//! greyed, its label naming the feature ([`entry_label`]); a surface whose precondition is unmet
//! (no camera raw, no sequence, no audio) is greyed with the reason as hover text, and a command
//! refused anyway says why in the status log (never silent).
//!
//! Model navigation: [`Viewer::shell_command`] runs any entry headlessly, every entry has the
//! stable widget name `menu.<command id>` ([`widget::entry`]), and `Viewer::state_json()["shell"]`
//! projects the panels (open, visible, availability), the recent list and the open dialog.
//!
//! Lane SMB-HARNESS-SHELL-2: the menu tree is ONE set of controls descriptors
//! ([`Viewer::menu_bar`], `hsk_studio_controls::menu`, [STU-ARC-014]) and that one availability
//! evaluation feeds the egui menu (greyed with the reason line, never hidden, the remedy of an
//! `INAPPLICABLE_HERE` leaf drawn as an enabled sibling leaf: [STU-SHL-018] / [STU-SHL-020]), the
//! AccessKit description of every node (feature `desktop`), the command API ([`Viewer::shell_command`]
//! refuses with the same reason line, [STU-SHL-019] / MENU-INV-4), `state_json()["shell"]["menu"]` and
//! the manual. HELP carries the leaves of [STU-SHL-037]; the ones the viewer can serve (Open Studio
//! Manual, Search Manual, Full Command Index, Keyboard Shortcut Reference, System Report, About
//! Studio) open the in-window help view ([`HelpPage`]), the others stay in the menu greyed
//! `NOT_IMPLEMENTED_YET` ([STU-SHL-018]: never hidden). No vendor product name in any label or id
//! ([STU-SHL-008], audited by `MenuBar::audit`).
#![deny(clippy::arithmetic_side_effects)]

use super::file_dialog::{FileDialog, Outcome, Purpose};
use super::widget as vw;
use super::{Viewer, export::ExportFormat, render::Prefer};
use crate::layout_ui::{LayoutDoc, LayoutEditor};
use super::hosted::HostedPanel;
use hsk_studio_controls::menu::{self as cm, MenuBar, MenuNode, ReasonCode, WindowRole, WindowSpec};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Recent files kept (most recent first).
pub const MAX_RECENT: usize = 10;
/// Schema of the recent-files list bytes ([`encode_recent`] / [`decode_recent`]).
pub const RECENT_SCHEMA: &str = "hsk.studio.harness.recent@1";
/// Largest recent-files document accepted by [`decode_recent`].
pub const MAX_RECENT_BYTES: usize = 64 * 1024;
/// Raster size of monitors opened from the menu (pixels).
pub const MONITOR_WIDTH: u32 = 640;
pub const MONITOR_HEIGHT: u32 = 360;
/// Mixer stream opened from the menu: 48 kHz, 20 ms blocks (the mixer lane's test rate).
#[cfg(feature = "audio-batch")]
const MIXER_RATE: u32 = 48_000;
#[cfg(feature = "audio-batch")]
const MIXER_BLOCK: usize = 960;

/// Stable widget names of the shell.
pub mod widget {
    pub const MENU_BAR: &str = "menu";
    pub const FILE: &str = "menu.file";
    pub const EDIT: &str = "menu.edit";
    pub const VIEW: &str = "menu.view";
    pub const WORKSPACE: &str = "menu.workspace";
    pub const HELP: &str = "menu.help";
    pub const OPEN_RECENT: &str = "menu.studio.document.open_recent";
    /// WORKSPACE > Panels ([STU-SHL-036] expansion node `studio.workspace.panel`).
    pub const PANELS: &str = "menu.studio.workspace.panel";
    /// HELP > Studio Manual and HELP > Diagnostics submenus ([STU-SHL-037]).
    pub const HELP_MANUAL: &str = "menu.studio.help.manual";
    pub const HELP_DIAGNOSTICS: &str = "menu.studio.help.diagnostics";
    /// The help view window and its items.
    pub const HELP_WINDOW: &str = "help";
    pub const HELP_SEARCH: &str = "help.search";
    pub const HELP_ROWS: &str = "help.rows";
    pub const HELP_CLOSE: &str = "help.close";
    /// One menu entry: `menu.<command id>`.
    pub fn entry(command_id: &str) -> String {
        format!("menu.{command_id}")
    }
}

/// The surfaces WORKSPACE > Panels shows and hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Panel {
    Layers,
    Properties,
    Develop,
    LayoutEditor,
    ProgramMonitor,
    SourceMonitor,
    Timeline,
    Mixer,
    WaveformEditor,
    SpectralDisplay,
    PrototypePlayer,
    /// A panel hosted by module [`super::hosted`] (lane SMB-HARNESS-PANELS-1).
    Hosted(HostedPanel),
}

impl Panel {
    pub const ALL: [Panel; 24] = [
        Panel::Layers,
        Panel::Properties,
        Panel::Develop,
        Panel::LayoutEditor,
        Panel::ProgramMonitor,
        Panel::SourceMonitor,
        Panel::Timeline,
        Panel::Mixer,
        Panel::WaveformEditor,
        Panel::SpectralDisplay,
        Panel::PrototypePlayer,
        Panel::Hosted(HostedPanel::Appearance),
        Panel::Hosted(HostedPanel::Stroke),
        Panel::Hosted(HostedPanel::Swatches),
        Panel::Hosted(HostedPanel::GraphicStyles),
        Panel::Hosted(HostedPanel::AlignDistribute),
        Panel::Hosted(HostedPanel::Transform),
        Panel::Hosted(HostedPanel::PathOperations),
        Panel::Hosted(HostedPanel::Masks),
        Panel::Hosted(HostedPanel::EnvelopeDistort),
        Panel::Hosted(HostedPanel::Brushes),
        Panel::Hosted(HostedPanel::VectorExport),
        Panel::Hosted(HostedPanel::VectorImport),
        Panel::Hosted(HostedPanel::AudioBatch),
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Layers => "layers",
            Self::Properties => "properties",
            Self::Develop => "develop",
            Self::LayoutEditor => "layout_editor",
            Self::ProgramMonitor => "program_monitor",
            Self::SourceMonitor => "source_monitor",
            Self::Timeline => "timeline",
            Self::Mixer => "mixer",
            Self::WaveformEditor => "waveform_editor",
            Self::SpectralDisplay => "spectral_display",
            Self::PrototypePlayer => "prototype_player",
            Self::Hosted(h) => h.key(),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Layers => "Layers",
            Self::Properties => "Properties",
            Self::Develop => "Develop",
            Self::LayoutEditor => "Layout Editor",
            Self::ProgramMonitor => "Program Monitor",
            Self::SourceMonitor => "Source Monitor",
            Self::Timeline => "Timeline",
            Self::Mixer => "Mixer",
            Self::WaveformEditor => "Waveform Editor",
            Self::SpectralDisplay => "Spectral Display",
            Self::PrototypePlayer => "Prototype Player",
            Self::Hosted(h) => h.label(),
        }
    }
    /// The cargo feature the surface needs (beyond `viewer`), if any.
    pub fn feature(self) -> Option<&'static str> {
        match self {
            Self::SourceMonitor => Some("media"),
            Self::Mixer | Self::WaveformEditor | Self::SpectralDisplay => Some("audio-batch"),
            Self::Hosted(h) => h.feature(),
            _ => None,
        }
    }
    /// Function key that toggles the surface (layout-independent keys).
    pub fn key_code(self) -> egui::Key {
        match self {
            Self::Layers => egui::Key::F2,
            Self::Properties => egui::Key::F3,
            Self::Develop => egui::Key::F4,
            Self::LayoutEditor => egui::Key::F5,
            Self::ProgramMonitor => egui::Key::F6,
            Self::SourceMonitor => egui::Key::F7,
            Self::Timeline => egui::Key::F10,
            Self::Mixer => egui::Key::F8,
            Self::WaveformEditor => egui::Key::F9,
            Self::SpectralDisplay => egui::Key::F11,
            Self::PrototypePlayer => egui::Key::F12,
            Self::Hosted(h) => h.key_code(),
        }
    }
    pub fn shortcut_text(self) -> &'static str {
        match self {
            Self::Layers => "F2",
            Self::Properties => "F3",
            Self::Develop => "F4",
            Self::LayoutEditor => "F5",
            Self::ProgramMonitor => "F6",
            Self::SourceMonitor => "F7",
            Self::Timeline => "F10",
            Self::Mixer => "F8",
            Self::WaveformEditor => "F9",
            Self::SpectralDisplay => "F11",
            Self::PrototypePlayer => "F12",
            Self::Hosted(h) => h.shortcut_text(),
        }
    }
    /// Modifiers of the toggle key (none for the shell's own panels).
    pub fn modifiers(self) -> egui::Modifiers {
        match self {
            Self::Hosted(h) => h.modifiers(),
            _ => egui::Modifiers::NONE,
        }
    }
}

/// The features compiled into this build that gate shell surfaces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompiledFeatures {
    pub media: bool,
    pub audio_batch: bool,
}

impl CompiledFeatures {
    pub fn current() -> Self {
        Self { media: cfg!(feature = "media"), audio_batch: cfg!(feature = "audio-batch") }
    }
    pub fn has(&self, feature: &str) -> bool {
        match feature {
            "media" => self.media,
            "audio-batch" => self.audio_batch,
            _ => false,
        }
    }
}

/// Whether a menu entry can run now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// The build lacks this cargo feature.
    NeedsFeature(&'static str),
    /// The precondition is unmet (the reason).
    Unavailable(String),
}

impl Availability {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NeedsFeature(_) => "needs_feature",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

/// The feature gate of `panel` in a build with `compiled` features.
pub fn feature_gate(panel: Panel, compiled: &CompiledFeatures) -> Availability {
    match panel.feature() {
        Some(f) if !compiled.has(f) => Availability::NeedsFeature(f),
        _ => Availability::Available,
    }
}

/// The menu label of `panel`: a greyed feature-gated entry names its feature.
pub fn entry_label(panel: Panel, availability: &Availability) -> String {
    match availability {
        Availability::NeedsFeature(f) => format!("{} (needs feature {f})", panel.label()),
        _ => panel.label().to_owned(),
    }
}

/// HELP leaves ([STU-SHL-037]: Studio Manual submenu, Full Command Index, Keyboard Shortcut
/// Reference, Diagnostics submenu, About Studio). The spec gives labels but no command ids for
/// HELP; the `studio.help.*` ids are harness ids (no canonical id verified).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpCommand {
    OpenManual,
    SearchManual,
    ForCommand,
    ForPanel,
    ForTool,
    DomainGuides,
    CommandIndex,
    ShortcutReference,
    RenderHarness,
    VisualDebugger,
    FlightRecorder,
    ValidationReport,
    SystemReport,
    GpuReport,
    AccessibilitySnapshot,
    About,
}

/// Node id of HELP > Studio Manual.
pub const HELP_MANUAL_ID: &str = "studio.help.manual";
/// Node id of HELP > Diagnostics.
pub const HELP_DIAGNOSTICS_ID: &str = "studio.help.diagnostics";

impl HelpCommand {
    /// Menu order ([STU-SHL-037]).
    pub const ALL: [HelpCommand; 16] = [
        Self::OpenManual,
        Self::SearchManual,
        Self::ForCommand,
        Self::ForPanel,
        Self::ForTool,
        Self::DomainGuides,
        Self::CommandIndex,
        Self::ShortcutReference,
        Self::RenderHarness,
        Self::VisualDebugger,
        Self::FlightRecorder,
        Self::ValidationReport,
        Self::SystemReport,
        Self::GpuReport,
        Self::AccessibilitySnapshot,
        Self::About,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenManual => "studio.help.manual.open",
            Self::SearchManual => "studio.help.manual.search",
            Self::ForCommand => "studio.help.manual.this_command",
            Self::ForPanel => "studio.help.manual.this_panel",
            Self::ForTool => "studio.help.manual.this_tool",
            Self::DomainGuides => "studio.help.manual.domain_guides",
            Self::CommandIndex => "studio.help.command_index",
            Self::ShortcutReference => "studio.help.shortcut_reference",
            Self::RenderHarness => "studio.help.diagnostics.render_harness",
            Self::VisualDebugger => "studio.help.diagnostics.visual_debugger",
            Self::FlightRecorder => "studio.help.diagnostics.flight_recorder",
            Self::ValidationReport => "studio.help.diagnostics.validation_report",
            Self::SystemReport => "studio.help.diagnostics.system_report",
            Self::GpuReport => "studio.help.diagnostics.gpu_report",
            Self::AccessibilitySnapshot => "studio.help.diagnostics.accessibility_snapshot",
            Self::About => "studio.help.about",
        }
    }
    /// The spec label.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenManual => "Open Studio Manual",
            Self::SearchManual => "Search Manual…",
            Self::ForCommand => "Help for This Command",
            Self::ForPanel => "Help for This Panel",
            Self::ForTool => "Help for This Tool",
            Self::DomainGuides => "Domain Guides…",
            Self::CommandIndex => "Full Command Index…",
            Self::ShortcutReference => "Keyboard Shortcut Reference…",
            Self::RenderHarness => "Render Harness…",
            Self::VisualDebugger => "Visual Debugger",
            Self::FlightRecorder => "Flight Recorder",
            Self::ValidationReport => "Validation Report…",
            Self::SystemReport => "System Report…",
            Self::GpuReport => "GPU Report…",
            Self::AccessibilitySnapshot => "Accessibility Tree Snapshot…",
            Self::About => "About Studio",
        }
    }
    /// The submenu holding the leaf (`None`: directly under HELP).
    pub fn group(self) -> Option<&'static str> {
        match self {
            Self::OpenManual | Self::SearchManual | Self::ForCommand | Self::ForPanel | Self::ForTool | Self::DomainGuides => Some(HELP_MANUAL_ID),
            Self::RenderHarness
            | Self::VisualDebugger
            | Self::FlightRecorder
            | Self::ValidationReport
            | Self::SystemReport
            | Self::GpuReport
            | Self::AccessibilitySnapshot => Some(HELP_DIAGNOSTICS_ID),
            Self::CommandIndex | Self::ShortcutReference | Self::About => None,
        }
    }
    /// The help view the leaf opens; `None`: not implemented in the viewer (greyed
    /// `NOT_IMPLEMENTED_YET`, never hidden).
    pub fn page(self) -> Option<HelpPage> {
        match self {
            Self::OpenManual | Self::SearchManual => Some(HelpPage::Manual),
            Self::CommandIndex => Some(HelpPage::CommandIndex),
            Self::ShortcutReference => Some(HelpPage::Shortcuts),
            Self::SystemReport => Some(HelpPage::SystemReport),
            Self::About => Some(HelpPage::About),
            _ => None,
        }
    }
    /// Menu-only per [STU-SHL-037] (Full Command Index and the Diagnostics leaves).
    pub fn menu_only(self) -> bool {
        self == Self::CommandIndex || self.group() == Some(HELP_DIAGNOSTICS_ID)
    }
    /// Why an unimplemented leaf is greyed (reason text of `NOT_IMPLEMENTED_YET`).
    pub fn missing(self) -> &'static str {
        match self {
            Self::ForCommand | Self::ForPanel | Self::ForTool => "context help needs a focused element and per-element manual anchors; the viewer has neither yet",
            Self::DomainGuides => "no domain guides are written for the viewer yet",
            Self::RenderHarness => "the render harness report is not wired into the viewer yet",
            Self::VisualDebugger => "the viewer has no visual debugger surface yet",
            Self::FlightRecorder => "the viewer has no flight recorder binding yet",
            Self::ValidationReport => "the viewer has no validation report surface yet",
            Self::GpuReport => "the viewer does not expose the GPU adapter report yet",
            Self::AccessibilitySnapshot => "the AccessKit tree is emitted to the host with each frame; the viewer does not snapshot it yet (its widget names: Viewer::drawn)",
            _ => "",
        }
    }
}

/// The page the in-window help view shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpPage {
    /// The manual: menu, window, panel-control and command-line topics, filtered by the query.
    Manual,
    /// Every menu node with its path, command id, shortcut and availability.
    CommandIndex,
    /// Every menu shortcut.
    Shortcuts,
    /// Build: package version, compiled features, render preference, crate descriptors.
    SystemReport,
    /// Name, version and build features.
    About,
}

impl HelpPage {
    pub fn key(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::CommandIndex => "command_index",
            Self::Shortcuts => "shortcuts",
            Self::SystemReport => "system_report",
            Self::About => "about",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Manual => "Studio Manual",
            Self::CommandIndex => "Full Command Index",
            Self::Shortcuts => "Keyboard Shortcut Reference",
            Self::SystemReport => "System Report",
            Self::About => "About Studio",
        }
    }
}

/// Most rows one help page holds (the rest are reported as truncated).
pub const MAX_HELP_ROWS: usize = 4096;
/// Longest manual search query, in bytes.
pub const MAX_HELP_QUERY_BYTES: usize = 256;

/// One help row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpRow {
    pub id: String,
    pub title: String,
    pub body: String,
}

/// The open help view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpState {
    pub page: HelpPage,
    /// Manual search text (empty: every topic).
    pub query: String,
}

/// Validates a manual search query: refused `help_query_too_long` (over
/// [`MAX_HELP_QUERY_BYTES`]) or `help_query_control_character`. Kept as typed (blanks are
/// ignored when matching).
pub fn parse_help_query(text: &str) -> Result<String, &'static str> {
    if text.len() > MAX_HELP_QUERY_BYTES {
        return Err("help_query_too_long");
    }
    if text.chars().any(char::is_control) {
        return Err("help_query_control_character");
    }
    Ok(text.to_owned())
}

/// Cargo features of this crate compiled into the build (system report, about).
pub fn compiled_feature_names() -> Vec<&'static str> {
    let all = [
        ("viewer", cfg!(feature = "viewer")),
        ("desktop", cfg!(feature = "desktop")),
        ("layout-ui", cfg!(feature = "layout-ui")),
        ("vector-interop", cfg!(feature = "vector-interop")),
        ("import-psd", cfg!(feature = "import-psd")),
        ("import-raw", cfg!(feature = "import-raw")),
        ("import-tiff", cfg!(feature = "import-tiff")),
        ("import-pdf", cfg!(feature = "import-pdf")),
        ("import-raster", cfg!(feature = "import-raster")),
        ("import-affinity", cfg!(feature = "import-affinity")),
        ("media", cfg!(feature = "media")),
        ("media-native", cfg!(feature = "media-native")),
        ("os-encode", cfg!(feature = "os-encode")),
        ("audio-device", cfg!(feature = "audio-device")),
        ("audio-batch", cfg!(feature = "audio-batch")),
        ("audio-plugin", cfg!(feature = "audio-plugin")),
        ("studio-aac", cfg!(feature = "studio-aac")),
    ];
    all.iter().filter(|(_, on)| *on).map(|(n, _)| *n).collect()
}

/// The descriptor of the help view window.
pub fn help_window_spec(page: HelpPage) -> WindowSpec {
    let mut w = WindowSpec::new(
        widget::HELP_WINDOW,
        page.title(),
        "In-window help view opened from the HELP menu: manual topics, command index, shortcut reference, system report or about.",
    );
    if page == HelpPage::Manual {
        w = w.with(widget::HELP_SEARCH, "search the manual", WindowRole::Field, "Filters the topics: case-insensitive text match in title and body.");
    }
    w.with(widget::HELP_ROWS, "topics", WindowRole::List, "The rows of the page.").with(widget::HELP_CLOSE, "Close", WindowRole::Action, "Closes the help view.")
}

/// A shell command (one menu entry or shortcut).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellCommand {
    Open,
    OpenRecent(usize),
    ClearRecent,
    Save,
    SaveAs,
    ExportPng,
    ExportJpeg,
    Close,
    Undo,
    Redo,
    Fit,
    ActualSize,
    ZoomIn,
    ZoomOut,
    ToggleGpu,
    ToggleStatus,
    Panel(Panel),
    Help(HelpCommand),
}

impl ShellCommand {
    /// Stable command id (spec 14.30 roots; harness leaf suffixes).
    pub fn id(self) -> String {
        match self {
            Self::Open => "studio.document.open".into(),
            Self::OpenRecent(i) => format!("studio.document.open_recent.{i}"),
            Self::ClearRecent => "studio.document.open_recent.clear".into(),
            Self::Save => "studio.document.save".into(),
            Self::SaveAs => "studio.document.save_as".into(),
            Self::ExportPng => "studio.interop.export.png".into(),
            Self::ExportJpeg => "studio.interop.export.jpeg".into(),
            Self::Close => "studio.document.close".into(),
            Self::Undo => "studio.history.undo".into(),
            Self::Redo => "studio.history.redo".into(),
            Self::Fit => "studio.view.zoom_to.fit".into(),
            Self::ActualSize => "studio.view.zoom_to.actual_size".into(),
            Self::ZoomIn => "studio.view.zoom_in".into(),
            Self::ZoomOut => "studio.view.zoom_out".into(),
            Self::ToggleGpu => "studio.view.render_gpu".into(),
            Self::ToggleStatus => "studio.view.status_log".into(),
            Self::Panel(p) => format!("studio.workspace.panel.{}", p.key()),
            Self::Help(h) => h.id().into(),
        }
    }

    /// The command of a menu leaf id (`None`: not a leaf of the shell).
    pub fn from_id(id: &str) -> Option<ShellCommand> {
        if let Some(i) = id.strip_prefix("studio.document.open_recent.").and_then(|i| i.parse::<usize>().ok()) {
            return Some(Self::OpenRecent(i));
        }
        menu_entries().into_iter().map(|e| e.command).find(|c| c.id() == id)
    }
}

/// One fixed menu entry (Open Recent's generated leaves are not listed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub menu: &'static str,
    pub command: ShellCommand,
    pub label: &'static str,
    pub shortcut: Option<&'static str>,
}

/// The fixed menu entries in menu order.
pub fn menu_entries() -> Vec<MenuEntry> {
    let e = |menu, command, label, shortcut| MenuEntry { menu, command, label, shortcut };
    let mut out = vec![
        e("File", ShellCommand::Open, "Open…", Some("Ctrl+O")),
        e("File", ShellCommand::ClearRecent, "Clear Recent", None),
        e("File", ShellCommand::Save, "Save", Some("Ctrl+S")),
        e("File", ShellCommand::SaveAs, "Save As…", Some("Ctrl+Shift+S")),
        e("File", ShellCommand::ExportPng, "Export PNG…", None),
        e("File", ShellCommand::ExportJpeg, "Export JPEG…", None),
        e("File", ShellCommand::Close, "Close", Some("Ctrl+W")),
        e("Edit", ShellCommand::Undo, "Undo", Some("Ctrl+Z")),
        e("Edit", ShellCommand::Redo, "Redo", Some("Ctrl+Y")),
        e("View", ShellCommand::Fit, "Fit in Window", None),
        e("View", ShellCommand::ActualSize, "Actual Size", None),
        e("View", ShellCommand::ZoomIn, "Zoom In", None),
        e("View", ShellCommand::ZoomOut, "Zoom Out", None),
        e("View", ShellCommand::ToggleGpu, "Render on GPU", None),
        e("View", ShellCommand::ToggleStatus, "Status Log", None),
    ];
    out.extend(Panel::ALL.map(|p| e("Workspace", ShellCommand::Panel(p), p.label(), Some(p.shortcut_text()))));
    out.extend(HelpCommand::ALL.map(|h| e("Help", ShellCommand::Help(h), h.label(), None)));
    out
}

/// Encodes the recent list (`{"schema": RECENT_SCHEMA, "paths": [...]}`).
pub fn encode_recent(paths: &[PathBuf]) -> Vec<u8> {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    serde_json::to_vec(&json!({"schema": RECENT_SCHEMA, "paths": paths})).unwrap_or_default()
}

/// Decodes recent-list bytes. Refused: `recent_too_large` (over [`MAX_RECENT_BYTES`]),
/// `recent_json`, `recent_schema`, `recent_keys` (anything but schema and paths),
/// `recent_too_many` (over [`MAX_RECENT`]), `recent_path` (not a string, or refused by
/// [`super::file_dialog::parse_typed`]). Duplicates are dropped, order kept.
pub fn decode_recent(bytes: &[u8]) -> Result<Vec<PathBuf>, &'static str> {
    if bytes.len() > MAX_RECENT_BYTES {
        return Err("recent_too_large");
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "recent_json")?;
    let o = v.as_object().ok_or("recent_json")?;
    if o.keys().any(|k| k != "schema" && k != "paths") {
        return Err("recent_keys");
    }
    if o.get("schema").and_then(Value::as_str) != Some(RECENT_SCHEMA) {
        return Err("recent_schema");
    }
    let list = o.get("paths").and_then(Value::as_array).ok_or("recent_json")?;
    if list.len() > MAX_RECENT {
        return Err("recent_too_many");
    }
    let mut out: Vec<PathBuf> = Vec::with_capacity(list.len());
    for p in list {
        let text = p.as_str().ok_or("recent_path")?;
        let path = super::file_dialog::parse_typed(text).map_err(|_| "recent_path")?;
        if !out.contains(&path) {
            out.push(path);
        }
    }
    Ok(out)
}

/// Shell state owned by the viewer.
pub struct ShellState {
    hidden: Vec<Panel>,
    show_status: bool,
    layout: Option<LayoutEditor>,
    dialog: Option<FileDialog>,
    recent: Vec<PathBuf>,
    recent_generation: u64,
    /// Form state of the hosted panels (module [`super::hosted`]).
    pub(super) hosted: super::hosted::HostedState,
    help: Option<HelpState>,
}

impl Default for ShellState {
    fn default() -> Self {
        Self { hidden: Vec::new(), show_status: true, layout: None, dialog: None, recent: Vec::new(), recent_generation: 0, hosted: Default::default(), help: None }
    }
}

impl ShellState {
    pub(super) fn visible(&self, panel: Panel) -> bool {
        !self.hidden.contains(&panel)
    }
    pub(super) fn show_status(&self) -> bool {
        self.show_status
    }
    /// Puts `path` first in the recent list.
    pub(super) fn remember(&mut self, path: &Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
        self.recent_generation = self.recent_generation.wrapping_add(1);
    }
    /// Closes the document-bound shell surfaces (the open dialog and the layout editor).
    pub(super) fn close_document(&mut self) {
        self.layout = None;
        self.dialog = None;
        self.hosted.close_document();
    }
    /// Key state of the hosted layout editor.
    pub(super) fn layout_state(&self) -> Option<Value> {
        self.layout.as_ref().map(LayoutEditor::state_json)
    }
}

/// Mixer sources when no audio file is open: every clip is silent.
#[cfg(feature = "audio-batch")]
#[derive(Clone, Copy)]
struct NoAudio;
#[cfg(feature = "audio-batch")]
impl hsk_studio_pulse::AudioSources for NoAudio {
    fn audio(&self, _: &hsk_studio_folio::ClipSource) -> Option<&[Vec<f32>]> {
        None
    }
}

impl Viewer {
    /// The recent files, most recent first.
    pub fn recent_files(&self) -> &[PathBuf] {
        &self.shell.recent
    }
    /// Changes whenever the recent list changes (hosts persist the list on change).
    pub fn recent_generation(&self) -> u64 {
        self.shell.recent_generation
    }
    /// The recent list as bytes ([`encode_recent`]).
    pub fn recent_bytes(&self) -> Vec<u8> {
        encode_recent(&self.shell.recent)
    }
    /// Replaces the recent list from bytes ([`decode_recent`]); returns the number of paths.
    pub fn load_recent(&mut self, bytes: &[u8]) -> Result<usize, &'static str> {
        let list = decode_recent(bytes)?;
        let n = list.len();
        self.shell.recent = list;
        self.shell.recent_generation = self.shell.recent_generation.wrapping_add(1);
        Ok(n)
    }

    /// The open file dialog.
    pub fn file_dialog(&mut self) -> Option<&mut FileDialog> {
        self.shell.dialog.as_mut()
    }

    /// The hosted layout editor (WORKSPACE > Panels > Layout Editor).
    pub fn layout_editor(&mut self) -> Option<&mut LayoutEditor> {
        self.shell.layout.as_mut()
    }

    /// Whether `panel` exists now (open; it may be hidden).
    pub fn panel_open(&self, panel: Panel) -> bool {
        match panel {
            Panel::Layers | Panel::Properties => true,
            Panel::Develop => self.develop.is_some(),
            Panel::LayoutEditor => self.shell.layout.is_some(),
            Panel::ProgramMonitor => self.monitor.is_some(),
            Panel::SourceMonitor => self.source_monitor.is_some(),
            Panel::Timeline => self.timeline.is_some(),
            #[cfg(feature = "audio-batch")]
            Panel::Mixer => self.mixer.is_some(),
            #[cfg(feature = "audio-batch")]
            Panel::WaveformEditor => self.waveform.is_some(),
            #[cfg(feature = "audio-batch")]
            Panel::SpectralDisplay => self.spectral.is_some(),
            #[cfg(not(feature = "audio-batch"))]
            Panel::Mixer | Panel::WaveformEditor | Panel::SpectralDisplay => false,
            Panel::PrototypePlayer => self.prototype.is_some(),
            Panel::Hosted(h) => self.shell.hosted.is_open(h),
        }
    }

    /// Whether `panel` is open and shown.
    pub fn panel_visible(&self, panel: Panel) -> bool {
        self.panel_open(panel) && self.shell.visible(panel)
    }

    fn first_sequence(&self) -> Option<String> {
        self.run.snapshot().and_then(|s| s.document().sequences.first().map(|q| q.sequence_id.clone()))
    }

    /// Whether the WORKSPACE entry of `panel` can run now (feature, then precondition).
    pub fn panel_availability(&self, panel: Panel) -> Availability {
        let gate = feature_gate(panel, &CompiledFeatures::current());
        if gate != Availability::Available || self.panel_open(panel) {
            return gate;
        }
        let unavailable = |why: &str| Availability::Unavailable(why.to_owned());
        match panel {
            Panel::Develop => unavailable("open a camera raw or DNG first"),
            Panel::ProgramMonitor if self.first_sequence().is_none() => unavailable("the open document has no sequence"),
            Panel::Timeline if self.first_sequence().is_none() => unavailable("the open document has no sequence"),
            Panel::Mixer if self.first_sequence().is_none() => unavailable("the open document has no sequence"),
            #[cfg(feature = "media")]
            Panel::WaveformEditor if self.audio.is_none() => unavailable("open an audio file first"),
            Panel::WaveformEditor if self.run.snapshot().is_none() => unavailable("a document must be open to hold the edit list"),
            Panel::SpectralDisplay if !self.panel_open(Panel::WaveformEditor) => unavailable("open the waveform editor first"),
            Panel::Hosted(h) => self.hosted_availability(h),
            _ => Availability::Available,
        }
    }

    /// The availability of a panel entry as a controls descriptor record ([STU-SHL-050] states,
    /// [STU-SHL-051] codes). A missing cargo feature: `NOT_IN_THIS_DOCUMENT CAPABILITY_NOT_GRANTED`
    /// (no in-app action builds it in); no sequence: `NOT_IN_THIS_DOCUMENT NO_SUCH_CONTAINER`
    /// (`container:timeline`; the harness has no New Timeline command); otherwise
    /// `INAPPLICABLE_HERE` with the remedy that makes it true (Open..., or the waveform editor for
    /// the spectral display).
    fn panel_descriptor_availability(panel: Panel, a: &Availability) -> cm::Availability {
        match a {
            Availability::Available => cm::Availability::available(),
            Availability::NeedsFeature(f) => cm::Availability::not_in_document(
                ReasonCode::CapabilityNotGranted,
                &format!("needs feature {f} (build with --features {f})"),
                Some(&format!("capability:{f}")),
            ),
            Availability::Unavailable(why) => match panel {
                Panel::ProgramMonitor | Panel::Mixer => cm::Availability::not_in_document(ReasonCode::NoSuchContainer, why, Some("container:timeline")),
                Panel::SpectralDisplay => cm::Availability::inapplicable(ReasonCode::DocFeatureAbsent, why, &ShellCommand::Panel(Panel::WaveformEditor).id()),
                _ => cm::Availability::inapplicable(ReasonCode::DocFeatureAbsent, why, &ShellCommand::Open.id()),
            },
        }
    }

    /// The menu tree as controls descriptors, ONE availability evaluation per node ([STU-SHL-046]):
    /// the egui menu, the AccessKit descriptions, [`Viewer::shell_command`],
    /// `state_json()["shell"]["menu"]` and the manual all read it. Titles FILE, EDIT, VIEW,
    /// WORKSPACE, HELP in the order of [STU-SHL-011]; WORKSPACE > Panels ([STU-SHL-036]); HELP per
    /// [STU-SHL-037].
    pub fn menu_bar(&self) -> MenuBar {
        use cm::Availability as A;
        let entries = menu_entries();
        let leaf = |c: ShellCommand| {
            let e = entries.iter().find(|e| e.command == c);
            let mut n = MenuNode::item(&c.id(), e.map_or("", |e| e.label));
            if let Some(s) = e.and_then(|e| e.shortcut) {
                n = n.with_shortcut(s);
            }
            n
        };
        let open = self.run.snapshot().is_some();
        let history = self.run.history();
        let can = |k: &str| history[k].as_array().is_some_and(|a| !a.is_empty());
        let (can_undo, can_redo) = (can("undo"), can("redo"));
        let open_id = ShellCommand::Open.id();
        let needs_doc = |what: &str| if open { A::available() } else { A::inapplicable(ReasonCode::NoSuchContainer, &format!("nothing open to {what}"), &open_id) };

        let mut recent: Vec<MenuNode> = self
            .shell
            .recent
            .iter()
            .enumerate()
            .map(|(i, p)| MenuNode::item(&ShellCommand::OpenRecent(i).id(), &p.to_string_lossy()).data_label().with_help("Reopens this file."))
            .collect();
        recent.push(leaf(ShellCommand::ClearRecent).with_help("Empties the recent list.").with_availability(if self.shell.recent.is_empty() {
            A::not_in_document(ReasonCode::DocFeatureAbsent, "no recent files", None)
        } else {
            A::available()
        }));
        let file = MenuNode::title(
            "file",
            "File",
            vec![
                leaf(ShellCommand::Open).with_help("Opens a document, image, camera raw, PDF or (feature media) audio file through the file dialog."),
                MenuNode::submenu("studio.document.open_recent", "Open Recent", recent).with_help("The ten most recent opens (generated leaves)."),
                leaf(ShellCommand::Save).with_availability(needs_doc("save")).with_help("Saves the native package to the save path (Save As when there is none)."),
                leaf(ShellCommand::SaveAs).with_availability(needs_doc("save")).with_help("Saves the native package under a chosen name."),
                leaf(ShellCommand::ExportPng).with_availability(needs_doc("export")).with_help("Exports the rendered composite as PNG."),
                leaf(ShellCommand::ExportJpeg).with_availability(needs_doc("export")).with_help("Exports the rendered composite as JPEG."),
                leaf(ShellCommand::Close).with_help("Closes the document and every document-bound surface; the recent list stays."),
            ],
        );
        let undo = if can_undo { A::available() } else { A::not_in_document(ReasonCode::DocFeatureAbsent, "nothing to undo", None) };
        let redo = match (can_redo, can_undo) {
            (true, _) => A::available(),
            (false, true) => A::inapplicable(ReasonCode::DocFeatureAbsent, "nothing to redo", &ShellCommand::Undo.id()),
            (false, false) => A::not_in_document(ReasonCode::DocFeatureAbsent, "nothing to redo", None),
        };
        let edit = MenuNode::title(
            "edit",
            "Edit",
            vec![
                leaf(ShellCommand::Undo).with_availability(undo).with_help("Undoes the last committed step."),
                leaf(ShellCommand::Redo).with_availability(redo).with_help("Redoes the last undone step."),
            ],
        );
        let view = MenuNode::title(
            "view",
            "View",
            vec![
                leaf(ShellCommand::Fit).selected(self.fit).with_help("Fits the canvas into the window."),
                leaf(ShellCommand::ActualSize).with_help("Shows the canvas at 100 %."),
                leaf(ShellCommand::ZoomIn).with_help("Zooms in by 2x."),
                leaf(ShellCommand::ZoomOut).with_help("Zooms out by 2x."),
                leaf(ShellCommand::ToggleGpu).selected(self.prefer == Prefer::Gpu).with_help("Renders on the GPU when on, on the CPU when off."),
                leaf(ShellCommand::ToggleStatus).selected(self.shell.show_status).with_help("Shows or hides the status log."),
            ],
        );
        let panels: Vec<MenuNode> = Panel::ALL
            .iter()
            .map(|p| {
                let a = self.panel_availability(*p);
                MenuNode::item(&ShellCommand::Panel(*p).id(), &entry_label(*p, &a))
                    .with_shortcut(p.shortcut_text())
                    .selected(self.panel_visible(*p))
                    .with_help(&format!("Shows or hides the {} panel (opens it when closed).", p.label()))
                    .with_availability(Self::panel_descriptor_availability(*p, &a))
            })
            .collect();
        let workspace = MenuNode::title(
            "workspace",
            "Workspace",
            vec![MenuNode::submenu("studio.workspace.panel", "Panels", panels).with_help("One show/hide toggle per panel (STU-SHL-036).")],
        );
        let help_leaf = |h: HelpCommand| {
            let mut n = MenuNode::item(h.id(), h.label()).with_help(match h.page() {
                Some(p) => p.title(),
                None => h.missing(),
            });
            if h.menu_only() {
                n = n.menu_only();
            }
            match h.page() {
                Some(_) => n,
                None => n.with_availability(A::not_in_document(ReasonCode::NotImplementedYet, h.missing(), None)),
            }
        };
        let group = |g: &str| HelpCommand::ALL.iter().filter(|h| h.group() == Some(g)).map(|h| help_leaf(*h)).collect::<Vec<_>>();
        let help = MenuNode::title(
            "help",
            "Help",
            vec![
                MenuNode::submenu(HELP_MANUAL_ID, "Studio Manual", group(HELP_MANUAL_ID)).with_help("The manual generated from the same descriptors."),
                help_leaf(HelpCommand::CommandIndex),
                help_leaf(HelpCommand::ShortcutReference),
                MenuNode::submenu(HELP_DIAGNOSTICS_ID, "Diagnostics", group(HELP_DIAGNOSTICS_ID)).menu_only().with_help("Reports about this build and its surfaces."),
                help_leaf(HelpCommand::About),
            ],
        );
        let mut bar = MenuBar::new(vec![file, edit, view, workspace, help]);
        bar.resolve_remedies();
        bar
    }

    /// Runs one shell command (a menu entry or shortcut). The menu's availability record gates it
    /// (the same evaluation the menu renders, MENU-INV-4): a node that is not available is refused
    /// with its reason line. Refusals are also said in the status log.
    pub fn shell_command(&mut self, command: ShellCommand) -> Result<(), String> {
        let id = command.id();
        let gate = self.menu_bar().find(&id).map(|n| n.availability.clone());
        let result = match gate {
            None => Err(format!("no menu leaf {id}")),
            Some(a) if !a.is_available() => Err(a.reason_line()),
            Some(_) => self.shell_command_inner(command),
        };
        if let Err(e) = &result {
            self.say(format!("{id}: {e}"));
        }
        result
    }

    /// The open help view.
    pub fn help_state(&self) -> Option<&HelpState> {
        self.shell.help.as_ref()
    }

    /// Closes the help view.
    pub fn help_close(&mut self) {
        self.shell.help = None;
    }

    /// Sets the manual search text (showing the manual page); returns the number of matching rows.
    /// Refused: [`parse_help_query`] codes (nothing changes).
    pub fn help_search(&mut self, text: &str) -> Result<usize, &'static str> {
        let query = parse_help_query(text)?;
        let n = self.help_rows(HelpPage::Manual, &query).0.len();
        self.shell.help = Some(HelpState { page: HelpPage::Manual, query });
        Ok(n)
    }

    /// The rows of help page `page` (the query filters the manual only: case-insensitive match in
    /// title and body, blanks around it ignored) and whether they were truncated at
    /// [`MAX_HELP_ROWS`].
    pub fn help_rows(&self, page: HelpPage, query: &str) -> (Vec<HelpRow>, bool) {
        let row = |id: String, title: String, body: String| HelpRow { id, title, body };
        let bar = self.menu_bar();
        let features = compiled_feature_names().join(", ");
        let version = env!("CARGO_PKG_VERSION");
        let mut rows: Vec<HelpRow> = match page {
            HelpPage::Manual => {
                let mut v: Vec<HelpRow> = bar.manual_topics().into_iter().map(|t| row(t.id, t.title, t.body)).collect();
                for w in [Purpose::Open.window_spec(), help_window_spec(HelpPage::Manual)] {
                    let t = w.manual_topic();
                    v.push(row(t.id, t.title, t.body));
                }
                let m = hsk_studio_harness_core::manual::manual_json(self.harness);
                for t in m["panel_controls"].as_array().into_iter().flatten() {
                    let s = |k: &str| t[k].as_str().unwrap_or_default().to_owned();
                    v.push(row(format!("panel.{}", s("id")), s("title"), s("body")));
                }
                for c in m["commands"].as_array().into_iter().flatten() {
                    let s = |k: &str| c[k].as_str().unwrap_or_default().to_owned();
                    v.push(row(format!("cli.{}", s("name")), format!("studio-harness {}", s("name")), format!("{}\n{}", s("usage"), s("summary"))));
                }
                v
            }
            HelpPage::CommandIndex => bar
                .walk()
                .iter()
                .map(|w| {
                    let n = w.node;
                    let mut body = format!("command id `{}`", n.id);
                    if let Some(s) = &n.shortcut {
                        body.push_str(&format!("; shortcut {s}"));
                    }
                    if n.menu_only {
                        body.push_str("; menu-only");
                    }
                    let state = if n.availability.is_available() { "AVAILABLE".to_owned() } else { n.availability.reason_line() };
                    body.push_str(&format!("; {state}"));
                    row(n.author_id(), w.path.join(" > "), body)
                })
                .collect(),
            HelpPage::Shortcuts => bar
                .walk()
                .iter()
                .filter_map(|w| w.node.shortcut.as_ref().map(|s| row(w.node.author_id(), s.clone(), format!("{} (`{}`)", w.path.join(" > "), w.node.id))))
                .collect(),
            HelpPage::SystemReport => {
                let descriptors = self.harness.collect_descriptors().unwrap_or_default();
                let names: Vec<String> = descriptors.iter().map(|d| d["crate"].as_str().or_else(|| d["owner"].as_str()).unwrap_or("?").to_owned()).collect();
                vec![
                    row("system.package".into(), "Package".into(), format!("hsk-studio-harness-view {version}")),
                    row("system.features".into(), "Compiled features".into(), features.clone()),
                    row("system.render".into(), "Render preference".into(), format!("{:?}", self.prefer)),
                    row("system.descriptors".into(), "Crate descriptors".into(), format!("{} descriptors: {}", descriptors.len(), names.join(", "))),
                ]
            }
            HelpPage::About => vec![
                row(
                    "about.name".into(),
                    "Studio".into(),
                    "Handshake Studio development viewer: the desktop harness over the Studio crates (window-free, headless-testable).".into(),
                ),
                row("about.version".into(), "Version".into(), version.into()),
                row("about.features".into(), "Build features".into(), features.clone()),
                row("about.manual".into(), "Manual".into(), "HELP > Studio Manual; on the command line: studio-harness help".into()),
            ],
        };
        let q = query.trim().to_lowercase();
        if page == HelpPage::Manual && !q.is_empty() {
            rows.retain(|r| r.title.to_lowercase().contains(&q) || r.body.to_lowercase().contains(&q));
        }
        let truncated = rows.len() > MAX_HELP_ROWS;
        rows.truncate(MAX_HELP_ROWS);
        (rows, truncated)
    }

    fn help_json(&self) -> Value {
        let Some(h) = self.shell.help.as_ref() else { return Value::Null };
        let (rows, truncated) = self.help_rows(h.page, &h.query);
        json!({
            "page": h.page.key(),
            "title": h.page.title(),
            "query": h.query,
            "rows": rows.iter().map(|r| json!({"id": r.id, "title": r.title, "body": r.body})).collect::<Vec<_>>(),
            "truncated": truncated,
            "window": help_window_spec(h.page).agent_json(),
        })
    }

    fn shell_command_inner(&mut self, command: ShellCommand) -> Result<(), String> {
        match command {
            ShellCommand::Open => {
                self.open_dialog(Purpose::Open, None);
                Ok(())
            }
            ShellCommand::OpenRecent(i) => {
                let path = self.shell.recent.get(i).cloned().ok_or_else(|| format!("no recent file {i}"))?;
                self.path_input = path.to_string_lossy().into_owned();
                self.open(&path)
            }
            ShellCommand::ClearRecent => {
                self.shell.recent.clear();
                self.shell.recent_generation = self.shell.recent_generation.wrapping_add(1);
                Ok(())
            }
            ShellCommand::Save => {
                if self.run.snapshot().is_none() {
                    return Err("nothing open to save".into());
                }
                let target = self.save_input.trim().to_owned();
                if target.is_empty() {
                    return self.shell_command_inner(ShellCommand::SaveAs);
                }
                let overwrite = self.overwrite;
                self.save(Path::new(&target), overwrite)
            }
            ShellCommand::SaveAs => {
                if self.run.snapshot().is_none() {
                    return Err("nothing open to save".into());
                }
                let name = self.suggested_name(None);
                self.open_dialog(Purpose::SaveAs, name.as_deref());
                Ok(())
            }
            ShellCommand::ExportPng | ShellCommand::ExportJpeg => {
                if self.run.snapshot().is_none() {
                    return Err("nothing open to export".into());
                }
                let (purpose, ext) = if command == ShellCommand::ExportPng { (Purpose::ExportPng, "png") } else { (Purpose::ExportJpeg, "jpg") };
                let name = self.suggested_name(Some(ext));
                self.open_dialog(purpose, name.as_deref());
                Ok(())
            }
            ShellCommand::Close => {
                self.close();
                Ok(())
            }
            ShellCommand::Undo => self.undo().map(|_| ()),
            ShellCommand::Redo => self.redo().map(|_| ()),
            ShellCommand::Fit => {
                self.fit = true;
                self.pan = egui::Vec2::ZERO;
                Ok(())
            }
            ShellCommand::ActualSize => {
                self.fit = false;
                self.zoom = 1.0;
                self.pan = egui::Vec2::ZERO;
                Ok(())
            }
            ShellCommand::ZoomIn | ShellCommand::ZoomOut => {
                let factor = if command == ShellCommand::ZoomIn { 2.0 } else { 0.5 };
                self.zoom = (self.zoom * factor).clamp(0.05, 16.0);
                self.fit = false;
                Ok(())
            }
            ShellCommand::ToggleGpu => {
                self.prefer = if self.prefer == Prefer::Gpu { Prefer::Cpu } else { Prefer::Gpu };
                self.dirty = true;
                Ok(())
            }
            ShellCommand::ToggleStatus => {
                self.shell.show_status = !self.shell.show_status;
                Ok(())
            }
            ShellCommand::Panel(p) => self.toggle_panel(p),
            ShellCommand::Help(h) => match h.page() {
                Some(page) => {
                    let query = self.shell.help.as_ref().filter(|s| s.page == page).map(|s| s.query.clone()).unwrap_or_default();
                    self.shell.help = Some(HelpState { page, query });
                    Ok(())
                }
                None => Err(h.missing().into()),
            },
        }
    }

    /// A save / export file name from the save path (extension replaced when given).
    fn suggested_name(&self, ext: Option<&str>) -> Option<String> {
        let save = self.save_input.trim();
        if save.is_empty() {
            return None;
        }
        let p = Path::new(save);
        let p = ext.map_or_else(|| p.to_path_buf(), |e| p.with_extension(e));
        p.file_name().map(|n| n.to_string_lossy().into_owned())
    }

    /// The folder dialogs start in: the save path's, else the opened file's, else the current one.
    fn dialog_dir(&self) -> PathBuf {
        let from = |p: &Path| p.parent().filter(|d| d.is_dir()).map(Path::to_path_buf);
        let save = self.save_input.trim();
        (!save.is_empty()).then(|| from(Path::new(save))).flatten().or_else(|| self.source.as_deref().and_then(from)).unwrap_or_else(|| PathBuf::from("."))
    }

    fn open_dialog(&mut self, purpose: Purpose, suggested: Option<&str>) {
        let dir = self.dialog_dir();
        self.shell.dialog = Some(FileDialog::new(purpose, &dir, suggested));
    }

    /// Accepts the open dialog's current path field as if its Open / Save button was pressed
    /// (model navigation; the same path the button takes). `Ok(false)`: the field named a folder
    /// (listed) and the dialog stays open.
    pub fn dialog_accept(&mut self) -> Result<bool, String> {
        let d = self.shell.dialog.as_mut().ok_or("no dialog is open")?;
        match d.submit().map_err(str::to_owned)? {
            None => Ok(false),
            Some(path) => self.finish_dialog(path).map(|()| true),
        }
    }

    /// Cancels the open dialog.
    pub fn dialog_cancel(&mut self) {
        self.shell.dialog = None;
    }

    /// Runs the chosen path of the open dialog; on success the dialog closes, on failure it stays
    /// open showing the refusal.
    fn finish_dialog(&mut self, path: PathBuf) -> Result<(), String> {
        let Some(d) = self.shell.dialog.as_ref() else { return Err("no dialog is open".into()) };
        let (purpose, replace) = (d.purpose, d.replace);
        let result = self.run_dialog_purpose(purpose, &path, replace);
        match &result {
            Ok(()) => self.shell.dialog = None,
            Err(e) => {
                if let Some(d) = self.shell.dialog.as_mut() {
                    d.error = Some(e.clone());
                }
            }
        }
        result
    }

    fn run_dialog_purpose(&mut self, purpose: Purpose, path: &Path, replace: bool) -> Result<(), String> {
        let overwrite = self.overwrite || replace;
        match purpose {
            Purpose::Open => {
                self.path_input = path.to_string_lossy().into_owned();
                self.open(path)
            }
            Purpose::SaveAs => {
                self.save_input = path.to_string_lossy().into_owned();
                self.save(path, overwrite)
            }
            Purpose::ExportPng => self.export(path, ExportFormat::Png, overwrite).map(|_| ()),
            Purpose::ExportJpeg => self.export(path, ExportFormat::Jpeg { quality: super::export::DEFAULT_JPEG_QUALITY }, overwrite).map(|_| ()),
            Purpose::OpenPrototype => {
                let bytes = read_bounded(path)?;
                self.open_prototype(&bytes).map_err(|e| format!("prototype {}: {} {}", path.display(), e.code, e.detail))?;
                self.shell.hidden.retain(|p| *p != Panel::PrototypePlayer);
                self.say(format!("prototype player: {}", path.display()));
                Ok(())
            }
            Purpose::OpenSourceMedia => self.open_source_media_path(path),
        }
    }

    #[cfg(feature = "media")]
    fn open_source_media_path(&mut self, path: &Path) -> Result<(), String> {
        let bytes = read_bounded(path)?;
        self.open_source_media(bytes, MONITOR_WIDTH, MONITOR_HEIGHT).map_err(|e| format!("source monitor {}: {} {}", path.display(), e.code, e.detail))?;
        self.shell.hidden.retain(|p| *p != Panel::SourceMonitor);
        self.say(format!("source monitor: {}", path.display()));
        Ok(())
    }

    #[cfg(not(feature = "media"))]
    fn open_source_media_path(&mut self, _: &Path) -> Result<(), String> {
        Err("source monitor: needs feature media".into())
    }

    /// WORKSPACE > Panels entry of `panel`: an open surface is shown or hidden; a closed one opens
    /// (through a file dialog for the source monitor and the prototype player).
    pub fn toggle_panel(&mut self, panel: Panel) -> Result<(), String> {
        let availability = self.panel_availability(panel);
        match availability {
            Availability::NeedsFeature(f) => return Err(format!("{}: needs feature {f} (build with --features {f})", panel.label())),
            Availability::Unavailable(why) => return Err(format!("{}: {why}", panel.label())),
            Availability::Available => {}
        }
        if self.panel_open(panel) {
            if self.shell.visible(panel) {
                self.shell.hidden.push(panel);
                self.say(format!("{} hidden", panel.label()));
            } else {
                self.shell.hidden.retain(|p| *p != panel);
                self.say(format!("{} shown", panel.label()));
            }
            return Ok(());
        }
        self.open_panel(panel)?;
        self.shell.hidden.retain(|p| *p != panel);
        Ok(())
    }

    fn open_panel(&mut self, panel: Panel) -> Result<(), String> {
        match panel {
            Panel::Layers | Panel::Properties => Ok(()),
            Panel::Develop => Err("open a camera raw or DNG first".into()),
            Panel::LayoutEditor => {
                let (doc, note) = self.layout_doc();
                self.shell.layout = Some(LayoutEditor::new(doc));
                self.say(format!("layout editor: {note}; layout edits stay in the editor (saving them with the package is a declared gap of the layout editor)"));
                Ok(())
            }
            Panel::ProgramMonitor => {
                let sequence = self.first_sequence().ok_or("the open document has no sequence")?;
                self.open_monitor(&sequence, MONITOR_WIDTH, MONITOR_HEIGHT).map_err(|e| format!("{} {}", e.code, e.detail))?;
                self.say(format!("program monitor: sequence {sequence} (no frame decoder is bound for document sequences; transport and scrub run)"));
                Ok(())
            }
            Panel::SourceMonitor => {
                self.open_dialog(Purpose::OpenSourceMedia, None);
                Ok(())
            }
            Panel::Timeline => self.open_timeline(),
            Panel::PrototypePlayer => {
                self.open_dialog(Purpose::OpenPrototype, None);
                Ok(())
            }
            Panel::Hosted(h) => {
                self.shell.hosted.open(h);
                Ok(())
            }
            #[cfg(feature = "audio-batch")]
            Panel::Mixer => {
                let sequence = self.first_sequence().ok_or("the open document has no sequence")?;
                let result = match self.audio.as_ref().map(|a| a.import.audio.clone()) {
                    Some(audio) => {
                        let extents: std::sync::Arc<dyn hsk_studio_pulse::SourceExtents> = std::sync::Arc::new(audio.clone());
                        self.open_mixer(&sequence, MIXER_RATE, MIXER_BLOCK, audio, extents)
                    }
                    None => self.open_mixer(&sequence, MIXER_RATE, MIXER_BLOCK, NoAudio, std::sync::Arc::new(hsk_studio_pulse::UnknownExtents)),
                };
                // The mixer stays open showing its error when the bus stream refuses (mixer_ui).
                if let Err(e) = result {
                    self.say(format!("mixer: {e}"));
                }
                Ok(())
            }
            #[cfg(feature = "audio-batch")]
            Panel::WaveformEditor => self.open_waveform_from_audio(),
            #[cfg(feature = "audio-batch")]
            Panel::SpectralDisplay => self.open_spectral_display(super::spectral_display::default_params()),
            #[cfg(not(feature = "audio-batch"))]
            Panel::Mixer | Panel::WaveformEditor | Panel::SpectralDisplay => Err(format!("{}: needs feature audio-batch", panel.label())),
        }
    }

    /// The layout editor's document: the open document when it has page spreads (its text stories
    /// and type styles from the `text` entry), else a new blank one-page document.
    fn layout_doc(&self) -> (LayoutDoc, &'static str) {
        if let Some(doc) = self.run.snapshot().map(|s| s.document().clone()).filter(|d| !d.page_spreads.is_empty()) {
            let (stories, styles) = match self.text_entry() {
                Ok(e) => (e.stories, e.styles),
                Err(_) => (Vec::new(), hsk_studio_folio::StudioTypeStyleRecords::new(&doc.document_id)),
            };
            let flow = hsk_studio_folio::StudioLayoutFlowRecords::new(doc.document_id.clone());
            return (LayoutDoc { document: doc, stories, flow, styles }, "editing the open document's pages");
        }
        (blank_layout_doc(), "the open document has no pages; editing a new blank one-page layout document")
    }

    /// Closes the open document and every document-bound surface (FILE > Close); the recent list,
    /// panel visibility and render preference stay.
    pub fn close(&mut self) {
        self.run = self.harness.run(crate::harness::default_actor(), self.cancel);
        self.source = None;
        self.assets = super::render::Assets::new();
        self.receipt = None;
        self.frame = None;
        self.render_error = None;
        self.texture = None;
        self.dirty = false;
        self.edit = super::edit::EditState { tool: self.edit.tool, combine: self.edit.combine, brush: self.edit.brush, ..super::edit::EditState::default() };
        self.selected = None;
        self.develop = None;
        self.monitor = None;
        self.source_monitor = None;
        self.timeline = None;
        self.prototype = None;
        self.selection_history.clear();
        self.records = None;
        #[cfg(feature = "media")]
        {
            self.audio = None;
        }
        #[cfg(feature = "audio-batch")]
        {
            self.mixer = None;
            self.waveform = None;
            self.spectral = None;
        }
        self.shell.close_document();
        self.path_input.clear();
        self.save_input.clear();
        self.overwrite = false;
        self.fit = true;
        self.pan = egui::Vec2::ZERO;
        self.say("closed the document".into());
    }

    pub(super) fn shell_json(&self) -> Value {
        let panels: serde_json::Map<String, Value> = Panel::ALL
            .iter()
            .map(|p| {
                let a = self.panel_availability(*p);
                let reason = match &a {
                    Availability::Unavailable(r) => Some(r.clone()),
                    _ => None,
                };
                (
                    p.key().to_owned(),
                    json!({"open": self.panel_open(*p), "visible": self.panel_visible(*p), "availability": a.key(), "feature": p.feature(), "reason": reason, "label": entry_label(*p, &a), "shortcut": p.shortcut_text(), "command": ShellCommand::Panel(*p).id()}),
                )
            })
            .collect();
        json!({
            "panels": panels,
            "status_log": self.shell.show_status,
            "recent": self.shell.recent.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            "dialog": self.shell.dialog.as_ref().map(FileDialog::state_json),
            "hosted": self.hosted_json(),
            "surfaces": self.surfaces_json(),
            "menu": self.menu_bar().agent_json(),
            "help": self.help_json(),
        })
    }

    /// The menu bar (top-most panel), drawn from [`Viewer::menu_bar`] by the controls renderer.
    /// With feature `desktop` every node's AccessKit description carries its availability
    /// (`AVAILABLE`, or the reason line: state, reason code, reason text, remedy; [STU-SHL-019] /
    /// [STU-MDL-107]). Menu clicks run after the bar is drawn.
    pub(super) fn shell_menu_ui(&mut self, ctx: &egui::Context) {
        let bar = self.menu_bar();
        let mut rendered = cm::MenuUi::default();
        egui::TopBottomPanel::top(vw::id(widget::MENU_BAR)).show(ctx, |ui| {
            rendered = cm::menu_bar_ui(ui, &bar, &vw::id);
        });
        #[cfg(feature = "desktop")]
        for n in &rendered.nodes {
            let line = if n.enabled { "AVAILABLE".to_owned() } else { n.reason.clone() };
            let _ = ctx.accesskit_node_builder(n.id, |b| b.set_description(line));
        }
        self.drawn.push(widget::MENU_BAR.into());
        self.drawn.extend(rendered.drawn());
        if let Some(command) = rendered.clicked.as_deref().and_then(ShellCommand::from_id) {
            let _ = self.shell_command(command);
        }
    }

    /// Shell shortcuts: Ctrl+O, Ctrl+S, Ctrl+Shift+S, Ctrl+W and the panel function keys
    /// (Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z stay with the editing shortcuts).
    pub(super) fn shell_shortcuts(&mut self, ctx: &egui::Context) {
        let mut commands = Vec::new();
        ctx.input_mut(|i| {
            // Most specific first: consume_key ignores extra Shift.
            if i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::S) {
                commands.push(ShellCommand::SaveAs);
            } else if i.consume_key(egui::Modifiers::COMMAND, egui::Key::S) {
                commands.push(ShellCommand::Save);
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::O) {
                commands.push(ShellCommand::Open);
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::W) {
                commands.push(ShellCommand::Close);
            }
            // Modified keys first: consume_key ignores an extra Shift, so a plain F-key pattern would
            // also take Shift+F-key (hosted panels, module hosted).
            let (modified, plain): (Vec<Panel>, Vec<Panel>) = Panel::ALL.into_iter().partition(|p| p.modifiers() != egui::Modifiers::NONE);
            for p in modified.into_iter().chain(plain) {
                if i.consume_key(p.modifiers(), p.key_code()) {
                    commands.push(ShellCommand::Panel(p));
                }
            }
        });
        for c in commands {
            let _ = self.shell_command(c);
        }
    }

    /// The help view (an in-window egui window, never an OS window), from its descriptor
    /// ([`help_window_spec`]).
    fn help_ui(&mut self, ctx: &egui::Context) {
        let Some(h) = self.shell.help.clone() else { return };
        let spec = help_window_spec(h.page);
        let (rows, truncated) = self.help_rows(h.page, &h.query);
        let (mut open, mut close, mut query) = (true, false, h.query.clone());
        egui::Window::new(spec.title.as_str())
            .id(vw::id(widget::HELP_WINDOW))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([520.0, 480.0])
            .show(ctx, |ui| {
                if spec.item(widget::HELP_SEARCH).is_some() {
                    ui.add(egui::TextEdit::singleline(&mut query).id(vw::id(widget::HELP_SEARCH)).hint_text(spec.label(widget::HELP_SEARCH)).desired_width(f32::INFINITY));
                }
                egui::ScrollArea::vertical().id_salt(vw::id(widget::HELP_ROWS)).max_height(380.0).show(ui, |ui| {
                    for r in &rows {
                        ui.strong(r.title.as_str());
                        ui.label(r.body.as_str());
                        ui.separator();
                    }
                    if truncated {
                        ui.label(format!("only the first {MAX_HELP_ROWS} rows are shown"));
                    }
                });
                if ui.push_id(vw::id(widget::HELP_CLOSE), |ui| ui.button(spec.label(widget::HELP_CLOSE))).inner.clicked() {
                    close = true;
                }
            });
        self.drawn.push(widget::HELP_WINDOW.into());
        self.drawn.extend(spec.items.iter().map(|i| i.id.clone()));
        if !open || close {
            self.shell.help = None;
        } else if query != h.query {
            match parse_help_query(&query) {
                Ok(q) => self.shell.help = Some(HelpState { page: h.page, query: q }),
                Err(code) => self.say(format!("help search: {code}")),
            }
        }
    }

    /// The open dialog, the help view and the hosted layout editor (drawn as floating windows).
    pub(super) fn shell_windows_ui(&mut self, ctx: &egui::Context) {
        self.hosted_ui(ctx);
        let show_layout = self.shell.visible(Panel::LayoutEditor);
        if let Some(ed) = self.shell.layout.as_mut()
            && show_layout
        {
            let drawn = ed.ui(ctx);
            self.drawn.extend(drawn.into_iter().map(String::from));
        }
        self.help_ui(ctx);
        let Some(d) = self.shell.dialog.as_mut() else { return };
        let (outcome, drawn) = d.ui(ctx);
        self.drawn.extend(drawn);
        match outcome {
            Some(Outcome::Cancel) => self.shell.dialog = None,
            Some(Outcome::Accept(path)) => {
                let _ = self.finish_dialog(path);
            }
            None => {}
        }
    }
}

/// Reads a file the menu opens directly (prototype, source media), bounded like every import.
fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path).map_err(|e| format!("open {}: {e}", path.display()))?.len();
    if len > crate::import::MAX_SOURCE_BYTES {
        return Err(format!("open {}: {len} bytes exceeds {}", path.display(), crate::import::MAX_SOURCE_BYTES));
    }
    std::fs::read(path).map_err(|e| format!("open {}: {e}", path.display()))
}

/// A new blank layout document: one spread with one 595 x 842 pt page (A4), 36 pt margins.
pub fn blank_layout_doc() -> LayoutDoc {
    use hsk_studio_folio::*;
    let len = |v: f64| Length { value: v, unit: GeometryUnit::Pt };
    let edges = |v: f64| LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) };
    let unassigned = || ProfileBinding::Unassigned(());
    let id = "SDOC-019abcde-0000-7000-8000-00000000b001".to_owned();
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id.clone(),
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: "SPGS-019abcde-0000-7000-8000-00000000b001".into(),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(595.0), height: len(842.0) },
            pages: vec![StudioPage { page_id: "page-1".into(), width: len(595.0), height: len(842.0), side: PageSide::RightHand, margins: edges(36.0), bleed: None }],
        }],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    };
    LayoutDoc { document, stories: vec![], flow: StudioLayoutFlowRecords::new(id.clone()), styles: StudioTypeStyleRecords::new(id) }
}
