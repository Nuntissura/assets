//! Desktop shell of the viewer (lane SMB-HARNESS-SHELL-1): a menu bar (FILE, EDIT, VIEW, WORKSPACE),
//! keyboard shortcuts, the in-crate file dialog ([`super::file_dialog`]) for open / save / export,
//! a recent-files list, and the WORKSPACE panel toggles that host every coded surface in the one
//! window: layers, properties, develop, the layout editor, program monitor, source monitor, mixer,
//! waveform editor, spectral display and prototype player.
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
#![deny(clippy::arithmetic_side_effects)]

use super::file_dialog::{FileDialog, Outcome, Purpose};
use super::widget as vw;
use super::{Viewer, export::ExportFormat, render::Prefer};
use crate::layout_ui::{LayoutDoc, LayoutEditor};
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
    pub const OPEN_RECENT: &str = "menu.studio.document.open_recent";
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
    Mixer,
    WaveformEditor,
    SpectralDisplay,
    PrototypePlayer,
}

impl Panel {
    pub const ALL: [Panel; 10] = [
        Panel::Layers,
        Panel::Properties,
        Panel::Develop,
        Panel::LayoutEditor,
        Panel::ProgramMonitor,
        Panel::SourceMonitor,
        Panel::Mixer,
        Panel::WaveformEditor,
        Panel::SpectralDisplay,
        Panel::PrototypePlayer,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Layers => "layers",
            Self::Properties => "properties",
            Self::Develop => "develop",
            Self::LayoutEditor => "layout_editor",
            Self::ProgramMonitor => "program_monitor",
            Self::SourceMonitor => "source_monitor",
            Self::Mixer => "mixer",
            Self::WaveformEditor => "waveform_editor",
            Self::SpectralDisplay => "spectral_display",
            Self::PrototypePlayer => "prototype_player",
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
            Self::Mixer => "Mixer",
            Self::WaveformEditor => "Waveform Editor",
            Self::SpectralDisplay => "Spectral Display",
            Self::PrototypePlayer => "Prototype Player",
        }
    }
    /// The cargo feature the surface needs (beyond `viewer`), if any.
    pub fn feature(self) -> Option<&'static str> {
        match self {
            Self::SourceMonitor => Some("media"),
            Self::Mixer | Self::WaveformEditor | Self::SpectralDisplay => Some("audio-batch"),
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
            Self::Mixer => egui::Key::F8,
            Self::WaveformEditor => egui::Key::F9,
            Self::SpectralDisplay => egui::Key::F11,
            Self::PrototypePlayer => egui::Key::F12,
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
            Self::Mixer => "F8",
            Self::WaveformEditor => "F9",
            Self::SpectralDisplay => "F11",
            Self::PrototypePlayer => "F12",
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
        }
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
}

