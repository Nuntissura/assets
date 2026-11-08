//! The frame layout (SMB-GUI-FRAME-1 item 5): one pure function from (client extent, scale, preset edge row) to every
//! region rect and the fit decisions of layouts.json fit_rules, used alike by paint, hit routing, the tree,
//! studio.dock.state and the receipt. CPU only, no winit or Vulkan type.
use kumihimo_ui::geom::{Rect, Scale};
use kumihimo_ui::tokens::size;

// layouts.json fit_rules.thresholds (design data, not tokens) and the bottom dock minimum of FIT-H1.
/// centre_min_w_per_viewer.
pub const CENTRE_MIN_W: f32 = 360.0;
/// centre_min_h.
pub const CENTRE_MIN_H: f32 = 240.0;
/// group_min_h: a group body under this collapses to its header (FIT-H2).
pub const GROUP_MIN_H: f32 = 90.0;
/// width_class_compact_below (FIT-W3).
pub const COMPACT_BELOW: f32 = 1500.0;
/// width_class_wide_from (FIT-L1).
pub const WIDE_FROM: f32 = 2400.0;
/// FIT-H1: the bottom dock's minimum without a time panel.
pub const BOTTOM_MIN_H: f32 = 180.0;

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
    /// The leaf regions that partition the window (AC-GEOM-01), named, top to bottom and left to right: the bars,
    /// the Tool Rail, the left edge, the centre, the right groups and foot (or the whole right edge as a rail), the
    /// Meter Rail, the bottom dock, the status bar.
    pub fn regions(&self) -> Vec<(&'static str, Rect)> {
        let mut v = vec![("menu_bar", self.menu_bar), ("tab_strip", self.tab_strip), ("context_bar", self.context_bar), ("tool_rail", self.tool_rail), ("left", self.left), ("centre", self.centre)];
        if self.groups.is_empty() {
            v.push(("right", self.right));
        } else {
            v.extend(self.groups.iter().map(|g| ("right_group", *g)));
            v.push(("right_foot", self.right_foot));
        }
        v.extend([("meter", self.meter), ("bottom", self.bottom), ("status", self.status)]);
        v
    }
}

/// The Tool Rail's drawn foot: pad 2 above the slots, family browse 32, gap 4, colour proxy 36, bottom 6
/// (tools/mock_lib.py toolrail; components.json tool_rail pad_top 2).
pub const RAIL_FOOT: f32 = 2.0 + 32.0 + 4.0 + 36.0 + 6.0;

/// FIT-R1 and FIT-R2 over a rail `main_h` tall with `slots` slots: (pitch, shown, hidden). Shown counts navigate; a
/// chevron slot above it holds the hidden ones.
pub fn rail_fit(main_h: f32, slots: usize) -> (f32, usize, usize) {
    let avail = (main_h - RAIL_FOOT).max(0.0);
    let fits = |pitch: f32| slots as f32 * pitch <= avail;
    if fits(size::RAIL_TOOL_SLOT_H) {
        return (size::RAIL_TOOL_SLOT_H, slots, 0);
    }
    let pitch = size::RAIL_TOOL_SLOT_H_COMPACT;
    if fits(pitch) {
        return (pitch, slots, 0);
    }
    // Room for `cap` slots of which one is the chevron.
    let cap = (avail / pitch).floor() as usize;
    let shown = cap.saturating_sub(1).min(slots);
    (pitch, shown, slots - shown)
}

