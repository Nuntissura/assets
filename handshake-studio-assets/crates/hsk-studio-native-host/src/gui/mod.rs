//! Feature `document`: the GUI frame. SMB-GUI-SPINE-1 built the spine (a real document in the Viewport, the Layers
//! panel, a status bar); SMB-GUI-FRAME-1 builds the complete static raster frame around it (build-order B5): the menu
//! bar, the document tab strip, the Context Bar, the Tool Rail, the collapsed Browse and Time & Results docks, the
//! Viewport with rulers, the three-group Inspect Dock and the status bar, at layouts.json sizes under the fit_table
//! ladder ([`frame::layout`]). One Interaction, one Text and one Panel per region (lead F7 ruling), plus one unpainted
//! panel with the observability nodes studio.dock.state and studio.tool.active. Chrome is the CPU painter (decision
//! O6), painted into one window-size canvas that the host copies into its pane: `cpu painter, Vulkan UI pass pending`.
//! [`GuiFrame::compose`] is the one place that lists what a frame draws, in paint order; every [`ChromeTarget`] draws
//! through it (SMB-VK-UI-PASS-1 hook).
//!
//! Input: typed [`HostEvent`]s in physical px, divided by the scale into logical px, routed to the panel under the
//! pointer; a release inside a control gives its author_id, and the command table answers every one: Layers controls
//! map to public Viewer methods (select, set_visible as the named undo step `panel_set visible <layer_id>`, undo,
//! redo), a rail slot picks its tool (`frame.tool_pick`), a group tab switches its group (`frame.tab_select`), and every
//! control this slice does not build is disabled with the reason `not_implemented_yet` and answers `frame.static` with
//! that code in the status bar and the receipt (build-order B6). An id outside the table is `unknown_author_id`, never
//! a silent drop. Frame-local picks are view state: no undo step, no Viewer call, no actor. Idle frames rebuild, paint
//! and upload nothing. No winit or Vulkan type is held here.
use crate::capture::Json;
use crate::error::HostError;
use crate::event::{Button, HostEvent, Key, Mods};
use crate::scene::DocumentFacts;
use crate::script::Step;
use hsk_studio_accord::ActorContext;
use hsk_studio_harness_view::view::{LayerRow, Viewer};
use hsk_studio_vulkan::{GpuClientPort, PaneId, Refusal, rgba_len};
use kumihimo_ui::checks::{self, Clipped};
use kumihimo_ui::display::{Border, DisplayList, IconId, ImageId, Prim};
use kumihimo_ui::fit;
use kumihimo_ui::geom::{Point, Rect, Rgba8, Scale};
use kumihimo_ui::icons::IconSet;
use kumihimo_ui::paint::{Canvas, IconMask, ImageRef, PaintStats, Resources, paint};
use kumihimo_ui::semantic::{Semantic, semantic};
use kumihimo_ui::text::{BoxText, MAX_LABEL_BYTES, Text, TextStats};
use kumihimo_ui::tokens::{ColorToken, State, StyleKind, TextToken, Theme, size, state_style};
use kumihimo_ui::tree::{BuildError, Interaction, Node, Panel, Tree};
use std::path::{Path, PathBuf};

pub mod chrome;
pub mod frame;
pub mod preset;
pub mod view;

/// STU-SHL-066: the raster centre is a `canvas` viewer (STU-SHL-085; lead ruling 2026-11-08 over the spine's
/// `studio.viewer.raster`).
pub const VIEWER_ID: &str = "studio.viewer.canvas";
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
/// Longest device readout, bytes (runtime input).
pub const MAX_DEVICE_BYTES: usize = 256;
/// Largest font file read for the UI text.
const MAX_FONT_BYTES: u64 = 32 << 20;
const INTER_REGULAR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
const INTER_BOLD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../hsk-studio-interop-fig/tests/fixtures/Inter-Bold.ttf");

/// The author_id of a layer's row (lowercased: the STU-MDL-100 grammar takes `[a-z0-9-]` only).
pub fn row_id(layer_id: &str) -> String {
    format!("{ROW_PREFIX}{}", layer_id.to_ascii_lowercase())
}

/// The spine's named regions in logical px, derived from the frame layout: the Viewport, the Inspect Dock, the Layers
/// group's header and body (empty while that group is collapsed), the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Regions {
    pub window: Rect,
    pub viewport: Rect,
    pub dock: Rect,
    pub header: Rect,
    pub body: Rect,
    pub status: Rect,
}

