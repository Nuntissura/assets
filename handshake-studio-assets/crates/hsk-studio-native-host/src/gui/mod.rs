//! Feature `document`: the GUI frame (SMB-GUI-SPINE-1). A real document in the Viewport, a right dock (300) with one
//! group, the Layers tab and its panel built with kumihimo-ui, and a status bar; one Interaction, one Text and one
//! Panel per region (lead F7 ruling). Chrome is the CPU painter (decision O6), painted into one window-size canvas
//! that the host copies into its pane: `cpu painter, Vulkan UI pass pending`.
//!
//! Input: typed [`HostEvent`]s in physical px, divided by the scale into logical px, routed to the panel under the
//! pointer; a release inside a control gives its author_id, the command table maps it to a public Viewer method
//! (select, set_visible as the named undo step `panel_set visible <layer_id>`, undo, redo), the frame re-reads the
//! document once (one `state_json` per revision: there is no selection getter), bumps its state revision and the
//! panels rebuild. Idle frames rebuild, paint and upload nothing. No winit or Vulkan type is held here: the frame runs
//! and tests on the CPU alone; the pane copy goes through [`PaneSink`].
use crate::capture::Json;
use crate::error::HostError;
use crate::event::{Button, HostEvent, Key, Mods};
use crate::scene::DocumentFacts;
use crate::script::Step;
use hsk_studio_accord::ActorContext;
use hsk_studio_harness_view::view::{LayerRow, Viewer};
use hsk_studio_vulkan::{GpuClientPort, PaneId, Refusal, rgba_len};
use kumihimo_ui::checks::{self, Clipped};
use kumihimo_ui::display::{IconId, ImageId};
use kumihimo_ui::gallery::GalleryIcons;
use kumihimo_ui::geom::{Point, Rect, Scale};
use kumihimo_ui::paint::{Canvas, IconMask, ImageRef, Resources, paint};
use kumihimo_ui::semantic::{Semantic, semantic};
use kumihimo_ui::text::{BoxText, MAX_LABEL_BYTES, Text, TextStats};
use kumihimo_ui::tokens::{ColorToken, Theme, size};
use kumihimo_ui::tree::{BuildError, Interaction, Node, Panel, Tree};
use std::path::{Path, PathBuf};

pub mod view;

pub const VIEWER_ID: &str = "studio.viewer.raster";
pub const DOCK_ID: &str = "studio.dock.right";
pub const GROUP_ID: &str = "studio.group.layers";
pub const TAB_ID: &str = "studio.tab.layers";
pub const BODY_ID: &str = "studio.panel.layers";
pub const LIST_ID: &str = "studio.panel.layers.list";
pub const ROW_PREFIX: &str = "studio.panel.layers.row.";
pub const SCROLL_ID: &str = "studio.scroll.layers";
pub const EMPTY_ID: &str = "studio.panel.layers.empty";
pub const STATUS_ID: &str = "studio.status";
/// Nodes the tree file keeps per panel (STU-MDL-108: truncation is explicit).
pub const MAX_TREE_NODES: usize = 65_536;
/// The document image in the frame's resource source.
pub const DOC_IMAGE: ImageId = ImageId(100);
/// Largest font file read for the UI text.
const MAX_FONT_BYTES: u64 = 32 << 20;
const INTER_REGULAR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
const INTER_BOLD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../hsk-studio-interop-fig/tests/fixtures/Inter-Bold.ttf");

/// The author_id of a layer's row (lowercased: the STU-MDL-100 grammar takes `[a-z0-9-]` only).
pub fn row_id(layer_id: &str) -> String {
    format!("{ROW_PREFIX}{}", layer_id.to_ascii_lowercase())
}

/// Frame regions in logical px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Regions {
    pub window: Rect,
    pub viewport: Rect,
    pub dock: Rect,
    pub header: Rect,
    pub body: Rect,
    pub status: Rect,
}