impl Default for ShellState {
    fn default() -> Self {
        Self { hidden: Vec::new(), show_status: true, layout: None, dialog: None, recent: Vec::new(), recent_generation: 0 }
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
            #[cfg(feature = "audio-batch")]
            Panel::Mixer => self.mixer.is_some(),
            #[cfg(feature = "audio-batch")]
            Panel::WaveformEditor => self.waveform.is_some(),
            #[cfg(feature = "audio-batch")]
            Panel::SpectralDisplay => self.spectral.is_some(),
            #[cfg(not(feature = "audio-batch"))]
            Panel::Mixer | Panel::WaveformEditor | Panel::SpectralDisplay => false,
            Panel::PrototypePlayer => self.prototype.is_some(),
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
            Panel::Mixer if self.first_sequence().is_none() => unavailable("the open document has no sequence"),
            #[cfg(feature = "media")]
            Panel::WaveformEditor if self.audio.is_none() => unavailable("open an audio file first"),
            Panel::WaveformEditor if self.run.snapshot().is_none() => unavailable("a document must be open to hold the edit list"),
            Panel::SpectralDisplay if !self.panel_open(Panel::WaveformEditor) => unavailable("open the waveform editor first"),
            _ => Availability::Available,
        }
    }

    /// Runs one shell command (a menu entry or shortcut). Refusals are also said in the status log.
    pub fn shell_command(&mut self, command: ShellCommand) -> Result<(), String> {
        let result = self.shell_command_inner(command);
        if let Err(e) = &result {
            self.say(format!("{}: {e}", command.id()));
        }
        result
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
            Panel::PrototypePlayer => {
                self.open_dialog(Purpose::OpenPrototype, None);
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
        })
    }

    /// The menu bar (top-most panel). Menu clicks run after the bar is drawn.
    pub(super) fn shell_menu_ui(&mut self, ctx: &egui::Context) {
        let mut chosen: Option<ShellCommand> = None;
        let mut drawn: Vec<String> = Vec::new();
        let open = self.run.snapshot().is_some();
        let history = self.run.history();
        let can = |k: &str| history[k].as_array().is_some_and(|a| !a.is_empty());
        let (can_undo, can_redo) = (can("undo"), can("redo"));
        let recent: Vec<String> = self.shell.recent.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        let panels: Vec<(Panel, Availability, bool)> = Panel::ALL.iter().map(|p| (*p, self.panel_availability(*p), self.panel_visible(*p))).collect();
        let (gpu, status, fit) = (self.prefer == Prefer::Gpu, self.shell.show_status, self.fit);
        egui::TopBottomPanel::top(vw::id(widget::MENU_BAR)).show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let mut item = |ui: &mut egui::Ui, command: ShellCommand, label: &str, shortcut: Option<&str>, enabled: bool, hover: Option<&str>, selected: bool| {
                    let name = widget::entry(&command.id());
                    let mut button = egui::Button::new(label).selected(selected);
                    if let Some(s) = shortcut {
                        button = button.shortcut_text(s);
                    }
                    let mut r = ui.push_id(vw::id(&name), |ui| ui.add_enabled(enabled, button)).inner;
                    if let Some(h) = hover {
                        r = r.on_disabled_hover_text(h);
                    }
                    drawn.push(name);
                    if r.clicked() {
                        chosen = Some(command);
                        ui.close();
                    }
                };
                ui.push_id(vw::id(widget::FILE), |ui| {
                    ui.menu_button("File", |ui| {
                        item(ui, ShellCommand::Open, "Open…", Some("Ctrl+O"), true, None, false);
                        ui.push_id(vw::id(widget::OPEN_RECENT), |ui| {
                            ui.menu_button("Open Recent", |ui| {
                                if recent.is_empty() {
                                    ui.label("No recent files");
                                }
                                for (i, path) in recent.iter().enumerate() {
                                    item(ui, ShellCommand::OpenRecent(i), path, None, true, None, false);
                                }
                                ui.separator();
                                item(ui, ShellCommand::ClearRecent, "Clear Recent", None, !recent.is_empty(), None, false);
                            });
                        });
                        ui.separator();
                        item(ui, ShellCommand::Save, "Save", Some("Ctrl+S"), open, Some("nothing open"), false);
                        item(ui, ShellCommand::SaveAs, "Save As…", Some("Ctrl+Shift+S"), open, Some("nothing open"), false);
                        ui.separator();
                        item(ui, ShellCommand::ExportPng, "Export PNG…", None, open, Some("nothing open"), false);
                        item(ui, ShellCommand::ExportJpeg, "Export JPEG…", None, open, Some("nothing open"), false);
                        ui.separator();
                        item(ui, ShellCommand::Close, "Close", Some("Ctrl+W"), true, None, false);
                    });
                });
                ui.push_id(vw::id(widget::EDIT), |ui| {
                    ui.menu_button("Edit", |ui| {
                        item(ui, ShellCommand::Undo, "Undo", Some("Ctrl+Z"), can_undo, Some("nothing to undo"), false);
                        item(ui, ShellCommand::Redo, "Redo", Some("Ctrl+Y"), can_redo, Some("nothing to redo"), false);
                    });
                });
                ui.push_id(vw::id(widget::VIEW), |ui| {
                    ui.menu_button("View", |ui| {
                        item(ui, ShellCommand::Fit, "Fit in Window", None, true, None, fit);
                        item(ui, ShellCommand::ActualSize, "Actual Size", None, true, None, false);
                        item(ui, ShellCommand::ZoomIn, "Zoom In", None, true, None, false);
                        item(ui, ShellCommand::ZoomOut, "Zoom Out", None, true, None, false);
                        ui.separator();
                        item(ui, ShellCommand::ToggleGpu, "Render on GPU", None, true, None, gpu);
                        item(ui, ShellCommand::ToggleStatus, "Status Log", None, true, None, status);
                    });
                });
                ui.push_id(vw::id(widget::WORKSPACE), |ui| {
                    ui.menu_button("Workspace", |ui| {
                        ui.label("Panels");
                        for (p, availability, visible) in &panels {
                            let hover = match availability {
                                Availability::NeedsFeature(f) => Some(format!("build with --features {f}")),
                                Availability::Unavailable(why) => Some(why.clone()),
                                Availability::Available => None,
                            };
                            item(ui, ShellCommand::Panel(*p), &entry_label(*p, availability), Some(p.shortcut_text()), *availability == Availability::Available, hover.as_deref(), *visible);
                        }
                    });
                });
            });
        });
        self.drawn.extend([widget::MENU_BAR, widget::FILE, widget::EDIT, widget::VIEW, widget::WORKSPACE].map(String::from));
        self.drawn.extend(drawn);
        if let Some(command) = chosen {
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
            for p in Panel::ALL {
                if i.consume_key(egui::Modifiers::NONE, p.key_code()) {
                    commands.push(ShellCommand::Panel(p));
                }
            }
        });
        for c in commands {
            let _ = self.shell_command(c);
        }
    }

    /// The open dialog and the hosted layout editor (drawn as floating windows).
    pub(super) fn shell_windows_ui(&mut self, ctx: &egui::Context) {
        let show_layout = self.shell.visible(Panel::LayoutEditor);
        if let Some(ed) = self.shell.layout.as_mut()
            && show_layout
        {
            let drawn = ed.ui(ctx);
            self.drawn.extend(drawn.into_iter().map(String::from));
        }
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
