//! Feature `document`: the GUI frame. SMB-GUI-SPINE-1 built the spine (a real document in the Viewport, the Layers
//! panel, a status bar); SMB-GUI-FRAME-1 builds the complete static raster frame around it (build-order B5): the menu
//! bar, the document tab strip, the Context Bar, the Tool Rail, the collapsed Browse and Time & Results docks, the
//! Viewport with rulers, the three-group Inspect Dock and the status bar, at layouts.json sizes under the fit_table
//! ladder ([`frame::layout`]). One Interaction, one Text and one Panel per region (lead F7 ruling), plus one unpainted
//! panel with the observability nodes studio.dock.state and studio.tool.active. [`GuiFrame::compose`] is the one place
//! that lists what a frame draws, in paint order; every [`ChromeTarget`] draws through it (SMB-VK-UI-PASS-1). The
//! default chrome is the CPU painter (decision O6, `cpu painter`), painted into one window-size canvas the host copies
//! into its pane; `--chrome vulkan` draws the same lists with hsk-studio-vulkan's UI pass into the pane
//! ([`VulkanChrome`], `vulkan ui pass`) and keeps no canvas.
//!
//! Input: typed [`HostEvent`]s in physical px, divided by the scale into logical px, routed to the panel under the
//! pointer; a release inside a control gives its author_id, and the command table answers every one: Layers controls
//! map to public Viewer methods (select, set_visible as the named undo step `panel_set visible <layer_id>`, undo,
//! redo), a rail slot picks its tool (`frame.tool_pick`), a group tab switches its group (`frame.tab_select`), and every
//! control this slice does not build is disabled with the reason `not_implemented_yet` and answers `frame.static` with
//! that code in the status bar and the receipt (build-order B6). An id outside the table is `unknown_author_id`, never
//! a silent drop. Frame-local picks are view state: no undo step, no Viewer call, no actor. Idle frames rebuild, paint
//! and upload nothing. No winit or Vulkan type is held here.
//!
//! Menus (SMB-GUI-MENU-1, build-order B6): the nineteen titles open popups of the designed tree from ONE descriptor
//! source (harness-view `menu_tree`), evaluated once per context change ([`GuiFrame::menu_bar`], STU-SHL-046), never per
//! frame. Every node is drawn at every level: available rows enabled, greyed rows disabled with their reason line under
//! the label and the availability record as node properties (STU-SHL-018, STU-SHL-019); an INAPPLICABLE_HERE row is
//! followed by its enabled remedy row (STU-SHL-020). A greyed row answers `frame.menu_unavailable` with its reason code
//! and runs nothing; an enabled leaf runs its command through the menu tree (the Viewer's run context, its own undo
//! step) or, for WORKSPACE > Panels, picks the tab here; a submenu opens its cascade. The chevron lists the titles it
//! hides; a right click on a Layers row opens the Layers context menu, whose rows reference menu-bar leaves and run the
//! identical commands (STU-SHL-014). Tooltips L0 and L1 (STU-SHL-230 to 232) read the same record; they appear on a
//! hover dwell only when the host feeds its clock ([`GuiFrame::advance`]; no script step does, STU-SHL-235 rule 5) or
//! through the explicit accessibility request [`GuiFrame::show_tip`]. Menu outcomes are codes and counters in the
//! receipt's `frame.menu` member.
use crate::capture::Json;
use crate::error::HostError;
use crate::event::{Button, HostEvent, Key, Mods};
use crate::scene::DocumentFacts;
use crate::script::Step;
use hsk_studio_accord::ActorContext;
use hsk_studio_harness_view::view::menu_tree::descriptors::{Availability, AvailabilityState, MenuBar, MenuNode, MenuNodeKind};
use hsk_studio_harness_view::view::menu_tree::{self, HostLeaf, MenuState, Profile};
use hsk_studio_harness_view::view::{LayerRow, Viewer};
use crate::scene::ChromeMode;
use hsk_studio_vulkan::ui::TraceRecord;
use hsk_studio_vulkan::{GpuClientPort, Owner, PaneId, Refusal, UiFrame, UiPass, rgba_len};
use kumihimo_ui::checks::{self, Clipped};
use kumihimo_ui::display::{Border, DisplayList, IconId, ImageId, Prim};
use kumihimo_ui::fit;
use kumihimo_ui::geom::{Point, Rect, Rgba8, Scale};
use kumihimo_ui::icons::IconSet;
use kumihimo_ui::paint::{Canvas, IconMask, ImageRef, PaintStats, Resources, paint};
use kumihimo_ui::semantic::{Semantic, semantic};
use kumihimo_ui::text::{BoxText, MAX_LABEL_BYTES, Text, TextStats};
use kumihimo_ui::tokens::{ColorToken, State, StyleKind, TextToken, Theme, size, state_style};
use kumihimo_ui::tree::{BuildError, Interaction, Node, NodeAvailability, Panel, Tree};
use kumihimo_ui::text::ruler_label_value;
use std::collections::HashMap;
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
    // Every group but the top one leads with 1 px under its seam (D-F11).
    let top = (g.y0 + if l.groups.len() > 1 { 1.0 } else { 0.0 }).min(g.y1);
    let hy = (top + size::DOCK_GROUP_HEADER_H).min(g.y1);
    Regions {
        window: l.window,
        viewport: l.centre,
        dock: l.right,
        header: Rect::new(g.x0, top, g.x1, hy),
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

/// The Vulkan UI pass as a chrome target: draws into `pane` on `owner`'s device (item 7).
pub struct VulkanChrome<'a> {
    pub pass: &'a mut UiPass,
    pub owner: &'a mut Owner,
    pub pane: PaneId,
}

impl ChromeTarget for VulkanChrome<'_> {
    fn draw(&mut self, chrome: Chrome<'_>) -> Result<(), HostError> {
        let Chrome { clear, scale, lists, text, res, .. } = chrome;
        self.pass.draw(self.owner, self.pane, UiFrame { clear, lists, scale, text, res }).map_err(HostError::Vulkan)
    }
}

/// Max channel difference, differing pixels and the max difference outside the AA rects of `trace` (every pixel
/// outside the rects of the AA, glyph, icon, ramp and translucent classes must be exact).
pub fn parity(want: &[u8], got: &[u8], width: u32, trace: &[TraceRecord]) -> (u8, u64, u8) {
    let w = width as usize;
    if want.len() != got.len() || w == 0 || !want.len().is_multiple_of(4 * w) {
        return (u8::MAX, u64::MAX, u8::MAX);
    }
    let h = want.len() / 4 / w;
    let mut aa = vec![false; w * h];
    for r in trace.iter().filter(|r| !matches!(r.class, "flat" | "image" | "scissor" | "skipped")) {
        let (x0, x1) = (r.rect.x0.clamp(0, w as i32) as usize, r.rect.x1.clamp(0, w as i32) as usize);
        let (y0, y1) = (r.rect.y0.clamp(0, h as i32) as usize, r.rect.y1.clamp(0, h as i32) as usize);
        for y in y0..y1 {
            aa[y * w + x0..y * w + x1.max(x0)].fill(true);
        }
    }
    let (mut max, mut n, mut outside) = (0u8, 0u64, 0u8);
    for (i, (p, q)) in want.chunks_exact(4).zip(got.chunks_exact(4)).enumerate() {
        let d = p.iter().zip(q).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        if d > 0 {
            n += 1;
            max = max.max(d);
            if !aa[i] {
                outside = outside.max(d);
            }
        }
    }
    (max, n, outside)
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
    /// Rotated ruler labels, rasterised with the frame's Text before the draw (the target holds the Text).
    labels: &'a HashMap<(IconId, u32), IconMask>,
}
impl Resources for Res<'_> {
    fn icon(&mut self, icon: IconId, px: u32) -> Option<IconMask> {
        if ruler_label_value(icon).is_some() {
            return self.labels.get(&(icon, px)).cloned();
        }
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
/// B6: the open popups (menus, cascades, the context menu) and the tooltip, over every band.
const POPUP: usize = 9;
const TIP: usize = 10;
const OBSERVE: usize = 11;
const PANELS: usize = 12;

/// Open-path roots that are not titles: the chevron's list of hidden titles and the Layers context menu.
pub const MORE_MENU: &str = "more";
pub const LAYERS_CONTEXT: &str = "ctx.layers";
/// The WORKSPACE > Panels expansion node the frame grafts its panels into (STU-SHL-036).
pub const WORKSPACE_PANELS: &str = "studio.workspace.panel";
/// Tooltip dwell (components.json tooltip_l0 400 ms, STU-SHL-230 L1 ~900 ms) and the quiet time after a press
/// (STU-SHL-235 rule 1).
pub const L0_DWELL_MS: u64 = 400;
pub const L1_DWELL_MS: u64 = 900;
pub const TIP_QUIET_MS: u64 = 500;

/// The menu leaf id of a panel (WORKSPACE > Panels > <panel>).
pub fn panel_leaf_id(panel: &str) -> String {
    format!("{WORKSPACE_PANELS}.{}", panel.replace('-', "_"))
}

/// Menu counters for the receipt (codes and counters only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuStats {
    /// Popups opened (titles, cascades, the chevron list, the context menu).
    pub opens: u64,
    /// Leaves that ran (commands and host leaves).
    pub runs: u64,
    /// Greyed rows that answered their reason.
    pub unavailable: u64,
    /// Remedy rows (and tooltip remedy buttons) used.
    pub remedies: u64,
    /// Leaves whose command refused.
    pub refused: u64,
    /// Availability evaluations (one per context change).
    pub evaluations: u64,
    /// Evaluations that grew the profile (STU-SHL-048).
    pub profile_growth: u64,
    /// Tooltips shown.
    pub tips: u64,
}