/// Lays the frame out at a physical client extent and scale: status bar 24 at the bottom, a right dock 300 wide
/// (size::DOCK_INSPECT_W) with its group header 26, the Viewport in the rest. Adjacent regions share the same f32
/// edge, so the one snapping function gives them one physical edge (AC-GEOM-03).
pub fn regions(width: u32, height: u32, scale: Scale) -> Regions {
    let s = scale.get();
    let (w, h) = (width as f32 / s, height as f32 / s);
    let main = (h - size::FRAME_STATUS_BAR_H).max(0.0);
    let dock_x = (w - size::DOCK_INSPECT_W).max(0.0);
    let header_y = size::DOCK_GROUP_HEADER_H.min(main);
    Regions {
        window: Rect::new(0.0, 0.0, w, h),
        viewport: Rect::new(0.0, 0.0, dock_x, main),
        dock: Rect::new(dock_x, 0.0, w, main),
        header: Rect::new(dock_x, 0.0, w, header_y),
        body: Rect::new(dock_x, header_y, w, main),
        status: Rect::new(0.0, main, w, h),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuiStats {
    /// Panel rebuilds, summed over the three panels.
    pub builds: u64,
    pub paints: u64,
    pub uploads: u64,
    pub commands: u64,
    pub refused: u64,
    pub events: u64,
    /// Frames that rebuilt and painted nothing.
    pub idle_frames: u64,
}

/// One command a control ran: its id and `ok` or the refusal code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub command: &'static str,
    pub code: String,
}

/// Where the painted frame goes. [`PortPane`] is every GPU client port with its pane; a test fake implements this
/// directly (a fake `GpuClientPort` cannot mint a `PaneId`, whose field is private to hsk-studio-vulkan).
pub trait PaneSink {
    fn upload(&mut self, rgba8: &[u8]) -> Result<(), Refusal>;
}

pub struct PortPane<'a, P: GpuClientPort + ?Sized> {
    pub port: &'a mut P,
    pub pane: PaneId,
}

impl<P: GpuClientPort + ?Sized> PaneSink for PortPane<'_, P> {
    fn upload(&mut self, rgba8: &[u8]) -> Result<(), Refusal> {
        self.port.upload_pane(self.pane, rgba8)
    }
}

enum Cmd {
    SetVisible { layer: String, visible: bool },
    Select(Option<String>),
    Undo,
    Redo,
}

struct DocImage {
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
    revision: u64,
}

/// Icons from the gallery stand-in (no icon set yet), plus the document image.
struct Res<'a> {
    icons: &'a mut GalleryIcons,
    image: Option<&'a DocImage>,
}
impl Resources for Res<'_> {
    fn icon(&mut self, icon: IconId, px: u32) -> Option<IconMask> {
        if icon == view::ICON_NONE {
            let n = px.clamp(1, 512);
            return Some(IconMask { width: n, height: n, rg: vec![[0, 0]; (n as usize) * (n as usize)] });
        }
        self.icons.icon(icon, px)
    }
    fn image(&self, image: ImageId) -> Option<ImageRef<'_>> {
        let img = self.image.filter(|_| image == DOC_IMAGE)?;
        Some(ImageRef { width: img.width, height: img.height, rgba: &img.rgba8 })
    }
}

const VIEWPORT: usize = 0;
const DOCK: usize = 1;
const STATUS: usize = 2;

pub struct GuiFrame {
    viewer: Viewer,
    file_name: String,
    text: Text,
    text_kind: &'static str,
    icons: GalleryIcons,
    ui: Interaction,
    theme: Theme,
    extent: (u32, u32),
    scale: Scale,
    scale_locked: bool,
    regions: Regions,
    panels: [Panel; 3],
    state_rev: u64,
    rows: Vec<LayerRow>,
    selected: Option<String>,
    revision: u64,
    image: Option<DocImage>,
    backend: &'static str,
    render_code: Option<&'static str>,
    first_row: usize,
    /// Wheel travel below one row, thousandths of a row (cleared when the pointer leaves).
    wheel_rest: i64,
    pointer: Option<Point>,
    press: Option<usize>,
    mods: Mods,
    minimized: bool,
    canvas: Option<Canvas>,
    repaint: bool,
    pending_upload: bool,
    stats: GuiStats,
    last: Option<Outcome>,
    device: String,
    notes: Vec<&'static str>,
}

