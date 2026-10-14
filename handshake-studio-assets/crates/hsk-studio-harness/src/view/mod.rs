//! Desktop viewer state and UI (feature `viewer`; the window host is feature `desktop`, binary
//! `studio-harness`). Everything here runs without a window: [`Viewer`] is the whole application
//! state and [`Viewer::ui`] draws one egui frame into any `egui::Context`, so tests drive it
//! headlessly (`tests/viewer_headless.rs`).
//!
//! Flow: open a file (PSD/PSB, TIFF, camera raw, PDF through `import::convert_to_native` and
//! `Run::open_import_detailed`; a native `.handshake` package through `Run::open_bytes`) -> render
//! the composite ([`render::render`]: render-gpu, render-cpu fallback) -> layer tree with
//! visibility toggles and the controls-descriptor panel of the selected layer (name, visible,
//! opacity and blend mode commit through the session's chronicle provider) -> editing tools
//! (module [`edit`]: selections, brush, undo/redo) -> import receipt and loss panel -> save as a
//! native package (`Run::save_to`, never over the opened source).
//!
//! Model navigation: every interactive widget has a stable id from [`widget`] (`hsk-studio-ui`
//! plus a name, layer rows add the layer id); [`Viewer::ui`] records the ids it drew in
//! [`Viewer::drawn`], and [`Viewer::state_json`] is a structured projection of the whole state.
#![deny(clippy::arithmetic_side_effects)]

#[cfg(feature = "desktop")]
pub mod display;
pub mod edit;
pub mod export;
pub mod brush_mix;
pub mod layers;
pub mod render;

use crate::harness::{Harness, References, Run, Selection};
use crate::panel::{self, PanelValues, Route};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Parent, StudioLayer};
use hsk_studio_package::ImportRole;
use export::ExportFormat;
use edit::{EditState, Tool};
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::selection::CombineMode;
use render::{Assets, Frame, GpuDisplay, Prefer, RenderFail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Stable widget ids (egui `Id` sources) of the viewer.
pub mod widget {
    pub const PATH_FIELD: &str = "open.path";
    pub const OPEN_BUTTON: &str = "open.button";
    pub const SAVE_FIELD: &str = "save.path";
    pub const SAVE_OVERWRITE: &str = "save.overwrite";
    pub const SAVE_BUTTON: &str = "save.button";
    /// Export the composite as PNG / JPEG next to the save path (extension replaced).
    pub const EXPORT_PNG_BUTTON: &str = "export.png";
    pub const EXPORT_JPEG_BUTTON: &str = "export.jpeg";
    pub const RENDER_BACKEND: &str = "render.prefer_gpu";
    pub const FIT_BUTTON: &str = "view.fit";
    pub const ACTUAL_SIZE_BUTTON: &str = "view.actual_size";
    pub const ZOOM_SLIDER: &str = "view.zoom";
    pub const CANVAS: &str = "view.canvas";
    pub const LAYER_PANEL: &str = "layers";
    pub const PROPERTIES_PANEL: &str = "properties";
    pub const RECEIPT_PANEL: &str = "receipt";
    pub const STATUS_PANEL: &str = "status";
    /// Editing (lane SMB-HARNESS-UI-2): tool buttons `tool.<key>` (keys of `edit::Tool`).
    pub const UNDO_BUTTON: &str = "edit.undo";
    pub const REDO_BUTTON: &str = "edit.redo";
    pub const COMBINE: &str = "select.combine";
    pub const CLEAR_SELECTION: &str = "select.clear";
    pub const BRUSH_SIZE: &str = "brush.size";
    pub const BRUSH_HARDNESS: &str = "brush.hardness";
    pub const BRUSH_OPACITY: &str = "brush.opacity";
    pub const BRUSH_COLOUR: &str = "brush.colour";
    /// Layer management (lane SMB-HARNESS-UI-3), acting on the selected layer.
    pub const LAYER_NEW: &str = "layers.new";
    pub const LAYER_DUPLICATE: &str = "layers.duplicate";
    pub const LAYER_DELETE: &str = "layers.delete";
    pub const LAYER_UP: &str = "layers.up";
    pub const LAYER_DOWN: &str = "layers.down";
    pub const LAYER_GROUP: &str = "layers.group";
    pub const LAYER_UNGROUP: &str = "layers.ungroup";
    pub fn tool(key: &str) -> String {
        format!("tool.{key}")
    }
    /// Layer rows: `layer.<layer_id>.visible` (checkbox) and `layer.<layer_id>.select` (label).
    pub fn layer(layer_id: &str, part: &str) -> String {
        format!("layer.{layer_id}.{part}")
    }
    /// The egui `Id` of a viewer widget name.
    pub fn id(name: &str) -> egui::Id {
        egui::Id::new(("hsk-studio-ui", name))
    }
}

/// One row of the layer tree (top-first, depth = nesting level).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerRow {
    pub layer_id: String,
    pub name: String,
    pub kind: String,
    pub depth: usize,
    pub visible: bool,
    pub opacity: f64,
}

