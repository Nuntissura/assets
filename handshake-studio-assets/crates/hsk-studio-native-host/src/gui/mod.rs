//! Feature `document`: the GUI frame. A real document in the Viewport, a right dock with the kumihimo-ui Layers
//! panel, a status bar. RED skeleton: every entry point refuses with `not_built`.
use crate::capture::Json;
use crate::error::HostError;
use crate::event::HostEvent;
use crate::script::Step;
use hsk_studio_harness_view::view::{LayerRow, Viewer};
use hsk_studio_vulkan::{GpuClientPort, PaneId, Refusal};
use kumihimo_ui::geom::{Rect, Scale};
use kumihimo_ui::paint::Canvas;
use kumihimo_ui::semantic::Semantic;
use kumihimo_ui::text::TextStats;
use kumihimo_ui::tree::{Node, Panel};
use std::path::Path;

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

/// Lays the frame out at a physical client extent and scale.
pub fn regions(width: u32, height: u32, scale: Scale) -> Regions {
    let _ = (width, height, scale);
    Regions::default()
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

pub struct GuiFrame {
    _viewer: Viewer,
}

impl GuiFrame {
    pub fn open(path: &Path, width: u32, height: u32, scale_milli: Option<u32>) -> Result<Self, HostError> {
        let _ = (path, width, height, scale_milli);
        Err(HostError::Document("not_built"))
    }
    pub fn extent(&self) -> (u32, u32) {
        (0, 0)
    }
    pub fn scale(&self) -> Scale {
        Scale::ONE
    }
    pub fn regions(&self) -> Regions {
        Regions::default()
    }
    pub fn rows(&self) -> &[LayerRow] {
        &[]
    }
    pub fn selected(&self) -> Option<&str> {
        None
    }
    pub fn revision(&self) -> u64 {
        0
    }
    pub fn stats(&self) -> GuiStats {
        GuiStats::default()
    }
    pub fn last(&self) -> Option<&Outcome> {
        None
    }
    pub fn notes(&self) -> &[&'static str] {
        &[]
    }
    pub fn text_kind(&self) -> &'static str {
        "none"
    }
    pub fn text_stats(&self) -> TextStats {
        TextStats::default()
    }
    pub fn viewer(&self) -> &Viewer {
        &self._viewer
    }
    /// Runs `f` on the viewer, then re-reads the document as after a command.
    pub fn edit(&mut self, f: impl FnOnce(&mut Viewer)) {
        f(&mut self._viewer);
    }
    pub fn panels(&self) -> [&Panel; 3] {
        unimplemented_panels()
    }
    pub fn node(&self, author_id: &str) -> Option<&Node> {
        let _ = author_id;
        None
    }
    /// Centre of a node in physical client px.
    pub fn centre(&self, author_id: &str) -> Option<(i32, i32)> {
        let _ = author_id;
        None
    }
    /// Where the document image sits, logical px.
    pub fn image_rect(&self) -> Option<Rect> {
        None
    }
    pub fn set_device(&mut self, readout: String) {
        let _ = readout;
    }
    pub fn apply(&mut self, ev: HostEvent) -> Option<Outcome> {
        let _ = ev;
        None
    }
    pub fn invoke(&mut self, author_id: &str) -> Result<Option<Outcome>, &'static str> {
        let _ = author_id;
        Err("not_built")
    }
    pub fn step(&mut self, step: &Step) -> Result<Option<Outcome>, HostError> {
        let _ = step;
        Err(HostError::Document("not_built"))
    }
    pub fn frame(&mut self) -> Result<bool, HostError> {
        Err(HostError::Document("not_built"))
    }
    pub fn upload(&mut self, sink: &mut dyn PaneSink) -> Result<bool, Refusal> {
        let _ = sink;
        Ok(false)
    }
    pub fn mark_upload(&mut self) {}
    pub fn canvas(&self) -> Option<&Canvas> {
        None
    }
    pub fn rgba(&self) -> Option<&[u8]> {
        None
    }
    pub fn semantic(&self) -> Semantic {
        Semantic { roots: Vec::new(), nodes: 0, truncated: false }
    }
    pub fn tree_json(&self) -> String {
        String::new()
    }
    pub fn tree_file(&self) -> String {
        String::new()
    }
    pub fn receipt(&self) -> Json {
        Json::Null
    }
    /// AC-TEXT-01 over the three panels.
    pub fn clipped(&mut self) -> Vec<kumihimo_ui::checks::Clipped> {
        Vec::new()
    }
}

fn unimplemented_panels() -> [&'static Panel; 3] {
    static EMPTY: std::sync::OnceLock<[Panel; 3]> = std::sync::OnceLock::new();
    let p = EMPTY.get_or_init(Default::default);
    [&p[0], &p[1], &p[2]]
}

/// Applies `steps` in order; after each, paints and uploads what changed. Stops at the first refusal of the sink.
pub fn drive(frame: &mut GuiFrame, sink: &mut dyn PaneSink, steps: &[Step]) -> Result<(), HostError> {
    let _ = (frame, sink, steps);
    Err(HostError::Document("not_built"))
}