/// The scale for thousandths `milli`: outside 0.5..=8 it is clamped and reported (`true`).
fn scale_of(milli: u32) -> (Scale, bool) {
    let s = milli as f32 / 1000.0;
    let c = s.clamp(0.5, 8.0);
    (Scale::new(c).unwrap_or(Scale::ONE), c != s)
}

fn read_font(path: &Path) -> Option<Vec<u8>> {
    let len = std::fs::metadata(path).ok()?.len();
    (len <= MAX_FONT_BYTES).then(|| std::fs::read(path).ok()).flatten()
}

/// Inter through hsk-studio-ui-text (B3); without the font files, the box stand-in, and the receipt says so.
fn ui_text(actor: &ActorContext) -> (Text, &'static str) {
    let (regular, bold) = match std::env::var_os("HSK_STUDIO_UI_FONTS").map(PathBuf::from) {
        Some(dir) => (dir.join("Inter-Regular.ttf"), dir.join("Inter-Bold.ttf")),
        None => (PathBuf::from(INTER_REGULAR), PathBuf::from(INTER_BOLD)),
    };
    match (read_font(&regular), read_font(&bold)) {
        (Some(r), Some(b)) => {
            let mut t = hsk_studio_ui_text::TypeText::new(actor.clone());
            t.add_face("Inter", 400, r);
            t.add_face("Inter", 700, b);
            (Text::new(Box::new(t)), "inter")
        }
        _ => (Text::new(Box::new(BoxText)), "box_stand_in"),
    }
}

fn build_code(e: &BuildError) -> &'static str {
    match e {
        BuildError::DuplicateId(_) => "duplicate_id",
        BuildError::InvalidId(_) => "invalid_id",
        BuildError::DuplicateStableId(_) => "duplicate_stable_id",
        BuildError::Unbalanced => "unbalanced",
        BuildError::TooManyNodes => "too_many_nodes",
    }
}