/// The application state of the desktop viewer.
pub struct Viewer {
    run: Run<'static>,
    pub path_input: String,
    pub save_input: String,
    pub overwrite: bool,
    pub prefer: Prefer,
    source: Option<PathBuf>,
    assets: Assets,
    receipt: Option<Value>,
    frame: Option<Frame>,
    render_error: Option<RenderFail>,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
    gpu: Option<hsk_studio_render_gpu::GpuBackend>,
    gpu_options: hsk_studio_render_gpu::DeviceOptions,
    /// Host display path (desktop: eframe's wgpu device); when set, GPU frames render straight into
    /// the display texture and no own render-gpu device is created.
    display: Option<Box<dyn GpuDisplay>>,
    /// Tools, selection, brush (module `edit`).
    edit: EditState,
    selected: Option<String>,
    status: Vec<String>,
    zoom: f32,
    pan: egui::Vec2,
    fit: bool,
    drawn: Vec<String>,
    cancel: &'static CancellationToken,
}

const MAX_STATUS: usize = 200;

impl Viewer {
    /// A viewer with the chronicle provider selected (edits commit through the session) and
    /// development references (`trust_all`, recorded in every harness report).
    pub fn new(prefer: Prefer) -> Self {
        Self::with_gpu_options(prefer, hsk_studio_render_gpu::DeviceOptions::default())
    }

    /// As [`Viewer::new`] with explicit render-gpu device options (tests pass `BackendChoice::None`
    /// to prove the render-cpu fallback without touching a GPU).
    pub fn with_gpu_options(prefer: Prefer, gpu_options: hsk_studio_render_gpu::DeviceOptions) -> Self {
        let selection = Selection { providers: vec![hsk_studio_session::CHRONICLE_PROVIDER_ID.to_string()], references: References::TrustAll };
        // The application owns one harness and one token for its whole life; a `Run` borrows both.
        let harness: &'static Harness = Box::leak(Box::new(Harness::new(selection).expect("the chronicle provider is available")));
        let cancel: &'static CancellationToken = Box::leak(Box::new(CancellationToken::default()));
        Self {
            run: harness.run(crate::harness::default_actor(), cancel),
            path_input: String::new(),
            save_input: String::new(),
            overwrite: false,
            prefer,
            source: None,
            assets: Assets::new(),
            receipt: None,
            frame: None,
            render_error: None,
            texture: None,
            dirty: false,
            gpu: None,
            gpu_options,
            display: None,
            edit: EditState::default(),
            selected: None,
            status: Vec::new(),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            fit: true,
            drawn: Vec::new(),
            cancel,
        }
    }

    /// Uses `display` for GPU frames (lane SMB-RENDER-GPU-SHARED-DEVICE): no read-back, no upload,
    /// no second device. The read-back path stays the fallback when the display refuses a frame.
    pub fn set_display(&mut self, display: Box<dyn GpuDisplay>) {
        self.display = Some(display);
        self.gpu = None;
        self.dirty = true;
    }

    /// Appends a line to the status log (hosts report their own setup there).
    pub fn status_line(&mut self, line: String) {
        self.say(line);
    }

    fn say(&mut self, line: String) {
        self.status.push(line);
        if self.status.len() > MAX_STATUS {
            let excess = self.status.len().saturating_sub(MAX_STATUS);
            self.status.drain(..excess);
        }
    }

    fn last_op(&self) -> Value {
        self.run.ops().last().cloned().unwrap_or(Value::Null)
    }

    fn op_error(&self) -> String {
        let op = self.last_op();
        op["error"].as_str().unwrap_or("failed").to_string()
    }

    /// Opens `path`: a recognised source format is imported and converted to native; anything else
    /// is opened as a native package. Bounded by `import::MAX_SOURCE_BYTES`.
    pub fn open(&mut self, path: &Path) -> Result<(), String> {
        let result = self.open_inner(path);
        if let Err(e) = &result {
            self.say(e.clone());
        }
        result
    }