/// Lays the frame out (layouts.json frame.note): bars from tokens alone (so AC-CONS-04 holds by construction); in the
/// main area the Tool Rail 44 at left over the full main height, the Meter Rail 36 at right when shown, the bottom dock
/// between the rails, and the left dock, Viewport and right dock above it; edges by the preset under the width ladder
/// (FIT-L1, FIT-W1 to W5), the height ladder (FIT-H1 to H3) and the rail ladder (FIT-R1, FIT-R2); right groups by
/// share, the last group taking the remainder (exact partition), FIT-H2 collapsing a group whose body is under 90 to
/// its header with the largest group kept open. Below the ladder's floor every edge clamps inside the window: nothing
/// goes negative and the partition still holds.
pub fn layout(extent: (u32, u32), scale: Scale, edges: &Edges) -> FrameLayout {
    let s = scale.get();
    let (w, h) = (extent.0 as f32 / s, extent.1 as f32 / s);
    let mut applied: Vec<&'static str> = Vec::new();
    // Width ladder (gen_layouts.py fit, the designer's own ladder code).
    let mut left_state = edges.left;
    let mut left_w = edges.left_w;
    if left_state == EdgeState::Rail && w >= WIDE_FROM {
        left_state = EdgeState::Expanded;
        applied.push("FIT-L1");
    }
    let mut right_state = edges.right;
    let mut right_w = edges.right_w;
    let meter = if edges.meter { size::RAIL_METER_W } else { 0.0 };
    let mut need_w = edges.viewers.max(1) as f32 * CENTRE_MIN_W;
    let lw = |st: EdgeState, v: f32| if st == EdgeState::Expanded { v } else { size::RAIL_ICON_W };
    let cw = |ls: EdgeState, l: f32, rs: EdgeState, r: f32| w - size::RAIL_TOOL_W - meter - lw(ls, l) - lw(rs, r);
    if left_state == EdgeState::Expanded && meter > 0.0 && w < COMPACT_BELOW {
        left_state = EdgeState::Rail;
        applied.push("FIT-W3");
    }
    if cw(left_state, left_w, right_state, right_w) < need_w && left_state == EdgeState::Expanded && left_w > size::DOCK_BROWSE_MIN_W {
        left_w = size::DOCK_BROWSE_MIN_W;
        applied.push("FIT-W1");
    }
    if cw(left_state, left_w, right_state, right_w) < need_w && right_state == EdgeState::Expanded && right_w > size::DOCK_INSPECT_MIN_W {
        right_w = size::DOCK_INSPECT_MIN_W;
        applied.push("FIT-W2");
    }
    if cw(left_state, left_w, right_state, right_w) < need_w && left_state == EdgeState::Expanded {
        left_state = EdgeState::Rail;
        applied.push("FIT-W3");
    }
    let mut viewers_as_tabs = false;
    if cw(left_state, left_w, right_state, right_w) < need_w && edges.viewers > 1 {
        viewers_as_tabs = true;
        need_w = CENTRE_MIN_W;
        applied.push("FIT-W4");
    }
    if cw(left_state, left_w, right_state, right_w) < need_w && right_state == EdgeState::Expanded {
        right_state = EdgeState::Rail;
        applied.push("FIT-W5");
    }
    // Height ladder.
    let bars = size::FRAME_MENU_BAR_H + size::FRAME_TAB_STRIP_H + size::FRAME_CONTEXT_BAR_H + size::FRAME_STATUS_BAR_H;
    let main_h = h - bars;
    let time = edges.time_panel && edges.bottom == EdgeState::Expanded;
    let bmin = if time { size::DOCK_BOTTOM_TIME_MIN_H } else { BOTTOM_MIN_H };
    let mut bottom_state = edges.bottom;
    let mut bottom_h = edges.bottom_h;
    let bh = |st: EdgeState, v: f32| if st == EdgeState::Expanded { v } else { size::RAIL_BOTTOM_H };
    if main_h - bh(bottom_state, bottom_h) < CENTRE_MIN_H && bottom_state == EdgeState::Expanded && bottom_h > bmin {
        bottom_h = bmin;
        applied.push("FIT-H1");
    }
    if main_h - bh(bottom_state, bottom_h) < CENTRE_MIN_H && bottom_state == EdgeState::Expanded && !time {
        bottom_state = EdgeState::Rail;
        applied.push("FIT-H3");
    }
    // Edges, clamped into the window from the left and the top.
    let x_rail = size::RAIL_TOOL_W.min(w);
    let x_left = (x_rail + lw(left_state, left_w)).min(w);
    let x_right = (w - meter - lw(right_state, right_w)).clamp(x_left, w);
    let x_meter = (w - meter).clamp(x_right, w);
    let y_menu = size::FRAME_MENU_BAR_H.min(h);
    let y_tabs = (y_menu + size::FRAME_TAB_STRIP_H).min(h);
    let y_ctx = (y_tabs + size::FRAME_CONTEXT_BAR_H).min(h);
    let y_status = (h - size::FRAME_STATUS_BAR_H).clamp(y_ctx, h);
    let y_bottom = (y_status - bh(bottom_state, bottom_h)).clamp(y_ctx, y_status);
    let right = Rect::new(x_right, y_ctx, x_meter, y_bottom);
    // Right groups.
    let mut groups = Vec::new();
    let mut collapsed = Vec::new();
    let mut right_foot = Rect::new(right.x0, right.y1, right.x1, right.y1);
    if right_state == EdgeState::Expanded && !edges.shares.is_empty() {
        let foot = if edges.right_foot { size::DOCK_FOOT_H.min(right.h()) } else { 0.0 };
        let avail = right.h() - foot;
        right_foot = Rect::new(right.x0, right.y1 - foot, right.x1, right.y1);
        let n = edges.shares.len();
        let mut hs: Vec<f32> = edges.shares.iter().map(|sh| (avail * sh).round()).collect();
        let head: f32 = hs[..n - 1].iter().sum();
        hs[n - 1] = avail - head;
        let header = size::DOCK_GROUP_HEADER_H;
        let mut shut: Vec<bool> = hs.iter().map(|g| g - header < GROUP_MIN_H).collect();
        if shut.iter().all(|c| *c) {
            // FIT-H2 never closes every group: the largest share stays open.
            let keep = edges.shares.iter().enumerate().fold(0, |k, (i, sh)| if *sh > edges.shares[k] { i } else { k });
            shut[keep] = false;
        }
        if shut.iter().any(|c| *c) {
            applied.push("FIT-H2");
            let free: f32 = hs.iter().zip(&shut).filter(|(_, c)| **c).map(|(g, _)| g - header).sum();
            let open: Vec<usize> = (0..n).filter(|i| !shut[*i]).collect();
            let each = (free / open.len() as f32).round();
            for i in 0..n {
                hs[i] = if shut[i] { header } else { hs[i] + each };
            }
            // The last open group takes the remainder, so the groups partition the dock exactly.
            let last = *open.last().unwrap_or(&(n - 1));
            let others: f32 = (0..n).filter(|i| *i != last).map(|i| hs[i]).sum();
            hs[last] = avail - others;
        }
        // Clamp from the top so a too-short dock never yields a negative group.
        let mut y = right.y0;
        let end = right.y0 + avail.max(0.0);
        for (i, g) in hs.iter().enumerate() {
            let y1 = if i == n - 1 { end } else { (y + g).min(end) };
            groups.push(Rect::new(right.x0, y, right.x1, y1.max(y)));
            y = y1.max(y);
        }
        collapsed = shut;
    }
    let centre = Rect::new(x_left, y_ctx, x_right, y_bottom);
    let t = size::RULER_THICKNESS;
    let cx = (x_left + t).min(x_right);
    let cy = (y_ctx + t).min(y_bottom);
    let (rail_pitch, rail_shown, rail_hidden) = rail_fit(y_status - y_ctx, edges.slots);
    if rail_pitch < size::RAIL_TOOL_SLOT_H {
        applied.push("FIT-R1");
    }
    if rail_hidden > 0 {
        applied.push("FIT-R2");
    }
    applied.sort_unstable();
    applied.dedup();
    FrameLayout {
        extent,
        scale,
        logical: (w, h),
        window: Rect::new(0.0, 0.0, w, h),
        menu_bar: Rect::new(0.0, 0.0, w, y_menu),
        tab_strip: Rect::new(0.0, y_menu, w, y_tabs),
        context_bar: Rect::new(0.0, y_tabs, w, y_ctx),
        tool_rail: Rect::new(0.0, y_ctx, x_rail, y_status),
        left: Rect::new(x_rail, y_ctx, x_left, y_bottom),
        centre,
        right,
        meter: Rect::new(x_meter, y_ctx, w, y_status),
        bottom: Rect::new(x_rail, y_bottom, x_meter, y_status),
        status: Rect::new(0.0, y_status, w, h),
        groups,
        collapsed,
        right_foot,
        ruler_corner: Rect::new(x_left, y_ctx, cx, cy),
        ruler_x: Rect::new(cx, y_ctx, x_right, cy),
        ruler_y: Rect::new(x_left, cy, cx, y_bottom),
        canvas: Rect::new(cx, cy, x_right, y_bottom),
        left_state,
        right_state,
        bottom_state,
        rail_pitch,
        rail_shown,
        rail_hidden,
        viewers_as_tabs,
        applied,
        below_min: w < size::WINDOW_MIN_W || h < size::WINDOW_MIN_H,
    }
}