fn regions_of(l: &frame::FrameLayout) -> Regions {
    let g = l.groups.last().copied().unwrap_or_else(|| Rect::new(l.right.x0, l.right.y0, l.right.x0, l.right.y0));
    let collapsed = l.collapsed.last().copied().unwrap_or(true);
    let hy = (g.y0 + size::DOCK_GROUP_HEADER_H).min(g.y1);
    Regions {
        window: l.window,
        viewport: l.centre,
        dock: l.right,
        header: Rect::new(g.x0, g.y0, g.x1, hy),
        body: if collapsed { Rect::new(g.x0, hy, g.x1, hy) } else { Rect::new(g.x0, hy, g.x1, g.y1) },
        status: l.status,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuiStats {
    /// Panel rebuilds, summed over every panel.
    pub builds: u64,
    pub paints: u64,
    pub uploads: u64,
    /// Viewer commands run.
    pub commands: u64,
    pub refused: u64,
    pub events: u64,
    /// Frames that rebuilt and painted nothing.
    pub idle_frames: u64,
    /// `frame.static` outcomes (not_implemented_yet).
    pub statics: u64,
    /// `frame.tool_pick` and `frame.tab_select` outcomes.
    pub picks: u64,
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

/// One frame's chrome in paint order (the compose hook): every chrome target draws exactly these lists, back to
/// front, over the frame clear, at this scale, with this text and these resources.
pub struct Chrome<'a> {
    /// Physical px of the target.
    pub extent: (u32, u32),
    pub clear: Rgba8,
    pub scale: Scale,
    /// Back to front: menu bar, tab strip, Context Bar, Tool Rail, the colour proxy's chips, Browse rail, Viewport
    /// panel, document, Inspect Dock, Time & Results rail, status bar.
    pub lists: &'a [&'a DisplayList],
    pub text: &'a mut Text,
    pub res: &'a mut dyn Resources,
}

/// Where a composed frame is drawn: [`CpuChrome`] (the CPU painter: the default chrome and the reference) or the
/// Vulkan UI pass. A test fake returns a refusal such as DeviceLost, as a [`PaneSink`] does.
pub trait ChromeTarget {
    fn draw(&mut self, chrome: Chrome<'_>) -> Result<(), HostError>;
}

/// The CPU painter's target: one canvas of the extent, cleared to the frame clear, each list painted in order; `stats`
/// sums the painter's statistics over the lists (the receipt's missing_resources and glyph_refusals).
#[derive(Default)]
pub struct CpuChrome {
    pub canvas: Option<Canvas>,
    pub stats: PaintStats,
}

impl ChromeTarget for CpuChrome {
    fn draw(&mut self, chrome: Chrome<'_>) -> Result<(), HostError> {
        let Chrome { extent: (w, h), clear, scale, lists, text, res } = chrome;
        let mut canvas = Canvas::new(w, h, clear).ok_or(HostError::Window("canvas_extent"))?;
        let mut sum = PaintStats::default();
        for list in lists {
            let st = paint(&mut canvas, list, scale, text, res);
            sum.prims = sum.prims.saturating_add(st.prims);
            sum.glyphs = sum.glyphs.saturating_add(st.glyphs);
            sum.glyph_refusals = sum.glyph_refusals.saturating_add(st.glyph_refusals);
            sum.missing_resources = sum.missing_resources.saturating_add(st.missing_resources);
            sum.scissor_underflow = sum.scissor_underflow.saturating_add(st.scissor_underflow);
        }
        self.canvas = Some(canvas);
        self.stats = sum;
        Ok(())
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

/// The designer's icon set (B3 icon half) plus the document image.
struct Res<'a> {
    icons: &'a mut IconSet,
    image: Option<&'a DocImage>,
}
impl Resources for Res<'_> {
    fn icon(&mut self, icon: IconId, px: u32) -> Option<IconMask> {
        self.icons.icon(icon, px)
    }
    fn image(&self, image: ImageId) -> Option<ImageRef<'_>> {
        let img = self.image.filter(|_| image == DOC_IMAGE)?;
        Some(ImageRef { width: img.width, height: img.height, rgba: &img.rgba8 })
    }
}

// Panels in paint order, then the unpainted observability panel.
const MENU: usize = 0;
const TABS: usize = 1;
const CONTEXT: usize = 2;
const RAIL: usize = 3;
const BROWSE: usize = 4;
const VIEWPORT: usize = 5;
const INSPECT: usize = 6;
const TIME: usize = 7;
const STATUS: usize = 8;
const OBSERVE: usize = 9;
const PANELS: usize = 10;

pub struct GuiFrame {
    viewer: Viewer,
    file_name: String,
    text: Text,
    text_kind: &'static str,
    icons: IconSet,
    ui: Interaction,
    theme: Theme,
    extent: (u32, u32),
    scale: Scale,
    scale_locked: bool,
    edges: frame::Edges,
    layout: frame::FrameLayout,
    regions: Regions,
    panels: [Panel; PANELS],
    /// Per-panel revision: a frame-local pick rebuilds only the panels it changes.
    revs: [u64; PANELS],
    rows: Vec<LayerRow>,
    selected: Option<String>,
    revision: u64,
    unsaved: bool,
    brush: Option<[u8; 3]>,
    image: Option<DocImage>,
    backend: &'static str,
    render_code: Option<&'static str>,
    /// The active rail slot (index into [`preset::RAIL`]) and each group's active tab.
    tool: usize,
    tabs: [usize; 3],
    focus_group: usize,
    first_row: usize,
    /// Wheel travel below one row, thousandths of a row (cleared when the pointer leaves).
    wheel_rest: i64,
    pointer: Option<Point>,
    press: Option<usize>,
    /// A press on a disabled control: its release answers `not_implemented_yet` (B6), the Interaction never presses it.
    press_static: Option<String>,
    mods: Mods,
    minimized: bool,
    canvas: Option<Canvas>,
    repaint: bool,
    pending_upload: bool,
    stats: GuiStats,
    paint_stats: PaintStats,
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

/// `v` logical px rounded to 0.01, integers without a fraction (studio.dock.state numbers).
fn num(v: f32) -> String {
    let r = (f64::from(v) * 100.0).round() / 100.0;
    if r.fract() == 0.0 { format!("{}", r as i64) } else { format!("{r}") }
}
fn jstr(s: &str) -> String {
    Json::str(s).render()
}

/// Every fit decision the views need, made with Text before the build (the view closure has no Text).
struct Fits {
    menu: (f32, bool, usize),
    title: String,
    tab_w: f32,
    tab_name: String,
    groups: Vec<fit::TabFit>,
    device_w: f32,
}

impl GuiFrame {
    /// Opens `path` with the Viewer on render-cpu and lays the frame out at `width x height` physical px. `scale_milli`
    /// fixes the UI scale (window ScaleFactor events are then ignored); None starts at 1 and follows them.
    pub fn open(path: &Path, width: u32, height: u32, scale_milli: Option<u32>) -> Result<Self, HostError> {
        rgba_len(width, height).map_err(|_| HostError::Usage("size"))?;
        let viewer = crate::document::open_viewer(path)?;
        let (scale, clamped) = scale_milli.map_or((Scale::ONE, false), scale_of);
        let (text, text_kind) = ui_text(viewer.run().actor());
        let edges = preset::raster_edges();
        let layout = frame::layout((width, height), scale, &edges);
        let mut f = Self {
            viewer,
            file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            text,
            text_kind,
            icons: IconSet::default(),
            ui: Interaction::default(),
            theme: Theme::Dark,
            extent: (width, height),
            scale,
            scale_locked: scale_milli.is_some(),
            regions: regions_of(&layout),
            layout,
            edges,
            panels: Default::default(),
            revs: [0; PANELS],
            rows: Vec::new(),
            selected: None,
            revision: 0,
            unsaved: false,
            brush: None,
            image: None,
            backend: "none",
            render_code: None,
            tool: 0,
            tabs: [0; 3],
            focus_group: 0,
            first_row: 0,
            wheel_rest: 0,
            pointer: None,
            press: None,
            press_static: None,
            mods: Mods::default(),
            minimized: false,
            canvas: None,
            repaint: true,
            pending_upload: false,
            stats: GuiStats::default(),
            paint_stats: PaintStats::default(),
            last: None,
            device: "no device".into(),
            notes: Vec::new(),
        };
        if clamped {
            f.note("scale_clamped");
        }
        f.note_size();
        f.refresh();
        Ok(f)
    }

    fn note(&mut self, code: &'static str) {
        if !self.notes.contains(&code) && self.notes.len() < 32 {
            self.notes.push(code);
        }
    }
    fn note_size(&mut self) {
        if self.layout.below_min {
            self.note("below_min_size");
        }
    }

    /// Renders what changed and re-reads the document: rows, selection, revision, the undo list and the brush colour
    /// from one `state_json` (the selection has no getter; `Viewer::layers` runs once inside it).
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
        self.unsaved = st["edit"]["history"]["undo"].as_array().is_some_and(|a| !a.is_empty());
        self.brush = st["edit"]["brush"]["rgb8"].as_array().and_then(|a| {
            let c: Vec<u8> = a.iter().filter_map(|v| v.as_u64().and_then(|v| u8::try_from(v).ok())).collect();
            (c.len() == 3).then(|| [c[0], c[1], c[2]])
        });
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
        self.bump_all();
        self.repaint = true;
    }

    fn bump(&mut self, panels: &[usize]) {
        for &p in panels {
            self.revs[p] = self.revs[p].wrapping_add(1);
        }
    }
    fn bump_all(&mut self) {
        for r in &mut self.revs {
            *r = r.wrapping_add(1);
        }
    }

    fn relayout(&mut self) {
        self.layout = frame::layout(self.extent, self.scale, &self.edges);
        self.regions = regions_of(&self.layout);
        self.note_size();
        self.first_row = self.first_row.min(view::max_first(self.rows.len(), self.regions.body.h()));
        // The dock state, the fit decisions and the fit-dependent bands change with the layout.
        self.bump_all();
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
    /// The frame layout: every region rect and the fit decisions.
    pub fn layout(&self) -> &frame::FrameLayout {
        &self.layout
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
    /// Static outcomes (`frame.static`, not_implemented_yet) so far.
    pub fn statics(&self) -> u64 {
        self.stats.statics
    }
    /// The summed paint statistics of the last painted frame.
    pub fn paint_stats(&self) -> PaintStats {
        self.paint_stats
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
    /// The face tool id of the active rail slot (studio.tool.active, STU-MDL-116).
    pub fn tool_active(&self) -> &str {
        preset::RAIL[self.tool].face_tool
    }
    /// The active tab's panel id of group `group_id` (`studio.group.<first panel>`).
    pub fn active_tab(&self, group_id: &str) -> Option<&str> {
        preset::RIGHT.iter().zip(self.tabs).find(|(g, _)| chrome::group_id(g.tabs[0].0) == group_id).map(|(g, t)| g.tabs[t].0)
    }
    /// The name the frame shows for the document (the opened file's by default); hostile-name tests feed others.
    pub fn set_file_name(&mut self, name: &str) {
        self.file_name = name.to_owned();
        self.bump(&[MENU, TABS, VIEWPORT]);
    }
    /// Runs `f` on the viewer, then re-reads the document as after a command.
    pub fn edit(&mut self, f: impl FnOnce(&mut Viewer)) {
        f(&mut self.viewer);
        self.refresh();
    }
    /// Every panel, in paint order, the observability panel last.
    pub fn panels(&self) -> &[Panel] {
        &self.panels
    }
    /// The panel whose tree holds `author_id`.
    pub fn panel_of(&self, author_id: &str) -> Option<&Panel> {
        self.panels.iter().find(|p| p.tree().find(author_id).is_some())
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
    /// Where the document image sits, logical px: centred 1:1 in the canvas area under the rulers.
    pub fn image_rect(&self) -> Option<Rect> {
        self.image.as_ref().map(|i| view::image_rect(self.layout.canvas, i.width, i.height))
    }
    /// The device and backend readout of the status bar (AC-BEHAV-07; the host passes the Owner's DeviceInfo), cut at
    /// [`MAX_DEVICE_BYTES`].
    pub fn set_device(&mut self, readout: String) {
        let readout = cut(&readout, MAX_DEVICE_BYTES).to_owned();
        if self.device != readout {
            self.device = readout;
            self.bump(&[STATUS]);
        }
    }
    /// Document facts for the receipt.
    pub fn facts(&self) -> DocumentFacts {
        DocumentFacts { file_name: self.file_name.clone(), revision: self.revision, backend: self.backend.to_string(), layers: self.rows.len() as u64 }
    }

    /// Physical pixel to the logical point the hit test uses. The painter gives pixel `p` to the node whose snapped
    /// edges hold it, `round(y0 * s) <= p < round(y1 * s)`, which is `y0 * s < p + 0.5 <= y1 * s`; the hit test is
    /// `y0 <= q < y1`. Taking `q` just under the pixel centre, `(p + 0.49) / s`, makes them agree at every edge whose
    /// physical fraction is outside (0.49, 0.5) (review F5).
    fn logical(&self, x: i32, y: i32) -> Option<Point> {
        let s = self.scale.get();
        let p = Point::new((x as f32 + 0.49) / s, (y as f32 + 0.49) / s);
        self.regions.window.contains(p).then_some(p)
    }
    fn panel_at(&self, p: Point) -> Option<usize> {
        let l = &self.layout;
        [(MENU, l.menu_bar), (TABS, l.tab_strip), (CONTEXT, l.context_bar), (RAIL, l.tool_rail), (BROWSE, l.left), (VIEWPORT, l.centre), (INSPECT, l.right), (TIME, l.bottom), (STATUS, l.status)]
            .into_iter()
            .find(|(_, q)| q.contains(p))
            .map(|(i, _)| i)
    }
    fn hover(&mut self) {
        let empty = Tree::default();
        let (tree, p) = match self.pointer.and_then(|p| self.panel_at(p).map(|i| (i, p))) {
            Some((i, p)) => (self.panels[i].tree(), p),
            None => (&empty, Point::new(f32::NAN, f32::NAN)),
        };
        self.ui.pointer_move(tree, p);
    }
    /// The author_id of the disabled control under `p` in panel `i`, if any.
    fn disabled_at(&self, i: usize, p: Point) -> Option<String> {
        let t = self.panels[i].tree();
        kumihimo_ui::hit::hit_test(t, p).filter(|&n| t.effectively_disabled(n)).map(|n| t.get(n).author_id.clone())
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
                    self.press_static = self.disabled_at(i, p);
                    self.ui.pointer_down(self.panels[i].tree(), p);
                    self.press = Some(i);
                }
                None
            }
            HostEvent::Button { button: Button::Left, pressed: false } => {
                let i = self.press.take()?;
                let pressed_static = self.press_static.take();
                let p = self.pointer.unwrap_or(Point::new(f32::NAN, f32::NAN));
                let id = match self.ui.pointer_up(self.panels[i].tree(), p) {
                    Some(id) => id,
                    // A disabled control is never pressed (STU-SHL-101); its release inside it still answers (B6).
                    None => pressed_static.filter(|s| self.disabled_at(i, p).as_deref() == Some(s.as_str()))?,
                };
                match self.activate(&id) {
                    Ok(o) => o,
                    // Never a silent drop: the code reaches the status bar and the receipt.
                    Err(code) => Some(self.frame_outcome("frame.pointer", code, &[STATUS])),
                }
            }
            HostEvent::Wheel { y, pixels, .. } => {
                if self.layers_active() && self.pointer.is_some_and(|p| self.regions.body.contains(p)) {
                    // Thousandths of a row, remainder kept (review F4).
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

    fn layers_active(&self) -> bool {
        let g = preset::RIGHT.len() - 1;
        preset::RIGHT[g].tabs.get(self.tabs[g]).is_some_and(|t| t.0 == "layers") && !self.layout.collapsed.last().copied().unwrap_or(true)
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
            self.bump(&[INSPECT]);
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

    /// A frame-local outcome: recorded, shown in the status bar, the panels in `panels` rebuilt.
    fn frame_outcome(&mut self, command: &'static str, code: &str, panels: &[usize]) -> Outcome {
        let out = Outcome { command, code: code.to_owned() };
        self.last = Some(out.clone());
        self.bump(panels);
        self.bump(&[STATUS]);
        out
    }

    /// The command table: every interactive author_id of the frame maps to one entry. Ok(None): an inert control (the
    /// active group tab).
    fn activate(&mut self, id: &str) -> Result<Option<Outcome>, &'static str> {
        if let Some(i) = preset::RAIL.iter().position(|g| preset::slot_id(g.group) == id) {
            self.tool = i;
            self.stats.picks = self.stats.picks.saturating_add(1);
            return Ok(Some(self.frame_outcome("frame.tool_pick", "ok", &[RAIL, CONTEXT, OBSERVE])));
        }
        for (gi, g) in preset::RIGHT.iter().enumerate() {
            if let Some(t) = g.tabs.iter().position(|(p, _)| chrome::tab_id(p) == id) {
                if t == self.tabs[gi] {
                    return Ok(None);
                }
                self.tabs[gi] = t;
                self.focus_group = gi;
                self.stats.picks = self.stats.picks.saturating_add(1);
                return Ok(Some(self.frame_outcome("frame.tab_select", "ok", &[INSPECT, OBSERVE])));
            }
        }
        let cmd = if id == LIST_ID {
            Cmd::Select(None)
        } else if let Some(rest) = id.strip_prefix(ROW_PREFIX) {
            let (lid, eye) = match rest.strip_suffix(".visibility") {
                Some(l) => (l, true),
                None => (rest, false),
            };
            let row = self.rows.iter().find(|r| r.layer_id.len() == lid.len() && r.layer_id.eq_ignore_ascii_case(lid)).ok_or("unknown_layer")?;
            if eye { Cmd::SetVisible { layer: row.layer_id.clone(), visible: !row.visible } } else { Cmd::Select(Some(row.layer_id.clone())) }
        } else if self.node(id).is_some_and(|n| n.flags.disabled && n.value.as_deref().is_some_and(|v| v.starts_with(chrome::NIY))) {
            // B6: disabled with a reason; no Viewer call, no focus move, no popup.
            self.stats.statics = self.stats.statics.saturating_add(1);
            return Ok(Some(self.frame_outcome("frame.static", chrome::NIY, &[])));
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

    /// studio.dock.state (STU-MDL-106): fit_table's own keys in its order, then the groups and the rail panels;
    /// logical sizes rounded to 0.01.
    pub fn dock_state(&self) -> String {
        let l = &self.layout;
        let groups: Vec<String> = preset::RIGHT
            .iter()
            .zip(self.tabs)
            .enumerate()
            .map(|(i, (g, t))| {
                let tabs: Vec<String> = g.tabs.iter().map(|(p, _)| jstr(p)).collect();
                format!(
                    "{{\"id\":{},\"tabs\":[{}],\"active\":{},\"collapsed\":{}}}",
                    jstr(&chrome::group_id(g.tabs[0].0)),
                    tabs.join(","),
                    jstr(g.tabs[t].0),
                    l.collapsed.get(i).copied().unwrap_or(false)
                )
            })
            .collect();
        let left: Vec<String> = preset::LEFT.iter().map(|p| jstr(p.id)).collect();
        let bottom: Vec<String> = preset::BOTTOM.iter().map(|p| jstr(p.id)).collect();
        let applied: Vec<String> = l.applied.iter().map(|a| jstr(a)).collect();
        let heights: Vec<String> = l.groups.iter().map(|g| num(g.h())).collect();
        format!(
            "{{\"window\":[{},{}],\"scale\":{},\"logical\":[{},{}],\"left\":{},\"left_state\":\"{}\",\"right\":{},\"right_state\":\"{}\",\"meter\":{},\"bottom\":{},\"bottom_state\":\"{}\",\"centre\":[{},{}],\"right_group_heights\":[{}],\"rail_pitch\":{},\"rail_overflow_slots\":{},\"applied\":[{}],\"groups\":[{}],\"rail_panels\":{{\"left\":[{}],\"bottom\":[{}]}}}}",
            l.extent.0,
            l.extent.1,
            num(l.scale.get()),
            num(l.logical.0),
            num(l.logical.1),
            num(l.left.w()),
            l.left_state.word(),
            num(l.right.w()),
            l.right_state.word(),
            num(l.meter.w()),
            num(l.bottom.h()),
            l.bottom_state.word(),
            num(l.centre.w()),
            num(l.centre.h()),
            heights.join(","),
            num(l.rail_pitch),
            l.rail_hidden,
            applied.join(","),
            groups.join(","),
            left.join(","),
            bottom.join(",")
        )
    }

    /// The fit decisions of this frame, measured with the frame's Text.
    fn fits(&mut self) -> Fits {
        let size_text = self.size_text();
        let l = &self.layout;
        let t = &mut self.text;
        // FIT-M1: padding 8, then 5, then menu_abbreviations, then the order-preserving chevron.
        let presence = 4.0 + kumihimo_ui::widgets::frame::PRESENCE_D + 8.0;
        let avail = (l.menu_bar.w() - kumihimo_ui::widgets::frame::MODULE_SWITCH_W - presence).max(0.0);
        let chip = kumihimo_ui::widgets::frame::GROUP_MENU;
        let full = fit::fit_labels(t, &preset::MENU, TextToken::MenuTitle, 0, &[size::MENU_TITLE_PAD_X, kumihimo_ui::widgets::frame::MENU_TITLE_PAD_TIGHT], avail, chip);
        let (menu, used) = if !full.overflow() {
            ((full.pad, false, preset::MENU.len()), full.used)
        } else {
            let short: Vec<&str> = preset::MENU.iter().map(|m| preset::abbreviation(m)).collect();
            let s = fit::fit_labels(t, &short, TextToken::MenuTitle, 0, &[kumihimo_ui::widgets::frame::MENU_TITLE_PAD_TIGHT], avail, chip);
            ((s.pad, true, s.shown), s.used)
        };
        let title_full = format!("{}: Studio", self.file_name);
        let title_w = (avail - used).max(0.0);
        let title = t.ellipsize_middle(&title_full, TextToken::Body, 0, title_w);
        // FIT-T1: the document tab is 220 wide until the strip is short, then shrinks to 96.
        let caption = t.measure("Layout", TextToken::Body, 0).ceil();
        let preset_w = t.measure(preset::LAYOUT_PRESET, TextToken::Body, 0).ceil() + 8.0 + 4.0 + 4.0 + kumihimo_ui::widgets::frame::ICON_MICRO;
        let room = l.tab_strip.w() - chrome::TAB_NEW_W - chrome::tab_strip_cluster_w(caption, preset_w);
        let tab_w = room.clamp(size::TAB_DOCUMENT_MIN_W, size::TAB_DOCUMENT_MAX_W);
        let active_weight = state_style(StyleKind::Tab, State::Active).map_or(0, |s| s.weight);
        let dot = if self.unsaved { kumihimo_ui::widgets::frame::DOC_TAB_DOT + 6.0 } else { 0.0 };
        let name_w = (tab_w - widgets_pad_doc_tab() - kumihimo_ui::widgets::frame::DOC_TAB_CLOSE - 6.0 - dot).max(0.0);
        let tab_name = t.ellipsize_middle(&self.file_name, TextToken::Tab, active_weight, name_w);
        // docking.tab_overflow: +N measured with the frame's font.
        let plus = t.measure("+9", TextToken::Caption, 0).ceil() + 1.0 + 10.0 + 8.0;
        let tabs_avail = (l.right.w() - kumihimo_ui::widgets::frame::GROUP_DISCLOSURE - kumihimo_ui::widgets::frame::GROUP_MENU).max(0.0);
        let groups = preset::RIGHT
            .iter()
            .zip(self.tabs)
            .map(|(g, a)| {
                let labels: Vec<&str> = g.tabs.iter().map(|(_, n)| *n).collect();
                fit::fit_tabs(t, &labels, a, TextToken::Tab, 0, active_weight, size::TAB_PAD_X, tabs_avail, plus)
            })
            .collect();
        // The status bar: the last readout (device) takes what the others leave.
        let device = format!("{}; viewer render-{}", self.device, self.backend);
        let device = cut(&device, MAX_DEVICE_BYTES).to_owned();
        let revision = format!("revision {}", self.revision);
        let others: f32 = [crate::scene::CHROME, "100%", size_text.as_str(), revision.as_str()].iter().map(|s| t.measure(s, TextToken::Body, 0).ceil()).sum();
        let fixed = 16.0 + 5.0 * kumihimo_ui::widgets::frame::STATUS_GAP + 3.0 * (1.0 + kumihimo_ui::widgets::frame::STATUS_GAP);
        let device_w = t.measure(&device, TextToken::Body, 0).ceil().min((l.status.w() - fixed - others).max(0.0));
        Fits { menu, title, tab_w, tab_name, groups, device_w }
    }

    fn size_text(&self) -> String {
        self.image.as_ref().map_or_else(|| "no frame".to_owned(), |i| format!("{} x {} px", i.width, i.height))
    }

    fn update(&mut self) -> Result<bool, HostError> {
        let fits = self.fits();
        let (s, theme) = (self.scale, self.theme);
        let l = self.layout.clone();
        let revs = self.revs;
        let command = match &self.last {
            None => "no command yet".to_owned(),
            Some(o) if o.code == "ok" => format!("{} ok", o.command),
            Some(o) if o.command.starts_with("frame.") => format!("{} {}", o.command, o.code),
            Some(o) => format!("{} refused {}", o.command, o.code),
        };
        let device = cut(&format!("{}; viewer render-{}", self.device, self.backend), MAX_DEVICE_BYTES).to_owned();
        let title_full = format!("{}: Studio", self.file_name);
        let size_text = self.size_text();
        let origin = self.image_rect().map_or((l.canvas.x0, l.canvas.y0), |r| (r.x0, r.y0));
        let face = &preset::RAIL[self.tool];
        let selection = self.selected.as_deref().and_then(|id| self.rows.iter().find(|r| r.layer_id == id)).map(|r| format!("{} layer", r.kind));
        let dock_state = self.dock_state();
        let tool = self.tool_active().to_owned();
        let (rows, selected, first, body_h) = (&self.rows, self.selected.as_deref(), self.first_row, self.regions.body.h());
        let shown: Vec<Vec<usize>> = fits.groups.iter().map(|g| g.shown.clone()).collect();
        let groups: Vec<chrome::GroupFacts<'_>> = preset::RIGHT
            .iter()
            .enumerate()
            .map(|(i, g)| chrome::GroupFacts {
                tabs: g.tabs,
                active: self.tabs[i],
                shown: &shown[i],
                collapsed: l.collapsed.get(i).copied().unwrap_or(true),
                focused: i == self.focus_group,
                h: l.groups.get(i).map_or(0.0, |r| r.h()),
            })
            .collect();
        let menu = chrome::MenuFacts { pad: fits.menu.0, abbreviated: fits.menu.1, shown: fits.menu.2, title: &fits.title, title_full: &title_full };
        let tab = chrome::TabFacts { full: &self.file_name, shown: &fits.tab_name, w: fits.tab_w, unsaved: self.unsaved };
        let ctx = chrome::ContextFacts { tool_name: face.name, tool_icon: chrome::tool_icon(face.icon), selection: selection.as_deref() };
        let rail = chrome::RailFacts { pitch: l.rail_pitch, shown: l.rail_shown, active: self.tool };
        let vp = chrome::ViewportFacts { file: &self.file_name, size: self.image.as_ref().map(|i| (i.width, i.height)), revision: self.revision, render_code: self.render_code, origin };
        let layers = chrome::LayersFacts { rows, selected, first, body_h };
        let status = chrome::StatusFacts { command: &command, zoom: "100%", size: &size_text, revision: self.revision, device: &device, device_w: fits.device_w };
        let text = &mut self.text;
        let ui = &self.ui;
        let p = &mut self.panels;
        let err = |e: BuildError| HostError::Document(build_code(&e));
        let mut any = false;
        any |= p[MENU].update(revs[MENU], ui, l.menu_bar, s, theme, text, |b| chrome::menu_bar(b, &menu)).map_err(err)?;
        any |= p[TABS].update(revs[TABS], ui, l.tab_strip, s, theme, text, |b| chrome::tab_strip(b, &tab)).map_err(err)?;
        any |= p[CONTEXT].update(revs[CONTEXT], ui, l.context_bar, s, theme, text, |b| chrome::context_bar(b, &ctx)).map_err(err)?;
        any |= p[RAIL].update(revs[RAIL], ui, l.tool_rail, s, theme, text, |b| chrome::tool_rail(b, &rail)).map_err(err)?;
        any |= p[BROWSE].update(revs[BROWSE], ui, l.left, s, theme, text, chrome::browse_rail).map_err(err)?;
        any |= p[VIEWPORT].update(revs[VIEWPORT], ui, l.centre, s, theme, text, |b| chrome::viewport(b, &vp)).map_err(err)?;
        let inspect_rail = l.right_state == frame::EdgeState::Rail;
        any |= match p[INSPECT].update(revs[INSPECT], ui, l.right, s, theme, text, |b| if inspect_rail { chrome::inspect_rail(b) } else { chrome::inspect_dock(b, &groups, &layers) }) {
            Ok(b) => b,
            // A collision of lowercased ids (or any build refusal) is an empty state with its code, never a panic.
            Err(e) => {
                let code = build_code(&e);
                p[INSPECT].update(revs[INSPECT], ui, l.right, s, theme, text, |b| view::dock_refused(b, code)).map_err(err)?
            }
        };
        any |= p[TIME].update(revs[TIME], ui, l.bottom, s, theme, text, chrome::time_rail).map_err(err)?;
        any |= p[STATUS].update(revs[STATUS], ui, l.status, s, theme, text, |b| chrome::status(b, &status)).map_err(err)?;
        any |= p[OBSERVE].update(revs[OBSERVE], ui, Rect::default(), s, theme, text, |b| chrome::observe(b, &dock_state, &tool)).map_err(err)?;
        Ok(any)
    }

    /// The colour proxy's chips over the rail (components.json `colour_proxy`: box 36, chips 22): the stroke chip
    /// (empty) behind, the fill chip with the brush colour in front. The fill is document colour, so it is drawn here
    /// rather than as a token paint.
    fn proxy_list(&self) -> DisplayList {
        let Some(n) = self.node("studio.colour.proxy") else { return DisplayList::default() };
        let b = n.bounds;
        let chip = 22.0;
        let (seam, control) = (ColorToken::EdgeSeam.resolve(self.theme), ColorToken::EdgeControl.resolve(self.theme));
        let mut items = Vec::new();
        for (dx, fill) in [(11.0, None), (3.0, self.brush)] {
            let r = Rect::xywh(b.x0 + dx, b.y0 + dx, chip, chip);
            let outer = Rect::new(r.x0 - 1.0, r.y0 - 1.0, r.x1 + 1.0, r.y1 + 1.0);
            let fill = fill.map_or(Rgba8::TRANSPARENT, |[r8, g8, b8]| Rgba8 { r: r8, g: g8, b: b8, a: u8::MAX });
            items.push(Prim::RRect { rect: outer, radius: 3.0, fill: Rgba8::TRANSPARENT, border: Some(Border { width: 1.0, color: seam }) });
            items.push(Prim::RRect { rect: r, radius: 2.0, fill, border: Some(Border { width: 1.0, color: control }) });
        }
        DisplayList { items }
    }

    /// The compose hook: the frame's ordered lists with its clear colour, scale, text and resources, drawn by `target`.
    /// Every chrome target goes through here, so all of them draw the same frame.
    pub fn compose(&mut self, target: &mut dyn ChromeTarget) -> Result<(), HostError> {
        let doc = self.image.as_ref().map(|i| view::document_list(self.layout.canvas, i.width, i.height, self.theme));
        let proxy = self.proxy_list();
        let p = &self.panels;
        let mut lists: Vec<&DisplayList> = Vec::with_capacity(PANELS + 2);
        lists.extend([MENU, TABS, CONTEXT, RAIL].map(|i| p[i].display_list()));
        lists.push(&proxy);
        lists.extend([BROWSE, VIEWPORT].map(|i| p[i].display_list()));
        lists.extend(doc.as_ref());
        lists.extend([INSPECT, TIME, STATUS].map(|i| p[i].display_list()));
        let mut res = Res { icons: &mut self.icons, image: self.image.as_ref() };
        target.draw(Chrome {
            extent: self.extent,
            clear: ColorToken::SurfaceFrame.resolve(self.theme),
            scale: self.scale,
            lists: &lists,
            text: &mut self.text,
            res: &mut res,
        })
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
        let mut cpu = CpuChrome::default();
        self.compose(&mut cpu)?;
        self.paint_stats = cpu.stats;
        self.canvas = cpu.canvas;
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

    /// One semantic tree whose roots are every panel's roots, in paint order, the observability nodes last.
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
        let l = &self.layout;
        let ps = self.paint_stats;
        let tabs: Vec<&str> = preset::RIGHT.iter().zip(self.tabs).map(|(g, t)| g.tabs[t].0).collect();
        let frame = Json::obj(vec![
            ("preset", Json::str(preset::PRESET)),
            ("applied", Json::strs(&l.applied)),
            ("below_min_size", Json::Bool(l.below_min)),
            ("tool_active", Json::str(self.tool_active())),
            ("active_tabs", Json::strs(&tabs)),
            ("static_outcomes", n(self.stats.statics)),
            ("picks", n(self.stats.picks)),
            ("rail_shown", n(l.rail_shown as u64)),
            ("rail_hidden", n(l.rail_hidden as u64)),
            (
                "paint",
                Json::obj(vec![("prims", n(ps.prims)), ("glyphs", n(ps.glyphs)), ("glyph_refusals", n(ps.glyph_refusals)), ("missing_resources", n(ps.missing_resources))]),
            ),
        ]);
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
            ("frame", frame),
        ])
    }
    /// AC-TEXT-01 over every panel.
    pub fn clipped(&mut self) -> Vec<Clipped> {
        let mut out = Vec::new();
        for p in &self.panels {
            out.extend(checks::clipped_labels(p.tree(), &mut self.text));
        }
        out
    }
}

/// The document tab's horizontal padding (components.json document_tab pad_l 12, pad_r 6).
fn widgets_pad_doc_tab() -> f32 {
    kumihimo_ui::widgets::component::DOC_TAB_PAD_L + kumihimo_ui::widgets::component::DOC_TAB_PAD_R
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