    fn open_inner(&mut self, path: &Path) -> Result<(), String> {
        let len = std::fs::metadata(path).map_err(|e| format!("open {}: {e}", path.display()))?.len();
        if len > crate::import::MAX_SOURCE_BYTES {
            return Err(format!("open {}: {len} bytes exceeds {}", path.display(), crate::import::MAX_SOURCE_BYTES));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let (package, receipt) = if crate::import::sniff(&bytes).is_some() {
            match self.run.open_import_detailed(&bytes) {
                Some((package, receipt)) => (package, Some(receipt)),
                None => return Err(format!("import {}: {}", path.display(), self.last_op())),
            }
        } else {
            if !self.run.open_bytes(&bytes) {
                return Err(format!("open {}: {}", path.display(), self.op_error()));
            }
            (bytes, None)
        };
        let opened = hsk_studio_package::read_package(
            &package,
            &hsk_studio_package::Limits::default(),
            hsk_studio_folio::Budget::default(),
            &crate::harness::TrustAll,
            self.cancel,
        )
        .map_err(|e| format!("tiles {}: {}", path.display(), e.code()))?;
        let receipt = receipt.or_else(|| opened.import_bytes(ImportRole::Receipt).and_then(|b| serde_json::from_slice(b).ok()));
        // Tile bodies come from the session (render-ready view of the opened revision).
        self.assets = self.run.render_assets().map_err(|e| format!("tiles {}: {e}", path.display()))?;
        self.edit = EditState { tool: self.edit.tool, combine: self.edit.combine, brush: self.edit.brush, ..EditState::default() };
        drop(opened);
        self.receipt = receipt;
        self.source = Some(path.to_path_buf());
        self.selected = None;
        self.frame = None;
        self.render_error = None;
        self.dirty = true;
        self.fit = true;
        self.pan = egui::Vec2::ZERO;
        if self.save_input.is_empty() {
            self.save_input = path.with_extension("handshake").to_string_lossy().into_owned();
            if self.source.as_deref() == Some(Path::new(&self.save_input)) {
                self.save_input = path.with_extension("saved.handshake").to_string_lossy().into_owned();
            }
        }
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        self.say(format!("opened {} (revision {revision}, {} tile assets)", path.display(), self.assets.len()));
        Ok(())
    }

    /// The layer tree in stack order, top-first, depth-first (the stack is Folio's graph: group
    /// inputs by ordinal, the root by graph outputs); layers outside any stack (no graph operation)
    /// follow their stacked siblings in document order.
    pub fn layers(&self) -> Vec<LayerRow> {
        let Some(snap) = self.run.snapshot() else {
            return Vec::new();
        };
        let doc = snap.document();
        let children = |parent: Option<&str>| -> Vec<&StudioLayer> {
            let stacked = layers::stack(doc, parent);
            let mut out: Vec<&StudioLayer> = stacked.iter().rev().filter_map(|id| doc.layers.iter().find(|l| &l.layer_id == id)).collect();
            out.extend(doc.layers.iter().filter(|l| {
                let under = match (&l.parent, parent) {
                    (Parent::Root(()), None) => true,
                    (Parent::Node(n), Some(p)) => n.node_id == p,
                    _ => false,
                };
                under && !stacked.contains(&l.layer_id)
            }));
            out
        };
        let mut rows = Vec::new();
        let mut stack: Vec<(&StudioLayer, usize)> = children(None).into_iter().rev().map(|l| (l, 0)).collect();
        while let Some((layer, depth)) = stack.pop() {
            rows.push(LayerRow {
                layer_id: layer.layer_id.clone(),
                name: layer.name.clone(),
                kind: layer.kind.clone(),
                depth,
                visible: layer.visible,
                opacity: layer.effective_appearance().opacity,
            });
            let next = depth.saturating_add(1);
            stack.extend(children(Some(&layer.layer_id)).into_iter().rev().map(|c| (c, next)));
        }
        rows
    }

    /// Shows or hides a layer through the layer panel's `layer.visible` control (descriptor-parsed,
    /// committed by the chronicle provider). Returns the new document revision.
    pub fn set_visible(&mut self, layer_id: &str, visible: bool) -> Result<u64, String> {
        self.commit(layer_id, panel::VISIBLE, if visible { "true" } else { "false" })
    }

    fn commit(&mut self, layer_id: &str, control: &str, text: &str) -> Result<u64, String> {
        if self.run.panel_set(layer_id, control, text, None) {
            let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
            self.dirty = true;
            self.say(format!("{control} = {text} on {layer_id} (revision {revision})"));
            Ok(revision)
        } else {
            let e = format!("{control} on {layer_id} refused: {}", self.op_error());
            self.say(e.clone());
            Err(e)
        }
    }

    fn is_source(&self, path: &Path) -> bool {
        self.source.as_deref().is_some_and(|source| match (std::fs::canonicalize(source), std::fs::canonicalize(path)) {
            (Ok(a), Ok(b)) => a == b,
            _ => source == path,
        })
    }

    /// Exports the current composite (rendered first when the document changed) as PNG or JPEG through
    /// [`export::encode_frame`] and writes it atomically. Refuses the opened source path unless
    /// `overwrite` is set. Returns the export report (format, size, notes, backend, revision, path).
    pub fn export(&mut self, path: &Path, format: ExportFormat, overwrite: bool) -> Result<Value, String> {
        let result = self.export_inner(path, format, overwrite);
        match &result {
            Ok(report) => self.say(format!("exported {} ({} bytes, {})", path.display(), report["bytes"], format.name())),
            Err(e) => self.say(e.clone()),
        }
        result
    }

    fn export_inner(&mut self, path: &Path, format: ExportFormat, overwrite: bool) -> Result<Value, String> {
        if path.as_os_str().is_empty() {
            return Err("export: no output path".into());
        }
        if self.is_source(path) && !overwrite {
            return Err(format!("export {}: refused, it is the opened source (tick overwrite to replace it)", path.display()));
        }
        self.render_if_dirty();
        let Some(shown) = self.frame.as_ref() else {
            let why = self.render_error.as_ref().map_or_else(|| "nothing open".to_string(), ToString::to_string);
            return Err(format!("export {}: no rendered frame ({why})", path.display()));
        };
        // A frame that lives only in the display texture is rendered again on the read-back path.
        let read_back;
        let frame = if shown.rgba8.is_empty() {
            let snap = self.run.snapshot().ok_or_else(|| format!("export {}: nothing open", path.display()))?;
            read_back = render::render(snap, &self.assets, Prefer::Cpu, None, self.cancel).map_err(|e| format!("export {}: {e}", path.display()))?;
            &read_back
        } else {
            shown
        };
        let exported = export::encode_frame(frame, format).map_err(|code| format!("export {}: {code}", path.display()))?;
        export::write_atomic(path, &exported.bytes).map_err(|code| format!("export {}: {code}", path.display()))?;
        let mut report = exported.report();
        report["path"] = json!(path.to_string_lossy());
        report["backend"] = json!(frame.backend);
        report["revision"] = json!(frame.revision);
        report["render_notes"] = json!(frame.notes);
        Ok(report)
    }

    /// Saves the working session as a native package. Refuses the opened source path unless
    /// `overwrite` is set (the same guard as the CLI's `--save`).
    pub fn save(&mut self, path: &Path, overwrite: bool) -> Result<(), String> {
        if self.source.is_some() {
            let same = self.is_source(path);
            if same && !overwrite {
                let e = format!("save {}: refused, it is the opened source (tick overwrite to replace it)", path.display());
                self.say(e.clone());
                return Err(e);
            }
        }
        if self.run.save_to(path) {
            self.say(format!("saved {}", path.display()));
            Ok(())
        } else {
            let e = format!("save {}: {}", path.display(), self.op_error());
            self.say(e.clone());
            Err(e)
        }
    }

    /// Renders now if the document changed since the last frame.
    pub fn render_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(snap) = self.run.snapshot() else {
            return;
        };
        if self.prefer == Prefer::Gpu && self.gpu.is_none() && self.display.is_none() {
            self.gpu = Some(hsk_studio_render_gpu::GpuBackend::new(self.gpu_options));
        }
        let result = render::render_to(snap, &self.assets, self.prefer, self.gpu.as_ref(), self.display.as_mut().map(|d| &mut **d as &mut dyn GpuDisplay), self.cancel);
        match result {
            Ok(frame) => {
                let line = format!("rendered revision {} with render-{} ({}x{}){}", frame.revision, frame.backend, frame.width, frame.height, if frame.notes.is_empty() { String::new() } else { format!("; {}", frame.notes.join("; ")) });
                self.frame = Some(frame);
                self.render_error = None;
                self.texture = None;
                self.say(line);
            }
            Err(e) => {
                self.say(format!("render failed: {e}"));
                self.render_error = Some(e);
                self.frame = None;
                self.texture = None;
            }
        }
    }

