//! The frame layout (SMB-GUI-FRAME-1 item 5): one pure function from (client extent, scale, preset edge row) to every
//! region rect and the fit decisions of layouts.json fit_rules, used alike by paint, hit routing, the tree,
//! studio.dock.state and the receipt. CPU only, no winit or Vulkan type.
use kumihimo_ui::geom::{Rect, Scale};

/// An edge's state (fit_table words).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeState {
    Rail,
    Expanded,
}
impl EdgeState {
    pub fn word(self) -> &'static str {
        match self {
            EdgeState::Rail => "rail",
            EdgeState::Expanded => "expanded",
        }
    }
}

/// A preset's edge row (layouts.json presets: left, right, right_rail, bottom, rail).
#[derive(Clone, Debug, PartialEq)]
pub struct Edges {
    pub left: EdgeState,
    pub left_w: f32,
    pub right: EdgeState,
    pub right_w: f32,
    /// Right group shares, top first.
    pub shares: Vec<f32>,
    /// A registered-panel foot (28) under the right groups.
    pub right_foot: bool,
    /// The Meter Rail (36) at the far right.
    pub meter: bool,
    pub bottom: EdgeState,
    pub bottom_h: f32,
    /// The bottom dock holds a time panel (its minimum is then 300, STU-SHL-091).
    pub time_panel: bool,
    pub viewers: u32,
    /// Tool Rail slots.
    pub slots: usize,
}

/// Every region rect in logical px and the fit decisions.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLayout {
    pub extent: (u32, u32),
    pub scale: Scale,
    pub logical: (f32, f32),
    pub window: Rect,
    pub menu_bar: Rect,
    pub tab_strip: Rect,
    pub context_bar: Rect,
    pub tool_rail: Rect,
    pub left: Rect,
    pub centre: Rect,
    pub right: Rect,
    pub meter: Rect,
    pub bottom: Rect,
    pub status: Rect,
    /// The right dock's group rects (top first) and its foot.
    pub groups: Vec<Rect>,
    pub collapsed: Vec<bool>,
    pub right_foot: Rect,
    /// Inside the centre: the 20 x 20 ruler corner, the rulers, the canvas area under them.
    pub ruler_corner: Rect,
    pub ruler_x: Rect,
    pub ruler_y: Rect,
    pub canvas: Rect,
    pub left_state: EdgeState,
    pub right_state: EdgeState,
    pub bottom_state: EdgeState,
    pub rail_pitch: f32,
    /// Rail slots shown (navigate included) and hidden behind the FIT-R2 chevron.
    pub rail_shown: usize,
    pub rail_hidden: usize,
    pub viewers_as_tabs: bool,
    /// The fit rules that fired, sorted.
    pub applied: Vec<&'static str>,
    /// The logical client is under size::WINDOW_MIN_W x WINDOW_MIN_H (`below_min_size`).
    pub below_min: bool,
}

impl FrameLayout {
    /// The leaf regions that partition the window (AC-GEOM-01), named.
    pub fn regions(&self) -> Vec<(&'static str, Rect)> {
        Vec::new()
    }
}

/// Lays the frame out.
pub fn layout(extent: (u32, u32), scale: Scale, edges: &Edges) -> FrameLayout {
    let _ = edges;
    FrameLayout {
        extent,
        scale,
        logical: (0.0, 0.0),
        window: Rect::default(),
        menu_bar: Rect::default(),
        tab_strip: Rect::default(),
        context_bar: Rect::default(),
        tool_rail: Rect::default(),
        left: Rect::default(),
        centre: Rect::default(),
        right: Rect::default(),
        meter: Rect::default(),
        bottom: Rect::default(),
        status: Rect::default(),
        groups: Vec::new(),
        collapsed: Vec::new(),
        right_foot: Rect::default(),
        ruler_corner: Rect::default(),
        ruler_x: Rect::default(),
        ruler_y: Rect::default(),
        canvas: Rect::default(),
        left_state: EdgeState::Rail,
        right_state: EdgeState::Rail,
        bottom_state: EdgeState::Rail,
        rail_pitch: 0.0,
        rail_shown: 0,
        rail_hidden: 0,
        viewers_as_tabs: false,
        applied: Vec::new(),
        below_min: false,
    }
}