/// A tooltip as shown (STU-SHL-231, STU-SHL-232), from the element's own record: L0 name and chord; L1 also the
/// summary (absent when null), the reason of a dimmed element and its remedy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipRecord {
    pub target: String,
    pub level: u8,
    pub name: String,
    pub chord: Option<String>,
    pub summary: Option<String>,
    pub reason: Option<String>,
    /// The remedy's node id and label.
    pub remedy: Option<String>,
    pub remedy_label: Option<String>,
    /// The element's reason line (its node value), which the tip node carries too (AC-REACH-05).
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TipOn {
    target: String,
    level: u8,
    explicit: bool,
}

/// What an author id names on the menu surfaces.
enum MenuTarget {
    Title(String),
    More,
    MoreTitle(String),
    Node(String),
    Remedy(String),
    TipRemedy,
}

/// Menu and tooltip state (B6).
#[derive(Default)]
struct Menus {
    /// The profile, grown at every evaluation and never shrunk (STU-SHL-048).
    profile: Profile,
    state: MenuState,
    bar: MenuBar,
    /// author id -> node id over the evaluated bar.
    authors: HashMap<String, String>,
    /// The open path: a title id (or [`MORE_MENU`], [`LAYERS_CONTEXT`]) then each open submenu id.
    open: Vec<String>,
    /// The highlighted row's author id (hover or keyboard).
    hot: Option<String>,
    /// First shown row per open level.
    first: Vec<usize>,
    /// Where the context menu opens, logical px.
    anchor: (f32, f32),
    stats: MenuStats,
    rev: u64,
    popup: Option<(String, chrome::PopupFacts)>,
    tip: Option<TipOn>,
    tip_facts: Option<(String, chrome::TipFacts)>,
    clock_ms: u64,
    dwell_ms: u64,
    dwell_target: Option<String>,
    warm: bool,
    quiet_until: u64,
    /// The control just operated: no tip on it until the pointer leaves (STU-SHL-235 rule 2).
    operated: Option<String>,
    tips_off: bool,
    tip_cap: Option<u8>,
}

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
    /// Each dock group's active tab: the three Inspect Dock groups, then the Browse Dock's one group (FIT-L1).
    tabs: [usize; GROUPS],
    focus_group: usize,
    /// The dock group whose +N list is open (D-F4).
    open_more: Option<usize>,
    /// The label of the control the last `frame.static` answered for (the status bar says it in words).
    last_label: Option<String>,
    /// Rotated ruler labels by (icon, px), kept for the draw.
    labels: HashMap<(IconId, u32), IconMask>,
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
    chrome: ChromeMode,
    /// Vulkan mode: a frame was composed (the counterpart of `canvas`), and it is due for a draw.
    composed: bool,
    pending_draw: bool,
    stats: GuiStats,
    paint_stats: PaintStats,
    last: Option<Outcome>,
    device: String,
    notes: Vec<&'static str>,
    /// The last fit decisions and the inputs they were measured from: a frame-local pick re-measures nothing.
    fits: Option<(String, Fits)>,
    /// Menus and tooltips (B6).
    menu: Menus,
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
#[derive(Clone)]
struct Fits {
    menu: (f32, bool, usize),
    title: String,
    tab_w: f32,
    tab_name: String,
    groups: Vec<fit::TabFit>,
    status_shown: [bool; 5],
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
            tabs: [0; GROUPS],
            focus_group: 0,
            open_more: None,
            last_label: None,
            labels: HashMap::new(),
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
            chrome: ChromeMode::Cpu,
            composed: false,
            pending_draw: false,
            stats: GuiStats::default(),
            paint_stats: PaintStats::default(),
            last: None,
            device: "no device".into(),
            notes: Vec::new(),
            fits: None,
            menu: Menus::default(),
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
        self.menu_eval();
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
        (0..GROUPS).find(|&gi| chrome::group_id(group_tabs(gi)[0].0) == group_id).map(|gi| group_tabs(gi)[self.tabs[gi]].0)
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
        // The tooltip, then the open popups, are over every band.
        if self.menu.tip.is_some() && self.menu.tip_facts.as_ref().is_some_and(|(_, t)| t.rect.contains(p)) {
            return Some(TIP);
        }
        if !self.menu.open.is_empty() && self.menu.popup.as_ref().is_some_and(|(_, f)| f.levels.iter().any(|l| l.rect.contains(p))) {
            return Some(POPUP);
        }
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
        self.menu_hover();
        self.dwell_reset();
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
                self.hide_dwell_tip();
                if let Some(p) = self.pointer
                    && !self.menu.open.is_empty()
                    && !matches!(self.panel_at(p), Some(POPUP | MENU | TIP))
                {
                    // A press outside the open menus closes them and goes no further.
                    self.close_menus();
                    return Some(self.frame_outcome("frame.menu_close", "ok", &[]));
                }
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
                self.menu.quiet_until = self.menu.clock_ms.saturating_add(TIP_QUIET_MS);
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
            HostEvent::Button { button: Button::Right, pressed: false } => {
                // The context menu of the row under the pointer (STU-SHL-014).
                let p = self.pointer?;
                let i = self.panel_at(p).filter(|&i| i == INSPECT)?;
                let t = self.panels[i].tree();
                let id = kumihimo_ui::hit::hit_test(t, p).map(|n| t.get(n).author_id.clone())?;
                match self.context_at(&id, Some((p.x, p.y))) {
                    Ok(o) => Some(o),
                    Err(code) => Some(self.frame_outcome("frame.pointer", code, &[STATUS])),
                }
            }
            HostEvent::Wheel { y, .. } if !self.menu.open.is_empty() && self.pointer.is_some_and(|p| self.panel_at(p) == Some(POPUP)) => {
                // The wheel scrolls the deepest popup by rows (positive y reveals rows above).
                self.scroll_menu(if y > 0 { -1 } else { 1 });
                None
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
        if key == Key::Escape && self.menu.tip.is_some() && self.menu.open.is_empty() {
            self.hide_tip();
            return None;
        }
        if !self.menu.open.is_empty() && matches!(key, Key::Up | Key::Down | Key::Enter | Key::Space | Key::Escape) {
            return self.menu_key(key);
        }
        let page = view::rows_page(self.regions.body.h()) as i64;
        // Page keys go only to the active tab's panel: the Layers list scrolls only while Layers is shown.
        if matches!(key, Key::PageDown | Key::PageUp | Key::Home | Key::End) && !self.layers_active() {
            return None;
        }
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
        self.last_label = None;
        self.last = Some(out.clone());
        self.bump(panels);
        self.bump(&[STATUS]);
        out
    }