    pub fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    pub fn render_error(&self) -> Option<&RenderFail> {
        self.render_error.as_ref()
    }

    pub fn receipt(&self) -> Option<&Value> {
        self.receipt.as_ref()
    }

    pub fn run(&self) -> &Run<'static> {
        &self.run
    }

    /// Widget names drawn by the last [`Viewer::ui`] call (see [`widget`]).
    pub fn drawn(&self) -> &[String] {
        &self.drawn
    }

    pub fn select(&mut self, layer_id: Option<String>) {
        self.selected = layer_id;
    }

    /// Structured projection of the viewer for agents and tests.
    pub fn state_json(&self) -> Value {
        let rows: Vec<Value> = self
            .layers()
            .iter()
            .map(|r| json!({"id": r.layer_id, "name": r.name, "kind": r.kind, "depth": r.depth, "visible": r.visible, "opacity": r.opacity}))
            .collect();
        json!({
            "source": self.source.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "revision": self.run.snapshot().map(|s| s.document().revision),
            "layers": rows,
            "selected": self.selected,
            "frame": self.frame.as_ref().map(|f| json!({"width": f.width, "height": f.height, "backend": f.backend, "display_texture": f.texture.is_some(), "revision": f.revision, "notes": f.notes})),
            "render_error": self.render_error.as_ref().map(|e| json!({"code": e.code, "detail": e.detail})),
            "receipt": self.receipt.as_ref().map(|r| json!({"source_format": r["source_format"], "layers": r["layers"], "loss_entries": r["loss"].as_array().map_or(0, Vec::len), "blending_space": r["blending_space"]})),
            "edit": self.edit_json(),
            "ops": self.run.ops(),
            "status": self.status,
        })
    }

    // ----- egui ---------------------------------------------------------------------------

    /// Draws one frame. Never opens a window: the host (eframe in `desktop`, a bare
    /// `egui::Context` in tests) owns the context.
    pub fn ui(&mut self, ctx: &egui::Context) {
        self.drawn.clear();
        let mut open_request = None;
        let mut save_request = None;
        let mut export_request = None;
        egui::TopBottomPanel::top(widget::id("toolbar")).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("File");
                ui.add(egui::TextEdit::singleline(&mut self.path_input).id(widget::id(widget::PATH_FIELD)).hint_text("path to .psd .psb .tif .dng .cr2 .png .jpg .pdf .handshake").desired_width(360.0));
                self.drawn.push(widget::PATH_FIELD.into());
                if ui.add(egui::Button::new("Open")).on_hover_text("Import or open the file named in the path field").clicked() {
                    open_request = Some(PathBuf::from(self.path_input.trim()));
                }
                self.drawn.push(widget::OPEN_BUTTON.into());
                ui.separator();
                ui.label("Save as");
                ui.add(egui::TextEdit::singleline(&mut self.save_input).id(widget::id(widget::SAVE_FIELD)).hint_text("out.handshake").desired_width(260.0));
                self.drawn.push(widget::SAVE_FIELD.into());
                ui.checkbox(&mut self.overwrite, "overwrite source");
                self.drawn.push(widget::SAVE_OVERWRITE.into());
                if ui.add_enabled(self.run.snapshot().is_some(), egui::Button::new("Save")).clicked() {
                    save_request = Some(PathBuf::from(self.save_input.trim()));
                }
                self.drawn.push(widget::SAVE_BUTTON.into());
                let exportable = self.frame.is_some();
                if ui.add_enabled(exportable, egui::Button::new("Export PNG")).on_hover_text("Composite as PNG at the save path with extension .png").clicked() {
                    export_request = Some(ExportFormat::Png);
                }
                self.drawn.push(widget::EXPORT_PNG_BUTTON.into());
                if ui.add_enabled(exportable, egui::Button::new("Export JPEG")).on_hover_text("Composite as JPEG (quality 90, transparency flattened on white) at the save path with extension .jpg").clicked() {
                    export_request = Some(ExportFormat::Jpeg { quality: export::DEFAULT_JPEG_QUALITY });
                }
                self.drawn.push(widget::EXPORT_JPEG_BUTTON.into());
                ui.separator();
                let mut gpu = self.prefer == Prefer::Gpu;
                if ui.checkbox(&mut gpu, "GPU").on_hover_text("render-gpu with render-cpu fallback; off = render-cpu only").changed() {
                    self.prefer = if gpu { Prefer::Gpu } else { Prefer::Cpu };
                    self.dirty = true;
                }
                self.drawn.push(widget::RENDER_BACKEND.into());
                if let Some(f) = &self.frame {
                    ui.label(format!("render-{} r{}", f.backend, f.revision));
                }
            });
            ui.horizontal_wrapped(|ui| self.tools_ui(ui));
        });
        self.shortcuts(ctx);
        if let Some(path) = open_request {
            let _ = self.open(&path);
        }
        if let Some(path) = save_request {
            let overwrite = self.overwrite;
            let _ = self.save(&path, overwrite);
        }
        if let Some(format) = export_request {
            let path = PathBuf::from(self.save_input.trim()).with_extension(if format == ExportFormat::Png { "png" } else { "jpg" });
            let overwrite = self.overwrite;
            let _ = self.export(&path, format, overwrite);
        }
        self.render_if_dirty();

        egui::TopBottomPanel::bottom(widget::id(widget::STATUS_PANEL)).resizable(true).default_height(90.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                for line in &self.status {
                    ui.label(line);
                }
            });
        });
        self.drawn.push(widget::STATUS_PANEL.into());

        let mut toggles = Vec::new();
        let mut layer_action: Option<&'static str> = None;
        egui::SidePanel::left(widget::id(widget::LAYER_PANEL)).resizable(true).default_width(240.0).show(ctx, |ui| {
            ui.heading("Layers");
            let open = self.run.snapshot().is_some();
            let has_selection = self.selected.is_some();
            ui.horizontal_wrapped(|ui| {
                for (name, label, hover, needs_selection) in [
                    (widget::LAYER_NEW, "New", "New empty raster layer above the selected layer", false),
                    (widget::LAYER_DUPLICATE, "Dup", "Duplicate the selected layer (a group with its contents)", true),
                    (widget::LAYER_DELETE, "Del", "Delete the selected layer (a group with its contents)", true),
                    (widget::LAYER_UP, "Up", "Move the selected layer one step up", true),
                    (widget::LAYER_DOWN, "Down", "Move the selected layer one step down", true),
                    (widget::LAYER_GROUP, "Group", "Put the selected layer into a new group", true),
                    (widget::LAYER_UNGROUP, "Ungroup", "Move the selected group's layers out and delete the group", true),
                ] {
                    let enabled = open && (has_selection || !needs_selection);
                    if ui.push_id(widget::id(name), |ui| ui.add_enabled(enabled, egui::Button::new(label))).inner.on_hover_text(hover).clicked() {
                        layer_action = Some(name);
                    }
                    self.drawn.push(name.into());
                }
            });
            egui::ScrollArea::vertical().show(ui, |ui| {
                for row in self.layers() {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0 * row.depth as f32);
                        let mut visible = row.visible;
                        let vis_id = widget::layer(&row.layer_id, "visible");
                        if ui.push_id(widget::id(&vis_id), |ui| ui.checkbox(&mut visible, "")).inner.on_hover_text("Visible").changed() {
                            toggles.push((row.layer_id.clone(), visible));
                        }
                        self.drawn.push(vis_id);
                        let sel_id = widget::layer(&row.layer_id, "select");
                        let selected = self.selected.as_deref() == Some(row.layer_id.as_str());
                        let label = format!("{}  {:.0}%{}", row.name, row.opacity, if row.kind == "group" { "  (group)" } else { "" });
                        if ui.push_id(widget::id(&sel_id), |ui| ui.selectable_label(selected, label)).inner.clicked() {
                            self.selected = Some(row.layer_id.clone());
                        }
                        self.drawn.push(sel_id);
                    });
                }
            });
        });
        self.drawn.push(widget::LAYER_PANEL.into());
        for (layer, visible) in toggles {
            let _ = self.set_visible(&layer, visible);
        }
        if let Some(action) = layer_action {
            self.layer_action(action);
        }

        let mut commits = Vec::new();
        egui::SidePanel::right(widget::id(widget::PROPERTIES_PANEL)).resizable(true).default_width(300.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Properties");
                commits = self.properties_ui(ui);
                ui.separator();
                ui.heading("Import receipt");
                self.receipt_ui(ui);
            });
        });
        self.drawn.push(widget::PROPERTIES_PANEL.into());
        self.drawn.push(widget::RECEIPT_PANEL.into());
        for (layer, control, text) in commits {
            let _ = self.commit(&layer, &control, &text);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(if self.edit.selection.is_some() { 120 } else { 1000 }));
        self.render_if_dirty();

        egui::CentralPanel::default().show(ctx, |ui| self.canvas_ui(ui));
    }

    /// Controls-descriptor panel of the selected layer; returns the routed commits to perform.
    fn properties_ui(&mut self, ui: &mut egui::Ui) -> Vec<(String, String, String)> {
        let mut commits = Vec::new();
        let Some(layer_id) = self.selected.clone() else {
            ui.label("Select a layer.");
            return commits;
        };
        let Some(layer) = self.run.snapshot().and_then(|s| s.document().layers.iter().find(|l| l.layer_id == layer_id).cloned()) else {
            ui.label("The selected layer is gone.");
            return commits;
        };
        let mut built = match panel::build(&PanelValues::of(&layer)) {
            Ok(p) => p,
            Err(e) => {
                ui.colored_label(egui::Color32::RED, format!("panel: {}", e.code()));
                return commits;
            }
        };
        ui.label(format!("{} ({})", layer.name, layer.layer_id));
        let outcome = hsk_studio_controls::egui_adapter::show_panel_and_apply(ui, &built.registry, &mut built.store, None);
        for change in outcome.changes.iter().filter(|c| c.is_undo_step()) {
            match built.routes.get(&change.control_id).copied() {
                Some(Route::ChronicleName | Route::ChronicleVisible | Route::ChronicleOpacity | Route::ChronicleBlendMode) => {
                    let text = match &change.new {
                        hsk_studio_controls::Value::Bool(b) => b.to_string(),
                        hsk_studio_controls::Value::Text(t) => t.clone(),
                        other => built.registry.get(&change.control_id).map_or_else(String::new, |s| s.format_value(other)),
                    };
                    commits.push((layer_id.clone(), change.control_id.clone(), text));
                }
                Some(Route::None(reason)) => self.say(format!("{} is read-only here: {reason}", change.control_id)),
                None => self.say(format!("{} has no commit route", change.control_id)),
            }
        }
        ui.small("Name, Visible, Opacity and Blend mode commit through the session (chronicle, undoable). Fill opacity, Clipped and Isolation are read-only: chronicle has no property for them.");
        commits
    }

    fn receipt_ui(&mut self, ui: &mut egui::Ui) {
        let Some(r) = &self.receipt else {
            ui.label("No import receipt (a native package without import entries, or nothing open).");
            return;
        };
        ui.label(format!("source format: {}", r["source_format"].as_str().unwrap_or("?")));
        ui.label(format!("layers: {}", r["layers"]));
        if let Some(colour) = r.get("colour") {
            ui.label(format!("colour: {}", colour["status"].as_str().unwrap_or("?")));
        }
        let loss = r["loss"].as_array().cloned().unwrap_or_default();
        egui::CollapsingHeader::new(format!("Loss entries ({})", loss.len())).id_salt(widget::id("receipt.loss")).default_open(true).show(ui, |ui| {
            egui::Grid::new(widget::id("receipt.loss.grid")).striped(true).show(ui, |ui| {
                ui.strong("path");
                ui.strong("kind");
                ui.strong("code");
                ui.end_row();
                for e in &loss {
                    ui.label(e["path"].as_str().unwrap_or(""));
                    ui.label(e["kind"].as_str().unwrap_or(""));
                    ui.label(e["code"].as_str().unwrap_or(""));
                    ui.end_row();
                }
            });
        });
        if let Some(np) = r["not_proven"].as_array() {
            egui::CollapsingHeader::new(format!("Not proven ({})", np.len())).id_salt(widget::id("receipt.not_proven")).show(ui, |ui| {
                for v in np {
                    ui.label(v.as_str().unwrap_or(""));
                }
            });
        }
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.push_id(widget::id(widget::FIT_BUTTON), |ui| ui.button("Fit")).inner.clicked() {
                self.fit = true;
                self.pan = egui::Vec2::ZERO;
            }
            self.drawn.push(widget::FIT_BUTTON.into());
            if ui.push_id(widget::id(widget::ACTUAL_SIZE_BUTTON), |ui| ui.button("1:1")).inner.clicked() {
                self.fit = false;
                self.zoom = 1.0;
                self.pan = egui::Vec2::ZERO;
            }
            self.drawn.push(widget::ACTUAL_SIZE_BUTTON.into());
            let mut zoom = self.zoom;
            if ui.push_id(widget::id(widget::ZOOM_SLIDER), |ui| ui.add(egui::Slider::new(&mut zoom, 0.05..=16.0).logarithmic(true).text("zoom"))).inner.changed() {
                self.zoom = zoom;
                self.fit = false;
            }
            self.drawn.push(widget::ZOOM_SLIDER.into());
        });
        if let Some(e) = &self.render_error {
            ui.colored_label(egui::Color32::RED, format!("render failed: {e}"));
        }
        let Some(frame) = &self.frame else {
            if self.run.snapshot().is_none() {
                ui.label("Open a file: type its path above and press Open (or pass it as the first argument).");
            }
            return;
        };
        let (w, h) = (frame.width, frame.height);
        let id = match frame.texture {
            // Rendered on the host's device straight into this texture: nothing to upload.
            Some(id) => id,
            None => self
                .texture
                .get_or_insert_with(|| {
                    let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &frame.rgba8);
                    ui.ctx().load_texture("hsk-studio-ui.view.texture", image, egui::TextureOptions::NEAREST)
                })
                .id(),
        };
        let (fx, fy) = (frame.x as f32, frame.y as f32);
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        self.drawn.push(widget::CANVAS.into());
        let size = egui::vec2(w as f32, h as f32);
        if self.fit {
            self.zoom = (rect.width() / size.x).min(rect.height() / size.y).clamp(0.05, 16.0);
        }
        let tool = self.edit.tool;
        if tool == Tool::View && response.dragged() {
            let d = response.drag_delta();
            self.pan = egui::vec2(self.pan.x + d.x, self.pan.y + d.y);
            self.fit = false;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.zoom = (self.zoom * (1.0 + scroll * 0.002)).clamp(0.05, 16.0);
                self.fit = false;
            }
        }
        let centre = egui::pos2(rect.center().x + self.pan.x, rect.center().y + self.pan.y);
        let shown = egui::Rect::from_center_size(centre, egui::vec2(size.x * self.zoom, size.y * self.zoom));
        let zoom = self.zoom;
        // Screen <-> document pixels.
        let to_doc = |p: egui::Pos2| [f64::from(fx + (p.x - shown.min.x) / zoom), f64::from(fy + (p.y - shown.min.y) / zoom)];
        let to_screen = |d: [f32; 2]| egui::pos2(shown.min.x + (d[0] - fx) * zoom, shown.min.y + (d[1] - fy) * zoom);

        // Tool gestures (every action below is a headless Viewer method).
        if tool != Tool::View {
            if response.drag_started() {
                self.edit.gesture.clear();
            }
            if response.dragged()
                && let Some(p) = response.interact_pointer_pos()
            {
                // Tablet pressure when the platform reports a touch force; the mouse is pressure 1.
                let force = ui.input(|i| i.events.iter().rev().find_map(|e| if let egui::Event::Touch { force: Some(f), .. } = e { Some(*f) } else { None }));
                let [x, y] = to_doc(p);
                let sample = StrokeSample { x, y, pressure: Some(force.unwrap_or(1.0).clamp(0.0, 1.0)), time_ms: None };
                match tool {
                    Tool::RectSelect | Tool::EllipseSelect if self.edit.gesture.len() >= 2 => {
                        if let Some(last) = self.edit.gesture.last_mut() {
                            *last = sample;
                        }
                    }
                    _ => self.edit.gesture.push(sample),
                }
            }
            if response.drag_stopped() {
                let points: Vec<[f64; 2]> = self.edit.gesture.iter().map(|s| [s.x, s.y]).collect();
                let samples = std::mem::take(&mut self.edit.gesture);
                let corners = (points.first().copied(), points.last().copied());
                let _ = match (tool, corners) {
                    (Tool::RectSelect, (Some(a), Some(b))) => self.select_rect(a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()),
                    (Tool::EllipseSelect, (Some(a), Some(b))) => self.select_ellipse(a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()),
                    (Tool::Lasso, _) => self.select_lasso(&points),
                    (Tool::Brush, _) => match self.selected.clone() {
                        Some(layer) => self.paint(&layer, &samples).map(|_| ()),
                        None => {
                            self.say("brush: select a raster layer in the layer panel first".into());
                            Ok(())
                        }
                    },
                    _ => Ok(()),
                };
            }
            if tool == Tool::Text && response.clicked()
                && let Some(p) = response.interact_pointer_pos()
            {
                let [x, y] = to_doc(p);
                let _ = self.place_text(x, y, "Text");
            }
        }

        let painter = ui.painter_at(rect);
        checker(&painter, shown.intersect(rect));
        painter.image(id, shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);

        // Gesture preview.
        let g: Vec<egui::Pos2> = self.edit.gesture.iter().map(|s| to_screen([s.x as f32, s.y as f32])).collect();
        let line = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(40, 140, 255));
        match (tool, g.first(), g.last()) {
            (Tool::RectSelect, Some(a), Some(b)) => {
                painter.rect_stroke(egui::Rect::from_two_pos(*a, *b), 0.0, line, egui::StrokeKind::Middle);
            }
            (Tool::EllipseSelect, Some(a), Some(b)) => {
                let r = egui::Rect::from_two_pos(*a, *b);
                let pts: Vec<egui::Pos2> = (0..=64)
                    .map(|i| {
                        let t = i as f32 / 64.0 * std::f32::consts::TAU;
                        egui::pos2(r.center().x + r.width() / 2.0 * t.cos(), r.center().y + r.height() / 2.0 * t.sin())
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, line));
            }
            (Tool::Lasso | Tool::Brush, Some(_), Some(_)) => {
                let stroke = if tool == Tool::Brush {
                    let [r, gg, bb] = self.edit.brush.rgb8;
                    egui::Stroke::new((self.edit.brush.diameter_px as f32 * zoom).max(1.0), egui::Color32::from_rgba_unmultiplied(r, gg, bb, 120))
                } else {
                    line
                };
                painter.add(egui::Shape::line(g, stroke));
            }
            _ => {}
        }

        // Marching ants: the 50 % outline of the selection, black under white dashes that move.
        if !self.edit.ants.is_empty() {
            let phase = (ui.input(|i| i.time) * 12.0 % 8.0) as f32;
            let mut shapes = Vec::with_capacity(self.edit.ants.len().saturating_mul(2));
            for seg in &self.edit.ants {
                let pts = [to_screen(seg[0]), to_screen(seg[1])];
                if !rect.intersects(egui::Rect::from_two_pos(pts[0], pts[1]).expand(1.0)) {
                    continue;
                }
                shapes.push(egui::Shape::line_segment(pts, egui::Stroke::new(1.0_f32, egui::Color32::BLACK)));
                shapes.extend(egui::Shape::dashed_line_with_offset(&pts, egui::Stroke::new(1.0_f32, egui::Color32::WHITE), &[4.0], &[4.0], phase));
            }
            painter.extend(shapes);
        }
    }

    /// Tool row: tools, selection combine mode, brush settings, undo/redo (all with stable ids).
    fn tools_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Tool");
        for tool in Tool::ALL {
            let name = widget::tool(tool.key());
            let hover = match tool {
                Tool::View => "Drag pans the view",
                Tool::RectSelect => "Drag a rectangular selection",
                Tool::EllipseSelect => "Drag an elliptical selection",
                Tool::Lasso => "Drag a freehand selection",
                Tool::Brush => "Paint on the selected raster layer (clipped to the selection)",
                Tool::Text => edit::TEXT_TOOL_GAP,
            };
            if ui.push_id(widget::id(&name), |ui| ui.selectable_label(self.edit.tool == tool, tool.label())).inner.on_hover_text(hover).clicked() {
                self.set_tool(tool);
            }
            self.drawn.push(name);
        }
        ui.separator();
        let mut combine = self.edit.combine;
        ui.push_id(widget::id(widget::COMBINE), |ui| {
            egui::ComboBox::from_label("combine").selected_text(edit::combine_key(combine)).show_ui(ui, |ui| {
                for mode in [CombineMode::Replace, CombineMode::Extend, CombineMode::Diminish, CombineMode::Intersect] {
                    ui.selectable_value(&mut combine, mode, edit::combine_key(mode));
                }
            });
        });
        self.drawn.push(widget::COMBINE.into());
        if combine != self.edit.combine {
            self.set_combine(combine);
        }
        if ui.push_id(widget::id(widget::CLEAR_SELECTION), |ui| ui.add_enabled(self.edit.selection.is_some(), egui::Button::new("Deselect"))).inner.clicked() {
            self.clear_selection();
        }
        self.drawn.push(widget::CLEAR_SELECTION.into());
        ui.separator();
        let mut brush = self.edit.brush;
        ui.push_id(widget::id(widget::BRUSH_SIZE), |ui| ui.add(egui::Slider::new(&mut brush.diameter_px, 1.0..=500.0).logarithmic(true).text("size")));
        self.drawn.push(widget::BRUSH_SIZE.into());
        ui.push_id(widget::id(widget::BRUSH_HARDNESS), |ui| ui.add(egui::Slider::new(&mut brush.hardness_pct, 0.0..=100.0).text("hard")));
        self.drawn.push(widget::BRUSH_HARDNESS.into());
        ui.push_id(widget::id(widget::BRUSH_OPACITY), |ui| ui.add(egui::Slider::new(&mut brush.opacity_pct, 0.0..=100.0).text("opacity")));
        self.drawn.push(widget::BRUSH_OPACITY.into());
        ui.push_id(widget::id(widget::BRUSH_COLOUR), |ui| ui.color_edit_button_srgb(&mut brush.rgb8));
        self.drawn.push(widget::BRUSH_COLOUR.into());
        if brush != self.edit.brush {
            self.set_brush(brush);
        }
        ui.separator();
        let history = self.run.history();
        let can = |k: &str| history[k].as_array().is_some_and(|a| !a.is_empty());
        if ui.push_id(widget::id(widget::UNDO_BUTTON), |ui| ui.add_enabled(can("undo"), egui::Button::new("Undo"))).inner.on_hover_text("Ctrl+Z").clicked() {
            let _ = self.undo();
        }
        self.drawn.push(widget::UNDO_BUTTON.into());
        if ui.push_id(widget::id(widget::REDO_BUTTON), |ui| ui.add_enabled(can("redo"), egui::Button::new("Redo"))).inner.on_hover_text("Ctrl+Shift+Z / Ctrl+Y").clicked() {
            let _ = self.redo();
        }
        self.drawn.push(widget::REDO_BUTTON.into());
    }

    /// Runs a layer-panel button (a widget name of [`widget`]) on the selected layer.
    pub fn layer_action(&mut self, action: &str) {
        let selected = self.selected.clone();
        let result = match (action, selected.as_deref()) {
            (widget::LAYER_NEW, _) => self.new_raster_layer("Layer").map(|_| ()),
            (widget::LAYER_DUPLICATE, Some(id)) => self.duplicate_layer(id).map(|_| ()),
            (widget::LAYER_DELETE, Some(id)) => self.delete_layer(id).map(|_| ()),
            (widget::LAYER_UP, Some(id)) => self.shift_layer(id, true).map(|_| ()),
            (widget::LAYER_DOWN, Some(id)) => self.shift_layer(id, false).map(|_| ()),
            (widget::LAYER_GROUP, Some(id)) => self.group_layers(&[id.to_string()], "Group").map(|_| ()),
            (widget::LAYER_UNGROUP, Some(id)) => self.ungroup(id).map(|_| ()),
            _ => Err(format!("{action}: select a layer first")),
        };
        if let Err(e) = result {
            self.say(e);
        }
    }

    /// Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y when no text field has keyboard focus.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let redo_shift = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
        let redo_y = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y);
        let undo = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
        if ctx.input_mut(|i| i.consume_shortcut(&redo_shift) || i.consume_shortcut(&redo_y)) {
            let _ = self.redo();
        } else if ctx.input_mut(|i| i.consume_shortcut(&undo)) {
            let _ = self.undo();
        }
    }
}

/// Transparency checkerboard behind the image.
fn checker(painter: &egui::Painter, rect: egui::Rect) {
    if !rect.is_positive() {
        return;
    }
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(200));
    let step = 12.0;
    let mut y = rect.top();
    let mut row = 0u32;
    while y < rect.bottom() {
        let mut x = rect.left() + if row.is_multiple_of(2) { 0.0 } else { step };
        while x < rect.right() {
            let cell = egui::Rect::from_min_max(egui::pos2(x, y), egui::pos2((x + step).min(rect.right()), (y + step).min(rect.bottom())));
            painter.rect_filled(cell, 0.0, egui::Color32::from_gray(150));
            x += 2.0 * step;
        }
        y += step;
        row = row.wrapping_add(1);
    }
}