/// The text up to `max` bytes, cut at a char boundary.
pub fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl GuiFrame {
    /// Opens `path` with the Viewer on render-cpu and lays the frame out at `width x height` physical px. `scale_milli`
    /// fixes the UI scale (window ScaleFactor events are then ignored); None starts at 1 and follows them.
    pub fn open(path: &Path, width: u32, height: u32, scale_milli: Option<u32>) -> Result<Self, HostError> {
        rgba_len(width, height).map_err(|_| HostError::Usage("size"))?;
        let viewer = crate::document::open_viewer(path)?;
        let (scale, clamped) = scale_milli.map_or((Scale::ONE, false), scale_of);
        let (text, text_kind) = ui_text(viewer.run().actor());
        let mut f = Self {
            viewer,
            file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            text,
            text_kind,
            icons: GalleryIcons::default(),
            ui: Interaction::default(),
            theme: Theme::Dark,
            extent: (width, height),
            scale,
            scale_locked: scale_milli.is_some(),
            regions: regions(width, height, scale),
            panels: Default::default(),
            state_rev: 0,
            rows: Vec::new(),
            selected: None,
            revision: 0,
            image: None,
            backend: "none",
            render_code: None,
            first_row: 0,
            wheel_rest: 0,
            pointer: None,
            press: None,
            mods: Mods::default(),
            minimized: false,
            canvas: None,
            repaint: true,
            pending_upload: false,
            stats: GuiStats::default(),
            last: None,
            device: "no device".into(),
            notes: Vec::new(),
        };
        if clamped {
            f.note("scale_clamped");
        }
        f.refresh();
        Ok(f)
    }

    fn note(&mut self, code: &'static str) {
        if !self.notes.contains(&code) && self.notes.len() < 32 {
            self.notes.push(code);
        }
    }

    /// Renders what changed and re-reads the document: rows, selection and revision from one `state_json` (the
    /// selection has no getter; `Viewer::layers` runs once inside it).
    fn refresh(&mut self) {
        let want = self.viewer.run().snapshot().map(|s| s.document().revision);
        // A superseded preview re-marks the viewer dirty; three passes bound that (document.rs).
        for _ in 0..3 {
            self.viewer.render_if_dirty();
            if self.viewer.render_error().is_some() || self.viewer.frame().map(|f| f.revision) == want {
                break;
            }
        }
        self.render_code = self.viewer.render_error().map(|e| e.code);
        match self.viewer.frame() {
            Some(fr) if self.render_code.is_none() => {
                if self.image.as_ref().map(|i| (i.revision, i.width, i.height)) != Some((fr.revision, fr.width, fr.height)) {
                    if rgba_len(fr.width, fr.height).ok() == Some(fr.rgba8.len()) {
                        self.image = Some(DocImage { width: fr.width, height: fr.height, rgba8: fr.rgba8.clone(), revision: fr.revision });
                        self.backend = fr.backend;
                    } else {
                        self.image = None;
                        self.render_code = Some("frame");
                    }
                }
            }
            // Never stale content from an earlier revision (empty_state rule).
            _ => self.image = None,
        }
        let st = self.viewer.state_json();
        self.revision = st["revision"].as_u64().unwrap_or(0);
        self.selected = st["selected"].as_str().map(str::to_owned);
        self.rows = st["layers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| LayerRow {
                        layer_id: r["id"].as_str().unwrap_or_default().to_owned(),
                        name: cut(r["name"].as_str().unwrap_or_default(), MAX_LABEL_BYTES).to_owned(),
                        kind: r["kind"].as_str().unwrap_or_default().to_owned(),
                        depth: usize::try_from(r["depth"].as_u64().unwrap_or(0)).unwrap_or(usize::MAX),
                        visible: r["visible"].as_bool().unwrap_or(true),
                        opacity: r["opacity"].as_f64().unwrap_or(1.0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.first_row = self.first_row.min(view::max_first(self.rows.len(), self.regions.body.h()));
        self.bump();
        self.repaint = true;
    }

    fn bump(&mut self) {
        self.state_rev = self.state_rev.wrapping_add(1);
    }

    fn relayout(&mut self) {
        self.regions = regions(self.extent.0, self.extent.1, self.scale);
        self.first_row = self.first_row.min(view::max_first(self.rows.len(), self.regions.body.h()));
        self.repaint = true;
    }

    pub fn extent(&self) -> (u32, u32) {
        self.extent
    }
    pub fn scale(&self) -> Scale {
        self.scale
    }
    pub fn regions(&self) -> Regions {
        self.regions
    }
    pub fn rows(&self) -> &[LayerRow] {
        &self.rows
    }
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn stats(&self) -> GuiStats {
        self.stats
    }
    pub fn last(&self) -> Option<&Outcome> {
        self.last.as_ref()
    }
    pub fn notes(&self) -> &[&'static str] {
        &self.notes
    }
    pub fn text_kind(&self) -> &'static str {
        self.text_kind
    }
    pub fn text_stats(&self) -> TextStats {
        self.text.stats()
    }
    pub fn viewer(&self) -> &Viewer {
        &self.viewer
    }
    /// Runs `f` on the viewer, then re-reads the document as after a command.
    pub fn edit(&mut self, f: impl FnOnce(&mut Viewer)) {
        f(&mut self.viewer);
        self.refresh();
    }
    pub fn panels(&self) -> [&Panel; 3] {
        [&self.panels[VIEWPORT], &self.panels[DOCK], &self.panels[STATUS]]
    }
    pub fn node(&self, author_id: &str) -> Option<&Node> {
        self.panels.iter().find_map(|p| p.tree().find(author_id).map(|id| p.tree().get(id)))
    }
    /// Centre of a node in physical client px.
    pub fn centre(&self, author_id: &str) -> Option<(i32, i32)> {
        let b = self.node(author_id)?.bounds;
        let s = self.scale.get();
        Some((((b.x0 + b.x1) / 2.0 * s).floor() as i32, ((b.y0 + b.y1) / 2.0 * s).floor() as i32))
    }
    /// Where the document image sits, logical px.
    pub fn image_rect(&self) -> Option<Rect> {
        self.image.as_ref().map(|i| view::image_rect(self.regions.viewport, i.width, i.height))
    }
    /// The device and backend readout of the status bar (AC-BEHAV-07; the host passes the Owner's DeviceInfo).
    pub fn set_device(&mut self, readout: String) {
        if self.device != readout {
            self.device = readout;
            self.bump();
        }
    }
    /// Document facts for the receipt.
    pub fn facts(&self) -> DocumentFacts {
        DocumentFacts { file_name: self.file_name.clone(), revision: self.revision, backend: self.backend.to_string(), layers: self.rows.len() as u64 }
    }

    /// Physical pixel to the logical point the hit test uses. The painter gives pixel `p` to the node whose snapped
    /// edges hold it, `round(y0 * s) <= p < round(y1 * s)`, which is `y0 * s < p + 0.5 <= y1 * s`; the hit test is
    /// `y0 <= q < y1`. Taking `q` just under the pixel centre, `(p + 0.49) / s`, makes them agree at every edge whose
    /// physical fraction is outside (0.49, 0.5) (review F5: at scale 1.2 pixel 146 shows the fourth row and now clicks
    /// it; with `p / s` it clicked the third).
    fn logical(&self, x: i32, y: i32) -> Option<Point> {
        let s = self.scale.get();
        let p = Point::new((x as f32 + 0.49) / s, (y as f32 + 0.49) / s);
        self.regions.window.contains(p).then_some(p)
    }
    fn panel_at(&self, p: Point) -> Option<usize> {
        let r = &self.regions;
        [(VIEWPORT, r.viewport), (DOCK, r.dock), (STATUS, r.status)].into_iter().find(|(_, q)| q.contains(p)).map(|(i, _)| i)
    }
    fn hover(&mut self) {
        let empty = Tree::default();
        let (tree, p) = match self.pointer.and_then(|p| self.panel_at(p).map(|i| (i, p))) {
            Some((i, p)) => (self.panels[i].tree(), p),
            None => (&empty, Point::new(f32::NAN, f32::NAN)),
        };
        self.ui.pointer_move(tree, p);
    }

    /// Applies one host event; returns the command a release or chord ran.
    pub fn apply(&mut self, ev: HostEvent) -> Option<Outcome> {
        self.stats.events = self.stats.events.saturating_add(1);
        match ev {
            HostEvent::Resized { width, height } => {
                if width == 0 || height == 0 {
                    // Minimised: no layout, no paint, no pane (create_pane(0, 0) is InvalidExtent).
                    self.minimized = true;
                } else {
                    self.minimized = false;
                    self.resize(width, height);
                }
                None
            }
            HostEvent::ScaleFactor { milli } => {
                if !self.scale_locked {
                    self.set_scale(milli);
                }
                None
            }
            HostEvent::PointerMoved { x, y } => {
                self.pointer = self.logical(x, y);
                self.hover();
                None
            }
            HostEvent::PointerLeft => {
                self.pointer = None;
                self.wheel_rest = 0;
                self.hover();
                None
            }
            HostEvent::Button { button: Button::Left, pressed: true } => {
                if let Some(p) = self.pointer
                    && let Some(i) = self.panel_at(p)
                {
                    self.ui.pointer_down(self.panels[i].tree(), p);
                    self.press = Some(i);
                }
                None
            }
            HostEvent::Button { button: Button::Left, pressed: false } => {
                let i = self.press.take()?;
                let p = self.pointer.unwrap_or(Point::new(f32::NAN, f32::NAN));
                let id = self.ui.pointer_up(self.panels[i].tree(), p)?;
                self.activate(&id).ok().flatten()
            }
            HostEvent::Wheel { y, pixels, .. } => {
                if self.pointer.is_some_and(|p| self.regions.body.contains(p)) {
                    // Thousandths of a row, remainder kept (review F4): a notch (1000 milli-lines) is 3 rows, 32 px is
                    // one row, and many small high-resolution or touchpad deltas add up instead of each forcing a row.
                    let y = i64::from(y);
                    let milli_rows = if pixels { y.saturating_mul(1000) / size::ROW_LAYER_H as i64 } else { y.saturating_mul(3) };
                    let acc = self.wheel_rest.saturating_sub(milli_rows);
                    self.wheel_rest = acc % 1000;
                    self.scroll_by(acc / 1000);
                }
                None
            }
            HostEvent::Key { key, pressed: true, mods } => self.key(key, mods),
            HostEvent::Modifiers(m) => {
                self.mods = m;
                None
            }
            _ => None,
        }
    }

    fn key(&mut self, key: Key, mods: Mods) -> Option<Outcome> {
        let page = view::rows_page(self.regions.body.h()) as i64;
        match (key, mods.ctrl, mods.shift) {
            // components.json keyboard: PROVISIONAL chords.
            (Key::Char('z'), true, false) => Some(self.run(Cmd::Undo)),
            (Key::Char('z'), true, true) | (Key::Char('y'), true, false) => Some(self.run(Cmd::Redo)),
            (Key::PageDown, ..) => {
                self.scroll_by(page);
                None
            }
            (Key::PageUp, ..) => {
                self.scroll_by(-page);
                None
            }
            (Key::Home, ..) => {
                self.scroll_by(i64::MIN / 2);
                None
            }
            (Key::End, ..) => {
                self.scroll_by(i64::MAX / 2);
                None
            }
            _ => None,
        }
    }

    fn scroll_by(&mut self, rows: i64) {
        let max = i64::try_from(view::max_first(self.rows.len(), self.regions.body.h())).unwrap_or(i64::MAX);
        let first = i64::try_from(self.first_row).unwrap_or(i64::MAX).saturating_add(rows).clamp(0, max);
        let first = usize::try_from(first).unwrap_or(0);
        if first != self.first_row {
            self.first_row = first;
            self.bump();
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if (width, height) == self.extent {
            return;
        }
        if rgba_len(width, height).is_err() {
            self.note("extent_refused");
            return;
        }
        self.extent = (width, height);
        self.relayout();
    }

    fn set_scale(&mut self, milli: u32) {
        let (s, clamped) = scale_of(milli);
        if clamped {
            self.note("scale_clamped");
        }
        if s != self.scale {
            self.scale = s;
            // scale_rules.monitor_change: rasters at the old scale are dropped.
            self.text.clear_masks();
            self.relayout();
        }
    }

    /// The command table: author_id to a public Viewer method (the drive surface's own mappings). Ok(None): an inert
    /// control (the active tab).
    fn activate(&mut self, id: &str) -> Result<Option<Outcome>, &'static str> {
        let cmd = if id == LIST_ID {
            Cmd::Select(None)
        } else if id == TAB_ID {
            return Ok(None);
        } else if let Some(rest) = id.strip_prefix(ROW_PREFIX) {
            let (lid, eye) = match rest.strip_suffix(".visibility") {
                Some(l) => (l, true),
                None => (rest, false),
            };
            let row = self.rows.iter().find(|r| r.layer_id.len() == lid.len() && r.layer_id.eq_ignore_ascii_case(lid)).ok_or("unknown_layer")?;
            if eye { Cmd::SetVisible { layer: row.layer_id.clone(), visible: !row.visible } } else { Cmd::Select(Some(row.layer_id.clone())) }
        } else {
            return Err("unknown_author_id");
        };
        Ok(Some(self.run(cmd)))
    }

    fn run(&mut self, cmd: Cmd) -> Outcome {
        let (command, result) = match cmd {
            Cmd::SetVisible { layer, visible } => ("layer.set_visible", self.viewer.set_visible(&layer, visible).map(drop)),
            Cmd::Select(layer) => {
                self.viewer.select(layer);
                ("viewer.select", Ok(()))
            }
            Cmd::Undo => ("history.undo", self.viewer.undo().map(drop)),
            Cmd::Redo => ("history.redo", self.viewer.redo().map(drop)),
        };
        let code = match result {
            Ok(()) => "ok".to_owned(),
            // The run's own refusal code, never its text.
            Err(_) => self
                .viewer
                .run()
                .ops()
                .last()
                .and_then(|o| o["error"].as_str())
                .filter(|c| crate::document::code_like(c))
                .unwrap_or("refused")
                .to_owned(),
        };
        self.stats.commands = self.stats.commands.saturating_add(1);
        if code != "ok" {
            self.stats.refused = self.stats.refused.saturating_add(1);
        }
        self.refresh();
        let out = Outcome { command, code };
        self.last = Some(out.clone());
        out
    }

    /// Activates the control `author_id` as a release inside it would (STU-MDL-109: command API plus the tree by
    /// author_id, no OS input). `Err("unknown_author_id")` / `Err("unknown_layer")`: a typed refusal.
    pub fn invoke(&mut self, author_id: &str) -> Result<Option<Outcome>, &'static str> {
        self.activate(author_id)
    }
    /// One scripted step; an unknown author_id is `Usage("unknown_author_id")` (exit 2).
    pub fn step(&mut self, step: &Step) -> Result<Option<Outcome>, HostError> {
        match step {
            Step::Event(e) => Ok(self.apply(*e)),
            Step::Invoke(id) => self.activate(id).map_err(HostError::Usage),
        }
    }

    fn update(&mut self) -> Result<bool, HostError> {
        let (r, s, theme, rev) = (self.regions, self.scale, self.theme, self.state_rev);
        let command = match &self.last {
            None => "no command yet".to_owned(),
            Some(o) if o.code == "ok" => format!("{} ok", o.command),
            Some(o) => format!("{} refused {}", o.command, o.code),
        };
        let device = format!("{}; viewer render-{}", self.device, self.backend);
        let facts = view::ViewportFacts {
            file: &self.file_name,
            size: self.image.as_ref().map(|i| (i.width, i.height)),
            revision: self.revision,
            render_code: self.render_code,
        };
        let status = view::StatusFacts { device: &device, revision: self.revision, command: &command };
        let [vp, dock, bar] = &mut self.panels;
        let mut any = vp
            .update(rev, &self.ui, r.viewport, s, theme, &mut self.text, |b| view::viewport(b, &facts))
            .map_err(|e| HostError::Document(build_code(&e)))?;
        let (rows, selected, first, body_h) = (&self.rows, self.selected.as_deref(), self.first_row, r.body.h());
        any |= match dock.update(rev, &self.ui, r.dock, s, theme, &mut self.text, |b| view::layers_dock(b, rows, selected, first, body_h)) {
            Ok(b) => b,
            // A collision of lowercased ids (or any build refusal) is an empty state with its code, never a panic.
            Err(e) => {
                let code = build_code(&e);
                dock.update(rev, &self.ui, r.dock, s, theme, &mut self.text, |b| view::dock_refused(b, code)).map_err(|e| HostError::Document(build_code(&e)))?
            }
        };
        any |= bar
            .update(rev, &self.ui, r.status, s, theme, &mut self.text, |b| view::status(b, &status))
            .map_err(|e| HostError::Document(build_code(&e)))?;
        Ok(any)
    }

    /// Rebuilds what changed and paints; false on an idle frame (nothing rebuilt, painted or due for upload) and
    /// while minimised.
    pub fn frame(&mut self) -> Result<bool, HostError> {
        if self.minimized {
            return Ok(false);
        }
        let rebuilt = self.update()?;
        self.stats.builds = self.panels.iter().map(Panel::builds).sum();
        if !rebuilt && !self.repaint && self.canvas.is_some() {
            self.stats.idle_frames = self.stats.idle_frames.saturating_add(1);
            return Ok(false);
        }
        let (w, h) = self.extent;
        let mut canvas = Canvas::new(w, h, ColorToken::SurfaceFrame.resolve(self.theme)).ok_or(HostError::Window("canvas_extent"))?;
        let s = self.scale;
        let doc = self.image.as_ref().map(|i| view::document_list(self.regions.viewport, i.width, i.height, self.theme));
        let mut res = Res { icons: &mut self.icons, image: self.image.as_ref() };
        paint(&mut canvas, self.panels[VIEWPORT].display_list(), s, &mut self.text, &mut res);
        if let Some(list) = &doc {
            paint(&mut canvas, list, s, &mut self.text, &mut res);
        }
        paint(&mut canvas, self.panels[DOCK].display_list(), s, &mut self.text, &mut res);
        paint(&mut canvas, self.panels[STATUS].display_list(), s, &mut self.text, &mut res);
        self.canvas = Some(canvas);
        self.repaint = false;
        self.pending_upload = true;
        self.stats.paints = self.stats.paints.saturating_add(1);
        Ok(true)
    }

    /// Copies the painted frame into the sink when it changed since the last upload.
    pub fn upload(&mut self, sink: &mut dyn PaneSink) -> Result<bool, Refusal> {
        if !self.pending_upload || self.minimized {
            return Ok(false);
        }
        let Some(c) = &self.canvas else { return Ok(false) };
        sink.upload(c.rgba())?;
        self.pending_upload = false;
        self.stats.uploads = self.stats.uploads.saturating_add(1);
        Ok(true)
    }
    /// [`GuiFrame::upload`] into a client port's pane.
    pub fn upload_to<P: GpuClientPort>(&mut self, port: &mut P, pane: PaneId) -> Result<bool, Refusal> {
        self.upload(&mut PortPane { port, pane })
    }
    /// The next upload copies the frame even if it did not change (a re-created pane starts empty).
    pub fn mark_upload(&mut self) {
        self.pending_upload = self.canvas.is_some();
    }
    pub fn canvas(&self) -> Option<&Canvas> {
        self.canvas.as_ref()
    }
    pub fn rgba(&self) -> Option<&[u8]> {
        self.canvas.as_ref().map(Canvas::rgba)
    }

    /// One semantic tree whose roots are the region panels (Viewport, right dock, status bar).
    pub fn semantic(&self) -> Semantic {
        let mut out = Semantic { roots: Vec::new(), nodes: 0, truncated: false };
        for p in &self.panels {
            let s = semantic(p.tree(), MAX_TREE_NODES);
            out.nodes = out.nodes.saturating_add(s.nodes);
            out.truncated |= s.truncated;
            out.roots.extend(s.roots);
        }
        out
    }
    /// `kumihimo.ui.tree@1` of the frame (the model path until the AccessKit adapter exists).
    pub fn tree_json(&self) -> String {
        self.semantic().to_json()
    }
    /// The tree file: the tree with the document revision.
    pub fn tree_file(&self) -> String {
        format!("{{\"schema\":\"hsk.studio.native_host.tree@1\",\"revision\":{},\"tree\":{}}}", self.revision, self.tree_json())
    }
    fn rows_shown(&self) -> usize {
        let total = self.rows.len();
        view::rows_fit(self.regions.body.h()).min(total.saturating_sub(self.first_row))
    }
    /// The receipt's `gui` member: codes and counters only, never document text.
    pub fn receipt(&self) -> Json {
        let n = |v: u64| Json::Num(v);
        Json::obj(vec![
            ("revision", n(self.revision)),
            ("rows_shown", n(self.rows_shown() as u64)),
            ("rows_total", n(self.rows.len() as u64)),
            ("first_row", n(self.first_row as u64)),
            ("builds", n(self.stats.builds)),
            ("paints", n(self.stats.paints)),
            ("uploads", n(self.stats.uploads)),
            ("idle_frames", n(self.stats.idle_frames)),
            ("commands", n(self.stats.commands)),
            ("refused", n(self.stats.refused)),
            ("events", n(self.stats.events)),
            ("last_command", self.last.as_ref().map_or(Json::Null, |o| Json::obj(vec![("command", Json::str(o.command)), ("code", Json::str(&o.code))]))),
            ("text", Json::str(self.text_kind)),
            ("scale_milli", n(u64::from((self.scale.get() * 1000.0).round() as u32))),
            ("render", Json::str(self.render_code.unwrap_or("ok"))),
            ("notes", Json::strs(&self.notes)),
        ])
    }
    /// AC-TEXT-01 over the three panels.
    pub fn clipped(&mut self) -> Vec<Clipped> {
        let mut out = Vec::new();
        for p in &self.panels {
            out.extend(checks::clipped_labels(p.tree(), &mut self.text));
        }
        out
    }
}

/// Applies `steps` in order; after each, paints and uploads what changed. Stops at the first refusal of the sink:
/// no command runs after a lost device, and nothing retries.
pub fn drive(frame: &mut GuiFrame, sink: &mut dyn PaneSink, steps: &[Step]) -> Result<(), HostError> {
    frame.frame()?;
    frame.upload(sink)?;
    for s in steps {
        frame.step(s)?;
        frame.frame()?;
        frame.upload(sink)?;
    }
    Ok(())
}