    /// The command table: every interactive author_id of the frame maps to one entry. Ok(None): an inert control (the
    /// active group tab).
    fn activate(&mut self, id: &str) -> Result<Option<Outcome>, &'static str> {
        self.menu.operated = Some(id.to_owned());
        if let Some(t) = self.menu_target(id) {
            return Ok(Some(self.menu_activate(t)));
        }
        // Any other control closes the open menus.
        if !self.menu.open.is_empty() {
            self.close_menus();
        }
        if let Some(i) = preset::RAIL.iter().position(|g| preset::slot_id(g.group) == id) {
            self.tool = i;
            self.stats.picks = self.stats.picks.saturating_add(1);
            self.menu_eval();
            return Ok(Some(self.frame_outcome("frame.tool_pick", "ok", &[RAIL, CONTEXT, OBSERVE])));
        }
        for gi in 0..GROUPS {
            let tabs = group_tabs(gi);
            let dock = if gi < 3 { INSPECT } else { BROWSE };
            let first = tabs[0].0;
            if chrome::more_id(first) == id {
                // D-F4: +N opens (or closes) the list of this group's hidden tabs.
                self.open_more = if self.open_more == Some(gi) { None } else { Some(gi) };
                return Ok(Some(self.frame_outcome("frame.overflow", "ok", &[INSPECT, BROWSE])));
            }
            let row = tabs.iter().position(|(p, _)| chrome::more_row_id(first, p) == id);
            if let Some(t) = tabs.iter().position(|(p, _)| chrome::tab_id(p) == id).or(row) {
                if t == self.tabs[gi] && row.is_none() {
                    return Ok(None);
                }
                self.tabs[gi] = t;
                self.focus_group = gi;
                self.open_more = None;
                self.stats.picks = self.stats.picks.saturating_add(1);
                self.menu_eval();
                return Ok(Some(self.frame_outcome("frame.tab_select", "ok", &[dock, OBSERVE])));
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
        } else if let Some(label) = self.node(id).filter(|n| n.value.as_deref().is_some_and(|v| v.starts_with(chrome::NIY))).map(|n| n.label.clone().unwrap_or_default()) {
            // An unbuilt control (availability not_implemented_yet as data, normal look): no Viewer call, no popup.
            self.stats.statics = self.stats.statics.saturating_add(1);
            let out = self.frame_outcome("frame.static", chrome::NIY, &[]);
            self.last_label = Some(cut(&label, 64).to_owned());
            return Ok(Some(out));
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
        let left_open = l.left_state == frame::EdgeState::Expanded;
        let groups: Vec<String> = (0..GROUPS)
            .filter(|&gi| gi < 3 || left_open)
            .map(|gi| {
                let g = group_tabs(gi);
                let tabs: Vec<String> = g.iter().map(|(p, _)| jstr(p)).collect();
                format!(
                    "{{\"id\":{},\"tabs\":[{}],\"active\":{},\"collapsed\":{}}}",
                    jstr(&chrome::group_id(g[0].0)),
                    tabs.join(","),
                    jstr(g[self.tabs[gi]].0),
                    l.collapsed.get(gi).copied().unwrap_or(false)
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

    /// The fit decisions of this frame, re-measured only when an input they read changed (extent, scale, text system,
    /// file name, device, document facts, active tabs).
    fn fits(&mut self) -> Fits {
        let key = format!(
            "{:?}|{}|{}|{}|{}|{}|{}|{}|{:?}|{}|{}|{:?}|{:?}",
            self.extent,
            self.scale.get(),
            self.text.epoch(),
            self.file_name,
            self.device,
            self.backend,
            self.revision,
            self.unsaved,
            self.tabs,
            self.size_text(),
            self.tool,
            self.last,
            self.last_label
        );
        if let Some((k, f)) = &self.fits
            && *k == key
        {
            return f.clone();
        }
        let f = self.measure_fits();
        self.fits = Some((key, f.clone()));
        f
    }

    fn measure_fits(&mut self) -> Fits {
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
        let left_avail = (l.left.w() - kumihimo_ui::widgets::frame::GROUP_DISCLOSURE - kumihimo_ui::widgets::frame::GROUP_MENU).max(0.0);
        let groups = (0..GROUPS)
            .map(|gi| {
                let labels: Vec<&str> = group_tabs(gi).iter().map(|(_, n)| *n).collect();
                let room = if gi < 3 { tabs_avail } else { left_avail };
                fit::fit_tabs(t, &labels, self.tabs[gi], TextToken::Tab, 0, active_weight, size::TAB_PAD_X, room, plus)
            })
            .collect();
        // FIT-S1: readouts drop (device first) until the hintline holds the tool name and the whole outcome, and keeps
        // at least 320.
        let texts = status_texts(&self.device, self.backend, &size_text, self.revision, self.chrome);
        let hint_w = t.measure(preset::RAIL[self.tool].name, TextToken::Body, 0).ceil();
        let outcome_w = t.measure(&outcome_words(self.last.as_ref(), self.last_label.as_deref()), TextToken::Body, 0).ceil();
        let widths: Vec<f32> = texts.iter().map(|s| t.measure(s, TextToken::Body, 0).ceil()).collect();
        let gap = kumihimo_ui::widgets::frame::STATUS_GAP;
        let avail = l.status.w() - 16.0;
        let mut status_shown = [true; 5];
        let fits_now = |shown: &[bool; 5]| {
            let n = shown.iter().filter(|s| **s).count();
            let right: f32 = (0..5).filter(|&i| shown[i]).map(|i| widths[i]).sum::<f32>() + n.saturating_sub(1) as f32;
            let children = 2 + n + n.saturating_sub(1) + (5 - n);
            let room = avail - hint_w - right - (children - 1) as f32 * gap;
            room >= outcome_w && hint_w + gap + room >= 320.0
        };
        for &i in &chrome::DROP_ORDER {
            if fits_now(&status_shown) {
                break;
            }
            status_shown[i] = false;
        }
        Fits { menu, title, tab_w, tab_name, groups, status_shown }
    }

    fn size_text(&self) -> String {
        self.image.as_ref().map_or_else(|| "no frame".to_owned(), |i| format!("{} x {} px", i.width, i.height))
    }

    fn update(&mut self) -> Result<bool, HostError> {
        let fits = self.fits();
        let (s, theme) = (self.scale, self.theme);
        let l = self.layout.clone();
        let revs = self.revs;
        let command = outcome_words(self.last.as_ref(), self.last_label.as_deref());
        let command_code = self.last.as_ref().map_or_else(String::new, |o| format!("{} {}", o.command, o.code));
        let texts = status_texts(&self.device, self.backend, &self.size_text(), self.revision, self.chrome);
        let title_full = format!("{}: Studio", self.file_name);
        let origin = self.image_rect().map_or((l.canvas.x0, l.canvas.y0), |r| (r.x0, r.y0));
        let face = &preset::RAIL[self.tool];
        let selection = self.selected.as_deref().and_then(|id| self.rows.iter().find(|r| r.layer_id == id)).map(|r| format!("{} layer", r.kind));
        let dock_state = self.dock_state();
        let tool = self.tool_active().to_owned();
        let popup = self.popup_facts();
        let tipf = self.tip_now();
        let (rows, selected, first, body_h) = (&self.rows, self.selected.as_deref(), self.first_row, self.regions.body.h());
        let shown: Vec<Vec<usize>> = fits.groups.iter().map(|g| g.shown.clone()).collect();
        let facts = |i: usize| chrome::GroupFacts {
            tabs: group_tabs(i),
            active: self.tabs[i],
            shown: &shown[i],
            collapsed: i < 3 && l.collapsed.get(i).copied().unwrap_or(true),
            focused: i == self.focus_group,
            h: if i < 3 { Some(l.groups.get(i).map_or(0.0, |r| r.h())) } else { None },
            open: self.open_more == Some(i),
        };
        let groups: Vec<chrome::GroupFacts<'_>> = (0..3).map(facts).collect();
        let left_group = facts(3);
        let left_open = l.left_state == frame::EdgeState::Expanded;
        let states = self.title_states();
        let menu = chrome::MenuFacts { pad: fits.menu.0, abbreviated: fits.menu.1, shown: fits.menu.2, title: &fits.title, title_full: &title_full, states: &states };
        let tab = chrome::TabFacts { full: &self.file_name, shown: &fits.tab_name, w: fits.tab_w, unsaved: self.unsaved };
        let ctx = chrome::ContextFacts { tool_name: face.name, tool_icon: chrome::tool_icon(face.icon), selection: selection.as_deref() };
        let rail = chrome::RailFacts { pitch: l.rail_pitch, shown: l.rail_shown, active: self.tool };
        let vp = chrome::ViewportFacts { file: &self.file_name, size: self.image.as_ref().map(|i| (i.width, i.height)), revision: self.revision, render_code: self.render_code, origin };
        let layers = chrome::LayersFacts { rows, selected, first, body_h };
        let status = chrome::StatusFacts {
            hint: preset::RAIL[self.tool].name,
            command: &command,
            command_code: &command_code,
            readouts: [texts[0].as_str(), texts[1].as_str(), texts[2].as_str(), texts[3].as_str(), texts[4].as_str()],
            shown: fits.status_shown,
            chrome: self.chrome.code(),
        };
        let text = &mut self.text;
        let ui = &self.ui;
        let p = &mut self.panels;
        let err = |e: BuildError| HostError::Document(build_code(&e));
        let mut any = false;
        any |= p[MENU].update(revs[MENU], ui, l.menu_bar, s, theme, text, |b| chrome::menu_bar(b, &menu)).map_err(err)?;
        any |= p[TABS].update(revs[TABS], ui, l.tab_strip, s, theme, text, |b| chrome::tab_strip(b, &tab)).map_err(err)?;
        any |= p[CONTEXT].update(revs[CONTEXT], ui, l.context_bar, s, theme, text, |b| chrome::context_bar(b, &ctx)).map_err(err)?;
        any |= p[RAIL].update(revs[RAIL], ui, l.tool_rail, s, theme, text, |b| chrome::tool_rail(b, &rail)).map_err(err)?;
        any |= p[BROWSE].update(revs[BROWSE], ui, l.left, s, theme, text, |b| if left_open { chrome::browse_dock(b, &left_group) } else { chrome::browse_rail(b) }).map_err(err)?;
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
        let popup_rect = popup.as_ref().map_or_else(Rect::default, |f| f.rect);
        any |= p[POPUP]
            .update(revs[POPUP], ui, popup_rect, s, theme, text, |b| {
                if let Some(f) = &popup {
                    chrome::popups(b, f);
                }
            })
            .map_err(err)?;
        let tip_rect = tipf.as_ref().map_or_else(Rect::default, |t| t.rect);
        any |= p[TIP]
            .update(revs[TIP], ui, tip_rect, s, theme, text, |b| {
                if let Some(t) = &tipf {
                    chrome::tip(b, t);
                }
            })
            .map_err(err)?;
        any |= p[OBSERVE].update(revs[OBSERVE], ui, Rect::default(), s, theme, text, |b| chrome::observe(b, &dock_state, &tool)).map_err(err)?;
        Ok(any)
    }

    /// The rotated ruler labels of the Viewport list at the px paint will ask for (D-F6), rasterised with the frame's
    /// Text; at most 512 kept (reset whole beyond).
    fn prepare_labels(&mut self) {
        let s = self.scale;
        let want: Vec<(IconId, u32)> = self.panels[VIEWPORT]
            .display_list()
            .items
            .iter()
            .filter_map(|it| match it {
                Prim::Icon { rect, icon, .. } if ruler_label_value(*icon).is_some() => {
                    let p = s.snap(*rect);
                    Some((*icon, p.w().min(p.h()).max(1) as u32))
                }
                _ => None,
            })
            .collect();
        if self.labels.len() + want.len() > 512 {
            self.labels.clear();
        }
        for key in want {
            if self.labels.contains_key(&key) {
                continue;
            }
            let Some(v) = ruler_label_value(key.0) else { continue };
            if let Some(m) = self.text.rotated_label(&v.to_string(), TextToken::Caption, s) {
                self.labels.insert(key, m);
            }
        }
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
        self.prepare_labels();
        let p = &self.panels;
        let mut lists: Vec<&DisplayList> = Vec::with_capacity(PANELS + 2);
        lists.extend([MENU, TABS, CONTEXT, RAIL].map(|i| p[i].display_list()));
        lists.push(&proxy);
        lists.extend([BROWSE, VIEWPORT].map(|i| p[i].display_list()));
        lists.extend(doc.as_ref());
        lists.extend([INSPECT, TIME, STATUS].map(|i| p[i].display_list()));
        // B6: the popups and the tooltip over everything, only while shown (a closed frame keeps its list count).
        if !self.menu.open.is_empty() {
            lists.push(p[POPUP].display_list());
        }
        if self.menu.tip.is_some() {
            lists.push(p[TIP].display_list());
        }
        let mut res = Res { icons: &mut self.icons, image: self.image.as_ref(), labels: &self.labels };
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
        let shown = match self.chrome {
            ChromeMode::Cpu => self.canvas.is_some(),
            ChromeMode::Vulkan => self.composed,
        };
        if !rebuilt && !self.repaint && shown {
            self.stats.idle_frames = self.stats.idle_frames.saturating_add(1);
            return Ok(false);
        }
        match self.chrome {
            ChromeMode::Cpu => {
                let mut cpu = CpuChrome::default();
                self.compose(&mut cpu)?;
                self.paint_stats = cpu.stats;
                self.canvas = cpu.canvas;
                self.pending_upload = true;
            }
            // The pass draws where the host draws (GuiFrame::draw): no canvas.
            ChromeMode::Vulkan => {
                self.composed = true;
                self.pending_draw = true;
            }
        }
        self.repaint = false;
        self.stats.paints = self.stats.paints.saturating_add(1);
        Ok(true)
    }

    /// Selects the chrome target (item 7): Vulkan mode keeps no canvas.
    pub fn set_chrome(&mut self, mode: ChromeMode) {
        if mode != self.chrome {
            self.chrome = mode;
            self.canvas = None;
            self.composed = false;
            self.pending_upload = false;
            self.pending_draw = false;
            self.repaint = true;
            // The status bar names the chrome: its panel rebuilds.
            self.bump(&[STATUS]);
        }
    }
    pub fn chrome(&self) -> ChromeMode {
        self.chrome
    }
    /// Vulkan mode: draws the frame through `target` when it changed (or the pane was re-created); false when idle,
    /// minimised or in CPU mode.
    pub fn draw(&mut self, target: &mut dyn ChromeTarget) -> Result<bool, HostError> {
        if self.chrome != ChromeMode::Vulkan || !self.pending_draw || self.minimized {
            return Ok(false);
        }
        self.compose(target)?;
        self.pending_draw = false;
        self.stats.uploads = self.stats.uploads.saturating_add(1);
        Ok(true)
    }

    /// Copies the painted frame into the sink when it changed since the last upload.
    pub fn upload(&mut self, sink: &mut dyn PaneSink) -> Result<bool, Refusal> {
        if self.chrome != ChromeMode::Cpu || !self.pending_upload || self.minimized {
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
        // Vulkan mode: a re-created pane starts empty and is drawn by the next draw.
        self.pending_draw = self.composed;
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
            ("menu", self.menu_receipt()),
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

/// An availability record as node properties (STU-SHL-019).
fn node_availability(a: &Availability) -> NodeAvailability {
    NodeAvailability { state: a.state.code().to_owned(), reason_code: a.reason_code.map(|c| c.code().to_owned()), reason_text: a.reason_text.clone(), remedy_command_id: a.remedy_command_id.clone() }
}

/// The author id of an open popup: `studio.menu.<title>` for a title, `studio.menu.<id without its studio prefix>` in
/// kebab case for a submenu, `studio.menu.more` and `studio.ctx.layers` for the two roots that are not titles
/// (components.json `menu_popup`, `context_menu`).
pub fn popup_id(node_id: &str, title: bool) -> String {
    match node_id {
        MORE_MENU => "studio.menu.more".to_owned(),
        LAYERS_CONTEXT => "studio.ctx.layers".to_owned(),
        _ if title => format!("studio.menu.{node_id}"),
        _ => format!("studio.menu.{}", node_id.strip_prefix("studio.").unwrap_or(node_id).replace('_', "-")),
    }
}

/// The author id of a hidden title's row in the chevron's list.
pub fn more_row_id(title: &str) -> String {
    format!("studio.menu.more.{title}")
}

impl GuiFrame {
    // ---- menus (build-order B6, SMB-GUI-MENU-1) ----

    /// The evaluated menu bar: one availability per node (STU-SHL-046), the record every surface reads.
    pub fn menu_bar(&self) -> &MenuBar {
        &self.menu.bar
    }
    /// The state the last evaluation read (the grown profile, selection, transient features).
    pub fn menu_state(&self) -> &MenuState {
        &self.menu.state
    }
    /// The open path: a title id (or `more`, `ctx.layers`) then each open submenu id; empty when closed.
    pub fn menu_open(&self) -> &[String] {
        &self.menu.open
    }
    pub fn menu_stats(&self) -> MenuStats {
        self.menu.stats
    }
    /// The model-navigable menu as JSON text: every node with its path, author id and availability, the profile and
    /// the counts (harness-view `menu_tree::bar_json`).
    pub fn menu_json(&self) -> String {
        menu_tree::bar_json(&self.menu.bar, &self.menu.state).to_string()
    }

    /// WORKSPACE > Panels: one leaf per panel of the raster preset, grafted into the expansion (its 90-panel registry
    /// is not in this host). A group's panel picks its tab here; the Time & Results panels have no bodies yet.
    fn host_leaves(&self) -> Vec<HostLeaf> {
        let mut out = Vec::new();
        for gi in 0..GROUPS {
            for (t, (panel, name)) in group_tabs(gi).iter().enumerate() {
                out.push(HostLeaf {
                    parent: WORKSPACE_PANELS.to_owned(),
                    id: panel_leaf_id(panel),
                    label: (*name).to_owned(),
                    requires: Vec::new(),
                    unbuilt: None,
                    selected: self.tabs[gi] == t,
                    help: format!("Shows the {name} panel in its group."),
                });
            }
        }
        for p in preset::BOTTOM {
            out.push(HostLeaf {
                parent: WORKSPACE_PANELS.to_owned(),
                id: panel_leaf_id(p.id),
                label: p.name.to_owned(),
                requires: Vec::new(),
                unbuilt: Some("the Time & Results Dock has no panel bodies in this host yet".to_owned()),
                selected: false,
                help: format!("Shows the {} panel.", p.name),
            });
        }
        out
    }

    /// The one availability evaluation per context change (STU-SHL-046): after a document refresh, a tool pick or a
    /// tab switch; never per frame. The profile grows first (STU-SHL-048). The open path keeps the nodes that remain.
    fn menu_eval(&mut self) {
        let derived = Profile::of(&self.viewer);
        if self.menu.profile.grow(&derived) {
            self.menu.stats.profile_growth = self.menu.stats.profile_growth.saturating_add(1);
        }
        let tool = preset::RAIL[self.tool].face_tool;
        self.menu.state = MenuState::of(&self.viewer, self.menu.profile.clone(), Some(tool));
        let host = self.host_leaves();
        let titles_before: Vec<Availability> = self.menu.bar.titles.iter().map(|t| t.availability.clone()).collect();
        self.menu.bar = menu_tree::menu_bar(&self.menu.state, &host);
        let titles_changed = self.menu.bar.titles.iter().map(|t| &t.availability).ne(titles_before.iter());
        let mut authors = HashMap::new();
        for w in self.menu.bar.walk() {
            authors.insert(menu_tree::node_author_id(&w.node.id), w.node.id.clone());
        }
        self.menu.authors = authors;
        let was_open = !self.menu.open.is_empty();
        let keep = self.menu.open.iter().take_while(|id| matches!(id.as_str(), MORE_MENU | LAYERS_CONTEXT) || self.menu.bar.find(id).is_some()).count();
        self.menu.open.truncate(keep);
        self.menu.first.truncate(keep);
        self.menu.stats.evaluations = self.menu.stats.evaluations.saturating_add(1);
        // Only what the evaluation changed rebuilds: the titles when a title's record changed, the popups while open
        // (a tool pick or a tab switch changes neither in a closed frame).
        self.menu.rev = self.menu.rev.wrapping_add(1);
        if titles_changed {
            self.bump(&[MENU]);
        }
        if was_open {
            self.bump(&[POPUP]);
        }
    }

    /// Each title's B6 state in STU-SHL-011 order.
    fn title_states(&self) -> Vec<kumihimo_ui::widgets::TitleState> {
        let open = match self.menu.open.first().map(String::as_str) {
            Some(MORE_MENU) => self.menu.open.get(1).map(String::as_str),
            o => o,
        };
        preset::MENU
            .iter()
            .map(|t| {
                let id = t.to_ascii_lowercase();
                let node = self.menu.bar.titles.iter().find(|n| n.id == id);
                let greyed = node.is_some_and(|n| !n.availability.is_available());
                kumihimo_ui::widgets::TitleState {
                    availability: node.map(|n| node_availability(&n.availability)),
                    value: node.filter(|_| greyed).map(|n| n.availability.reason_line()),
                    greyed,
                    open: open == Some(id.as_str()),
                }
            })
            .collect()
    }

    /// What `author` names on the menu surfaces, if anything.
    fn menu_target(&self, author: &str) -> Option<MenuTarget> {
        if author == chrome::MORE_ID {
            return Some(MenuTarget::More);
        }
        if author == chrome::TIP_REMEDY_ID {
            return self.menu.tip.as_ref().map(|_| MenuTarget::TipRemedy);
        }
        if let Some(t) = author.strip_prefix("studio.menu.more.")
            && self.menu.bar.titles.iter().any(|x| x.id == t)
        {
            return Some(MenuTarget::MoreTitle(t.to_owned()));
        }
        if let Some(base) = author.strip_suffix(".remedy")
            && let Some(id) = self.menu.authors.get(base)
        {
            return Some(MenuTarget::Remedy(id.clone()));
        }
        let id = self.menu.authors.get(author)?;
        if self.menu.bar.titles.iter().any(|t| &t.id == id) {
            return Some(MenuTarget::Title(id.clone()));
        }
        Some(MenuTarget::Node(id.clone()))
    }

    fn menu_activate(&mut self, t: MenuTarget) -> Outcome {
        match t {
            MenuTarget::Title(id) => {
                if self.menu.open.first() == Some(&id) {
                    self.close_menus();
                    return self.frame_outcome("frame.menu_close", "ok", &[]);
                }
                self.open_menu(vec![id])
            }
            MenuTarget::More => {
                if self.menu.open.first().map(String::as_str) == Some(MORE_MENU) {
                    self.close_menus();
                    return self.frame_outcome("frame.menu_close", "ok", &[]);
                }
                self.open_menu(vec![MORE_MENU.to_owned()])
            }
            MenuTarget::MoreTitle(t) => self.open_menu(vec![MORE_MENU.to_owned(), t]),
            MenuTarget::Node(id) => self.menu_node(&id),
            MenuTarget::Remedy(id) => self.run_remedy(&id),
            MenuTarget::TipRemedy => {
                let target = self.menu.tip.as_ref().map(|t| t.target.clone()).unwrap_or_default();
                self.hide_tip();
                match self.menu.authors.get(&target).cloned() {
                    Some(id) => self.run_remedy(&id),
                    None => self.frame_outcome("frame.menu", "menu_node_unknown", &[]),
                }
            }
        }
    }

    /// Runs the remedy of node `id` (its INAPPLICABLE_HERE record's one enabled command, STU-SHL-020).
    fn run_remedy(&mut self, id: &str) -> Outcome {
        match self.menu.bar.find(id).and_then(|n| n.availability.remedy_command_id.clone()) {
            Some(r) => {
                self.menu.stats.remedies = self.menu.stats.remedies.saturating_add(1);
                self.menu_node(&r)
            }
            None => self.frame_outcome("frame.menu", "menu_no_remedy", &[]),
        }
    }

    fn set_open(&mut self, path: Vec<String>) {
        if self.menu.open == path {
            return;
        }
        let keep = self.menu.open.iter().zip(&path).take_while(|(a, b)| a == b).count();
        self.menu.first.truncate(keep);
        self.menu.first.resize(path.len(), 0);
        self.menu.open = path;
        self.menu.stats.opens = self.menu.stats.opens.saturating_add(1);
        self.menu_changed();
        self.bump(&[MENU]);
    }

    fn open_menu(&mut self, path: Vec<String>) -> Outcome {
        if path.len() == 1 {
            self.menu.hot = None;
        }
        self.set_open(path);
        self.hide_dwell_tip();
        self.frame_outcome("frame.menu_open", "ok", &[MENU])
    }

    fn close_menus(&mut self) {
        if self.menu.open.is_empty() {
            return;
        }
        self.menu.open.clear();
        self.menu.first.clear();
        self.menu.hot = None;
        if self.menu.tip.as_ref().is_some_and(|t| self.panels[POPUP].tree().find(&t.target).is_some()) {
            self.hide_tip();
        }
        self.menu_changed();
        self.bump(&[MENU]);
    }

    /// The popups' content changed (open path, highlight, scroll): they rebuild; the titles rebuild only when the
    /// open title changes (set_open, close_menus).
    fn menu_changed(&mut self) {
        self.menu.rev = self.menu.rev.wrapping_add(1);
        self.bump(&[POPUP]);
    }

    fn set_hot(&mut self, h: String) {
        if self.menu.hot.as_ref() != Some(&h) {
            self.menu.hot = Some(h);
            self.menu_changed();
        }
    }

    /// The ids from a title down to node `id`.
    fn path_to(&self, id: &str) -> Option<Vec<String>> {
        fn go(n: &MenuNode, id: &str, path: &mut Vec<String>) -> bool {
            path.push(n.id.clone());
            if n.id == id || n.children.iter().any(|c| go(c, id, path)) {
                return true;
            }
            path.pop();
            false
        }
        let mut path = Vec::new();
        self.menu.bar.titles.iter().any(|t| go(t, id, &mut path)).then_some(path)
    }

    /// The open path that shows node `id`'s cascade (under the chevron's list when that is the root).
    fn cascade_path(&self, id: &str) -> Vec<String> {
        let mut path = self.path_to(id).unwrap_or_default();
        if self.menu.open.first().map(String::as_str) == Some(MORE_MENU) {
            path.insert(0, MORE_MENU.to_owned());
        }
        path
    }

    /// A menu node activated: a submenu opens its cascade; a greyed leaf answers its reason code and runs nothing
    /// (the menu stays open, the status bar says the reason); an enabled leaf runs and the menus close.
    fn menu_node(&mut self, id: &str) -> Outcome {
        let Some(n) = self.menu.bar.find(id) else { return self.frame_outcome("frame.menu", "menu_node_unknown", &[]) };
        let (kind, label, availability) = (n.kind, n.label.clone(), n.availability.clone());
        let author = menu_tree::node_author_id(id);
        if kind != MenuNodeKind::Item {
            let path = self.cascade_path(id);
            self.menu.hot = Some(author);
            return self.open_menu(path);
        }
        if !availability.is_available() {
            self.menu.stats.unavailable = self.menu.stats.unavailable.saturating_add(1);
            let code = menu_tree::reason_code_lower(&availability);
            let out = self.frame_outcome("frame.menu_unavailable", &code, &[]);
            self.last_label = Some(cut(&format!("{label}: {}", availability.reason_text.as_deref().unwrap_or_default()), 160).to_owned());
            self.set_hot(author);
            return out;
        }
        if let Some(o) = self.host_run(id) {
            self.menu.stats.runs = self.menu.stats.runs.saturating_add(1);
            self.close_menus();
            return o;
        }
        let result = menu_tree::run(&mut self.viewer, &self.menu.bar, id);
        self.stats.commands = self.stats.commands.saturating_add(1);
        let out = match result {
            Ok(r) => {
                self.menu.stats.runs = self.menu.stats.runs.saturating_add(1);
                Outcome { command: r.command, code: "ok".to_owned() }
            }
            Err(e) => {
                self.menu.stats.refused = self.menu.stats.refused.saturating_add(1);
                self.stats.refused = self.stats.refused.saturating_add(1);
                Outcome { command: e.command.unwrap_or("frame.menu"), code: e.code }
            }
        };
        self.close_menus();
        self.refresh();
        self.last_label = None;
        self.last = Some(out.clone());
        out
    }

    /// A WORKSPACE > Panels leaf: picks that panel's tab in its group (`frame.tab_select`, view state).
    fn host_run(&mut self, id: &str) -> Option<Outcome> {
        for gi in 0..GROUPS {
            if let Some(t) = group_tabs(gi).iter().position(|(p, _)| panel_leaf_id(p) == id) {
                self.tabs[gi] = t;
                self.focus_group = gi;
                self.open_more = None;
                self.stats.picks = self.stats.picks.saturating_add(1);
                let dock = if gi < 3 { INSPECT } else { BROWSE };
                self.menu_eval();
                return Some(self.frame_outcome("frame.tab_select", "ok", &[dock, OBSERVE]));
            }
        }
        None
    }

    /// Opens the Layers context menu on the row `author_id` (a Layers row or its visibility box), as Shift+F10 or the
    /// Menu key would on that row (STU-MDL-109: by author id): selects the layer (view state, no undo step) and opens the
    /// menu under the row. Its rows reference menu-bar leaves and run the identical commands (STU-SHL-014).
    /// `Err("menu_context_none")` for any other id, `Err("unknown_layer")` for a row that is gone.
    pub fn open_context(&mut self, author_id: &str) -> Result<Outcome, &'static str> {
        self.context_at(author_id, None)
    }

    fn context_at(&mut self, author_id: &str, at: Option<(f32, f32)>) -> Result<Outcome, &'static str> {
        let rest = author_id.strip_prefix(ROW_PREFIX).ok_or("menu_context_none")?;
        let rest = rest.strip_suffix(".visibility").unwrap_or(rest);
        let layer = self.rows.iter().find(|r| r.layer_id.len() == rest.len() && r.layer_id.eq_ignore_ascii_case(rest)).map(|r| r.layer_id.clone()).ok_or("unknown_layer")?;
        let at = at.or_else(|| self.node(&row_id(&layer)).map(|n| (n.bounds.x0 + size::MENU_GUTTER_ICON, n.bounds.y1))).unwrap_or((0.0, 0.0));
        self.menu.anchor = at;
        self.viewer.select(Some(layer));
        self.refresh();
        self.menu.hot = None;
        Ok(self.open_menu(vec![LAYERS_CONTEXT.to_owned()]))
    }

    /// Popup level `k`: its author id, label, rows (each INAPPLICABLE_HERE row followed by its remedy row) and, for a
    /// node with no children, the empty line saying why.
    #[allow(clippy::type_complexity)]
    fn level_rows(&self, k: usize) -> Option<(String, String, Vec<chrome::PopupRow>, Option<(String, String, String, String)>)> {
        let root = self.menu.open.first()?.as_str();
        let child_open = self.menu.open.get(k.saturating_add(1)).map(String::as_str);
        let (id, label, nodes, owner): (String, String, Vec<(&MenuNode, String)>, Option<&MenuNode>) = if k == 0 && root == MORE_MENU {
            let shown = self.fits.as_ref().map_or(preset::MENU.len(), |(_, f)| f.menu.2);
            let titles = self.menu.bar.titles.iter().skip(shown).map(|t| (t, more_row_id(&t.id))).collect();
            (popup_id(MORE_MENU, false), "More menus".to_owned(), titles, None)
        } else if k == 0 && root == LAYERS_CONTEXT {
            let rows = preset::LAYER_CONTEXT.iter().filter_map(|id| self.menu.bar.find(id)).map(|n| (n, menu_tree::node_author_id(&n.id))).collect();
            (popup_id(LAYERS_CONTEXT, false), "Layer".to_owned(), rows, None)
        } else {
            let n = self.menu.bar.find(self.menu.open.get(k)?)?;
            let rows = n.children.iter().map(|c| (c, menu_tree::node_author_id(&c.id))).collect();
            (popup_id(&n.id, n.kind == MenuNodeKind::Title), n.label.clone(), rows, Some(n))
        };
        let hot = self.menu.hot.as_deref();
        let mut out = Vec::with_capacity(nodes.len());
        for (n, author) in nodes {
            let a = &n.availability;
            let greyed = !a.is_available();
            out.push(chrome::PopupRow {
                hot: hot == Some(author.as_str()),
                id: author,
                label: n.label.clone(),
                chord: menu_tree::binding(&n.id).and_then(|b| preset::chord_of(b.op.command_id())).map(str::to_owned),
                reason: greyed.then(|| a.reason_text.clone().unwrap_or_default()),
                availability: Some(node_availability(a)),
                value: greyed.then(|| a.reason_line()),
                submenu: n.kind != MenuNodeKind::Item,
                open: child_open == Some(n.id.as_str()),
                checked: n.selected,
                shown: true,
                remedy: false,
                y: 0.0,
            });
            if a.state == AvailabilityState::InapplicableHere
                && let Some(r) = &a.remedy_command_id
            {
                let label = self.menu.bar.find(r).map_or_else(|| r.clone(), |m| m.label.clone());
                let rid = menu_tree::remedy_author_id(&n.id);
                out.push(chrome::PopupRow {
                    hot: hot == Some(rid.as_str()),
                    id: rid,
                    label: format!("-> {label}"),
                    chord: None,
                    reason: None,
                    availability: None,
                    value: Some(format!("remedy {r}")),
                    submenu: false,
                    open: false,
                    checked: false,
                    shown: true,
                    remedy: true,
                    y: 0.0,
                });
            }
        }
        let empty = if out.is_empty() {
            owner.map(|n| (format!("{id}.empty"), n.label.clone(), n.availability.reason_text.clone().unwrap_or_default(), n.availability.reason_line()))
        } else {
            None
        };
        Some((id, label, out, empty))
    }

    /// Where the first popup opens: under its title (the chevron for a hidden title), at the context anchor.
    fn menu_anchor(&self, top: f32) -> (f32, f32) {
        let chevron = || self.node(chrome::MORE_ID).map_or(0.0, |n| n.bounds.x0);
        match self.menu.open.first().map(String::as_str) {
            Some(LAYERS_CONTEXT) => self.menu.anchor,
            Some(MORE_MENU) | None => (chevron(), top),
            Some(t) => match self.node(&menu_tree::node_author_id(t)).map(|n| n.bounds).filter(|b| !b.is_empty()) {
                Some(b) => (b.x0, top),
                None => (chevron(), top),
            },
        }
    }

    fn popup_facts(&mut self) -> Option<chrome::PopupFacts> {
        if self.menu.open.is_empty() {
            return None;
        }
        let top = self.layout.menu_bar.y1;
        let key = format!("{}|{:?}|{}|{}|{:?}|{:?}", self.menu.rev, self.extent, self.scale.get(), self.text.epoch(), self.menu_anchor(top), self.fits.as_ref().map(|(_, f)| f.menu));
        if let Some((k, f)) = &self.menu.popup
            && *k == key
        {
            return Some(f.clone());
        }
        let f = self.measure_popups();
        self.menu.popup = Some((key, f.clone()));
        Some(f)
    }

    /// Lays the open popups out: width from the measured rows (220 to 480), height from the rows that fit under the
    /// menu bar (the rest scroll: in the tree with no box), cascades at the parent's right edge level with their row,
    /// the whole set moved left into the window.
    fn measure_popups(&mut self) -> chrome::PopupFacts {
        use kumihimo_ui::widgets::popup;
        let win = self.layout.window;
        let top = self.layout.menu_bar.y1;
        let mut levels: Vec<chrome::PopupLevel> = Vec::new();
        for k in 0..self.menu.open.len() {
            let Some((id, label, mut rows, empty)) = self.level_rows(k) else { break };
            let t = &mut self.text;
            let (gutter, pad) = (size::MENU_GUTTER_ICON, popup::PAD_R);
            let mut w: f32 = 0.0;
            for r in &rows {
                let mut rw = gutter + t.measure(&r.label, TextToken::Body, 0) + pad;
                if let Some(c) = &r.chord {
                    rw += popup::CHORD_GAP + t.measure(c, TextToken::Body, 0);
                }
                if r.submenu {
                    rw += popup::CHORD_GAP + popup::CHEVRON;
                }
                w = w.max(rw);
                if let Some(reason) = &r.reason {
                    w = w.max(gutter + t.measure(reason, TextToken::Caption, 0) + pad);
                }
            }
            if let Some((_, l, reason, _)) = &empty {
                w = w.max(gutter + t.measure(l, TextToken::Body, 0) + pad).max(gutter + t.measure(reason, TextToken::Caption, 0) + pad);
            }
            let w = w.ceil().clamp(size::MENU_MIN_W, size::MENU_MAX_W);
            let (x0, y0) = match levels.last() {
                None => self.menu_anchor(top),
                Some(prev) => (prev.rect.x1, prev.rows.iter().find(|r| r.open && r.shown).map_or(prev.rect.y0, |r| r.y - popup::PAD_Y)),
            };
            let hs: Vec<f32> = rows.iter().map(|r| kumihimo_ui::widgets::menu_row_h(r.reason.is_some())).collect();
            let room = (win.y1 - top - 4.0).max(2.0 * popup::PAD_Y + size::MENU_ITEM_H);
            let end_from = |first: usize| {
                let mut used = 2.0 * popup::PAD_Y;
                let mut end = first;
                while end < hs.len() && used + hs[end] <= room {
                    used += hs[end];
                    end += 1;
                }
                end.max(first.saturating_add(1).min(hs.len()))
            };
            let mut first = self.menu.first.get(k).copied().unwrap_or(0).min(rows.len().saturating_sub(1));
            if let Some(h) = rows.iter().position(|r| r.hot) {
                first = first.min(h);
                while end_from(first) <= h && first < h {
                    first += 1;
                }
            }
            let end = end_from(first);
            if let Some(f) = self.menu.first.get_mut(k) {
                *f = first;
            }
            let body: f32 = if rows.is_empty() { kumihimo_ui::widgets::menu_row_h(true) } else { hs[first..end].iter().sum() };
            let h = 2.0 * popup::PAD_Y + body;
            let y0 = if y0 + h > win.y1 { (win.y1 - h).max(top.min(y0)) } else { y0 };
            let mut y = y0 + popup::PAD_Y;
            for (i, r) in rows.iter_mut().enumerate() {
                r.shown = i >= first && i < end;
                if r.shown {
                    r.y = y;
                    y += hs[i];
                }
            }
            levels.push(chrome::PopupLevel { id, label, rect: Rect::xywh(x0, y0, w, h), rows, empty });
        }
        let mut rect = levels.first().map_or_else(Rect::default, |l| l.rect);
        for l in &levels {
            rect = Rect::new(rect.x0.min(l.rect.x0), rect.y0.min(l.rect.y0), rect.x1.max(l.rect.x1), rect.y1.max(l.rect.y1));
        }
        if rect.x1 > win.x1 {
            let dx = (rect.x1 - win.x1).min(rect.x0 - win.x0).max(0.0);
            for l in &mut levels {
                l.rect = Rect::new(l.rect.x0 - dx, l.rect.y0, l.rect.x1 - dx, l.rect.y1);
            }
            rect = Rect::new(rect.x0 - dx, rect.y0, rect.x1 - dx, rect.y1);
        }
        chrome::PopupFacts { rect, levels }
    }

    /// The wheel over a popup: the deepest level scrolls by `rows`.
    fn scroll_menu(&mut self, rows: i64) {
        let Some(k) = self.menu.open.len().checked_sub(1) else { return };
        let n = self.level_rows(k).map_or(0, |(_, _, r, _)| r.len());
        if let Some(f) = self.menu.first.get_mut(k) {
            let next = i64::try_from(*f).unwrap_or(0).saturating_add(rows).clamp(0, i64::try_from(n.saturating_sub(1)).unwrap_or(0));
            *f = usize::try_from(next).unwrap_or(0);
        }
        self.menu_changed();
    }

    /// Keys while a menu is open: Down and Up move the highlight through every row of the deepest popup (greyed rows
    /// too: their reason is read there), Enter or Space activates it, Escape closes the topmost popup.
    fn menu_key(&mut self, key: Key) -> Option<Outcome> {
        let k = self.menu.open.len().checked_sub(1)?;
        match key {
            Key::Escape => {
                if k == 0 {
                    self.close_menus();
                    return Some(self.frame_outcome("frame.menu_close", "ok", &[]));
                }
                let last = self.menu.open[k].clone();
                let hot = if k == 1 && self.menu.open[0] == MORE_MENU { more_row_id(&last) } else { menu_tree::node_author_id(&last) };
                let path = self.menu.open[..k].to_vec();
                self.set_open(path);
                self.set_hot(hot);
                None
            }
            Key::Up | Key::Down => {
                let rows: Vec<String> = self.level_rows(k).map(|(_, _, r, _)| r.into_iter().map(|r| r.id).collect()).unwrap_or_default();
                let n = rows.len();
                if n == 0 {
                    return None;
                }
                let at = self.menu.hot.as_ref().and_then(|h| rows.iter().position(|r| r == h));
                let next = match (at, key == Key::Down) {
                    (None, true) => 0,
                    (None, false) => n - 1,
                    (Some(i), true) => (i + 1) % n,
                    (Some(i), false) => (i + n - 1) % n,
                };
                self.set_hot(rows[next].clone());
                None
            }
            _ => {
                let hot = self.menu.hot.clone()?;
                let t = self.menu_target(&hot)?;
                Some(self.menu_activate(t))
            }
        }
    }

    /// Hover while a menu is open: another title switches menus, a submenu row opens its cascade at once (no delay,
    /// components.json `menu_popup.pointer`), an item row closes deeper cascades; the hovered row is highlighted.
    fn menu_hover(&mut self) {
        if self.menu.open.is_empty() {
            return;
        }
        let Some(h) = self.ui.hovered.clone() else { return };
        let in_popup = self.panels[POPUP].tree().find(&h).is_some();
        let in_bar = self.panels[MENU].tree().find(&h).is_some();
        if !in_popup && !in_bar {
            return;
        }
        match self.menu_target(&h) {
            Some(MenuTarget::Title(t)) if in_bar => {
                let root = self.menu.open[0].clone();
                if root != t && root != LAYERS_CONTEXT {
                    self.menu.hot = None;
                    self.set_open(vec![t]);
                }
            }
            Some(MenuTarget::MoreTitle(t)) if in_popup => {
                self.set_hot(h);
                self.set_open(vec![MORE_MENU.to_owned(), t]);
            }
            Some(MenuTarget::Node(id)) if in_popup => {
                let sub = self.menu.bar.find(&id).is_some_and(|n| n.kind != MenuNodeKind::Item);
                self.set_hot(h);
                if sub {
                    let path = self.cascade_path(&id);
                    self.set_open(path);
                } else {
                    let keep = if self.menu.open[0] == LAYERS_CONTEXT { 1 } else { self.cascade_path(&id).len().saturating_sub(1) };
                    if self.menu.open.len() > keep && keep > 0 {
                        let path = self.menu.open[..keep].to_vec();
                        self.set_open(path);
                    }
                }
            }
            Some(MenuTarget::Remedy(_)) if in_popup => self.set_hot(h),
            _ => {}
        }
    }

    fn menu_receipt(&self) -> Json {
        let n = |v: u64| Json::Num(v);
        let st = self.menu.stats;
        let (nodes, available, greyed, inapplicable) = menu_tree::counts(&self.menu.bar);
        Json::obj(vec![
            ("open", Json::strs(&self.menu.open)),
            ("opens", n(st.opens)),
            ("runs", n(st.runs)),
            ("unavailable", n(st.unavailable)),
            ("remedies", n(st.remedies)),
            ("refused", n(st.refused)),
            ("evaluations", n(st.evaluations)),
            ("profile_growth", n(st.profile_growth)),
            ("tips", n(st.tips)),
            ("nodes", n(nodes as u64)),
            ("available", n(available as u64)),
            ("greyed", n(greyed as u64)),
            ("inapplicable", n(inapplicable as u64)),
            ("profile_signature", Json::strs(&self.menu.profile.signature())),
            ("named_shape", self.menu.profile.named_shape().map_or(Json::Null, Json::str)),
        ])
    }

    // ---- tooltips (STU-SHL-230 to 235) ----

    /// Advances the host's clock by `ms` (a window frame clock; no script step calls it, so an agent-driven session
    /// never sees a dwell tip, STU-SHL-235 rule 5): L0 after 400 ms on the hovered control, L1 after 900 ms or at once
    /// when a tip was showing on the control the pointer came from; none while a button is held, for 500 ms after a
    /// release, on the control just operated, with tips off, or above the level cap. The tip stays while the pointer is
    /// on it (its remedy button is reachable).
    pub fn advance(&mut self, ms: u64) {
        self.menu.clock_ms = self.menu.clock_ms.saturating_add(ms);
        let hovered = self.ui.hovered.clone();
        if hovered.as_deref().is_some_and(|h| self.panels[TIP].tree().find(h).is_some()) {
            return;
        }
        if hovered != self.menu.dwell_target {
            self.dwell_reset();
        } else {
            self.menu.dwell_ms = self.menu.dwell_ms.saturating_add(ms);
        }
        let Some(target) = hovered else { return };
        if self.press.is_some() || self.menu.clock_ms < self.menu.quiet_until || self.menu.tips_off || self.menu.operated.as_deref() == Some(target.as_str()) {
            return;
        }
        let level = if self.menu.warm || self.menu.dwell_ms >= L1_DWELL_MS {
            1
        } else if self.menu.dwell_ms >= L0_DWELL_MS {
            0
        } else {
            return;
        };
        let level = self.menu.tip_cap.map_or(level, |c| level.min(c));
        let want = TipOn { target, level, explicit: false };
        if self.menu.tip.as_ref() == Some(&want) || self.tip_record(&want.target, level).is_none() {
            return;
        }
        self.menu.tip = Some(want);
        self.menu.stats.tips = self.menu.stats.tips.saturating_add(1);
        self.bump(&[TIP]);
    }

    /// The dwell starts again when the hovered control changes (a pointer move): a tip showing on the control left
    /// warms the next one (L1 at once), the operated mark clears, a dwell tip hides. The tip itself keeps its control.
    fn dwell_reset(&mut self) {
        let hovered = self.ui.hovered.clone();
        if hovered == self.menu.dwell_target || hovered.as_deref().is_some_and(|h| self.panels[TIP].tree().find(h).is_some()) {
            return;
        }
        self.menu.warm = self.menu.tip.as_ref().is_some_and(|t| !t.explicit);
        self.menu.dwell_target = hovered.clone();
        self.menu.dwell_ms = 0;
        if self.menu.operated.is_some() && self.menu.operated != hovered {
            self.menu.operated = None;
        }
        self.hide_dwell_tip();
    }

    /// The accessibility request for a tooltip (AC-REACH-05: "open L1 through the accessibility action"): shows the
    /// L0 (`level` 0) or L1 tip of `author_id` until hidden, also in an agent-driven session (it was asked for).
    /// `Err("unknown_author_id")` for an id with no labelled node.
    pub fn show_tip(&mut self, author_id: &str, level: u8) -> Result<TipRecord, &'static str> {
        let level = level.min(1);
        let r = self.tip_record(author_id, level).ok_or("unknown_author_id")?;
        self.menu.tip = Some(TipOn { target: author_id.to_owned(), level, explicit: true });
        self.menu.stats.tips = self.menu.stats.tips.saturating_add(1);
        self.bump(&[TIP]);
        Ok(r)
    }

    /// The tooltip shown, if any.
    pub fn tip(&self) -> Option<TipRecord> {
        self.menu.tip.as_ref().and_then(|t| self.tip_record(&t.target, t.level))
    }

    pub fn hide_tip(&mut self) {
        if self.menu.tip.take().is_some() {
            self.bump(&[TIP]);
        }
    }

    fn hide_dwell_tip(&mut self) {
        if self.menu.tip.as_ref().is_some_and(|t| !t.explicit) {
            self.hide_tip();
        }
    }

    /// The global tips-off preference and a per-level cap (STU-SHL-235 rule 4): `cap` Some(0) shows L0 only.
    pub fn set_tips(&mut self, off: bool, cap: Option<u8>) {
        self.menu.tips_off = off;
        self.menu.tip_cap = cap;
        if off {
            self.hide_dwell_tip();
        }
    }

    /// The tip of `target` at `level` from the element's own record: a menu node's label, its chord resolved now, its
    /// help sentence, its reason and remedy; any other control's label (and, for an unbuilt one, that it is not built).
    fn tip_record(&self, target: &str, level: u8) -> Option<TipRecord> {
        let node = self.node(target)?;
        let name = node.label.clone().filter(|l| !l.trim().is_empty())?;
        let mut r = TipRecord { target: target.to_owned(), level, name, chord: None, summary: None, reason: None, remedy: None, remedy_label: None, value: node.value.clone() };
        if let Some(n) = self.menu.authors.get(target).and_then(|id| self.menu.bar.find(id)) {
            let a = &n.availability;
            r.chord = menu_tree::binding(&n.id).and_then(|b| preset::chord_of(b.op.command_id())).map(str::to_owned);
            r.value = (!a.is_available()).then(|| a.reason_line());
            if level > 0 {
                r.summary = (!n.help.is_empty()).then(|| n.help.clone());
                r.reason = (!a.is_available()).then(|| a.reason_text.clone().unwrap_or_default());
                r.remedy = a.remedy_command_id.clone();
                r.remedy_label = r.remedy.as_deref().and_then(|x| self.menu.bar.find(x)).map(|x| x.label.clone());
            }
        } else if level > 0 && node.value.as_deref().is_some_and(|v| v.starts_with(chrome::NIY)) {
            r.reason = Some(chrome::NOT_BUILT.to_owned());
        }
        Some(r)
    }

    /// The shown tip laid out: L0 one line 24 high, L1 one line per part (at most 320 wide), below its control by 8,
    /// above it when there is no room below (never over its control, STU-SHL-235 rule 3), inside the window.
    fn tip_now(&mut self) -> Option<chrome::TipFacts> {
        use kumihimo_ui::widgets::popup;
        let on = self.menu.tip.clone()?;
        let rec = self.tip_record(&on.target, on.level)?;
        let b = self.node(&on.target)?.bounds;
        let key = format!("{rec:?}|{b:?}|{:?}|{}|{}", self.extent, self.scale.get(), self.text.epoch());
        if let Some((k, f)) = &self.menu.tip_facts
            && *k == key
        {
            return Some(f.clone());
        }
        let l1 = on.level > 0;
        let remedy = rec.remedy.as_ref().map(|r| format!("-> {}", rec.remedy_label.clone().unwrap_or_else(|| r.clone())));
        let t = &mut self.text;
        let (px, py) = if l1 { (popup::TIP_L1_PAD_X, popup::TIP_L1_PAD_Y) } else { (popup::TIP_PAD_X, 0.0) };
        let chord_w = rec.chord.as_deref().map_or(0.0, |c| kumihimo_ui::tokens::spacing::S4 + t.measure(c, TextToken::Caption, 0));
        let mut w = t.measure(&rec.name, TextToken::Body, 0) + chord_w;
        let parts: Vec<&String> = if l1 { [&rec.summary, &rec.reason, &remedy].into_iter().flatten().collect() } else { Vec::new() };
        for s in &parts {
            w = w.max(t.measure(s, TextToken::Body, 0));
        }
        let w = (w + 2.0 * px).ceil().min(size::TOOLTIP_MAX_W);
        let h = if l1 { 2.0 * py + (1 + parts.len()) as f32 * TextToken::Body.style().line } else { popup::TIP_L0_H };
        let win = self.layout.window;
        let below = b.y1 + size::TOOLTIP_OFFSET;
        let y0 = if below + h <= win.y1 { below } else { (b.y0 - size::TOOLTIP_OFFSET - h).max(win.y0) };
        let x0 = b.x0.min(win.x1 - w).max(win.x0);
        let f = chrome::TipFacts {
            rect: Rect::xywh(x0, y0, w, h),
            level: on.level,
            name: rec.name,
            chord: rec.chord,
            summary: rec.summary.filter(|_| l1),
            reason: rec.reason.filter(|_| l1),
            remedy: remedy.filter(|_| l1),
            value: rec.value,
        };
        self.menu.tip_facts = Some((key, f.clone()));
        Some(f)
    }
}


/// The dock groups the frame knows: the three Inspect Dock groups, then the Browse Dock's one group (FIT-L1).
const GROUPS: usize = 4;
fn group_tabs(gi: usize) -> &'static [(&'static str, &'static str)] {
    if gi < 3 { preset::RIGHT[gi].tabs } else { &preset::LEFT_TABS }
}

/// The status readouts' texts in [`chrome::READOUTS`] order: size, zoom, revision, renderer state, device.
/// The renderer state names the chrome target; in Vulkan mode the device readout also ends with `; chrome vulkan`
/// (inside the cut), so CPU frames keep their bytes (AC-BEHAV-07).
fn status_texts(device: &str, backend: &str, size_text: &str, revision: u64, chrome: ChromeMode) -> [String; 5] {
    let suffix = if chrome == ChromeMode::Vulkan { "; chrome vulkan" } else { "" };
    [
        size_text.to_owned(),
        "100%".to_owned(),
        format!("revision {revision}"),
        chrome.code().to_owned(),
        cut(&format!("{device}; viewer render-{backend}{suffix}"), MAX_DEVICE_BYTES).to_owned(),
    ]
}

/// The last outcome in words: an unbuilt control by its name, others by command and code.
fn outcome_words(last: Option<&Outcome>, label: Option<&str>) -> String {
    match (last, label) {
        (None, _) => "no command yet".to_owned(),
        (Some(o), Some(l)) if o.command == "frame.static" => format!("{l}: not built in this host yet"),
        (Some(o), Some(l)) if o.command == "frame.menu_unavailable" => l.to_owned(),
        (Some(o), _) if o.code == "ok" => format!("{} ok", o.command),
        (Some(o), _) if o.command.starts_with("frame.") => format!("{} {}", o.command, o.code),
        (Some(o), _) => format!("{} refused {}", o.command, o.code),
    }
}

/// The document tab's horizontal padding (components.json document_tab pad_l 12, pad_r 6).
fn widgets_pad_doc_tab() -> f32 {
    kumihimo_ui::widgets::component::DOC_TAB_PAD_L + kumihimo_ui::widgets::component::DOC_TAB_PAD_R
}

/// [`drive`] for Vulkan mode: after each step, draws what changed through `target`. Stops at the first refusal.
pub fn drive_chrome(frame: &mut GuiFrame, target: &mut dyn ChromeTarget, steps: &[Step]) -> Result<(), HostError> {
    frame.frame()?;
    frame.draw(target)?;
    for s in steps {
        frame.step(s)?;
        frame.frame()?;
        frame.draw(target)?;
    }
    Ok(())
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
