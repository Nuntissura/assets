//! Auto layout (STU-DS-036..045, 144..149): a deterministic flexbox-style subset evaluated in f64
//! points on crate-local node values. Horizontal and vertical flow, wrap, gaps (negative allowed),
//! per-side padding, primary/counter alignment, fixed/hug/fill sizing with min/max clamps,
//! absolutely positioned children (placed by constraints, STU-DS-048) and arbitrary nesting.
//!
//! Not adopted: `taffy` (0.14.0, MIT per docs.rs; pure-Rust status, f32 vs f64 geometry and negative
//! gap handling were NOT stated by the page read and stay UNVERIFIED). The spec contract differs
//! from CSS flex in checkable ways (container AUTO/FIXED modes plus per-child HUG/FILL/FIXED that
//! must agree, grow limited to 0|1, SPACE_BETWEEN as a primary-align mode, negative gaps legal,
//! absolute children falling back to constraints), and a hand-checkable f64 evaluator keeps every
//! deviation explicit instead of hidden inside a CSS engine.
//!
//! Decisions (spec silent; recorded, not source-derived):
//! * Grow children share remaining primary space equally (flex-basis 0 style) with min/max clamp
//!   redistribution; in a hugging primary axis they keep their content size. Relation to Figma
//!   UNVERIFIED.
//! * Overflow never produces negative offsets: centre/max alignment of an overflowing line packs to
//!   the start. Children are never shrunk.
//! * `SPACE_BETWEEN` gap = max(0, free space / (n - 1)); one item packs to the start.
//! * Sizing is stated twice in the spec (container AUTO/FIXED vs `layout_sizing_*`, child
//!   `layout_grow`/`layout_align` vs FILL). Both must agree; disagreement is `InconsistentSizing`.
//! * A hugging leaf (no auto layout) uses its authored width/height as intrinsic content size; text
//!   measurement belongs to Layout/Type and is not done here.
//! * Typed refusals (`UnsupportedLayout`): GRID, BASELINE, `strokes_included_in_layout` (stroke
//!   accounting unspecified), SPACE_BETWEEN or WRAP with a hugging primary axis, FILL on the primary
//!   axis in a wrapping parent, wrapping counter-hug with a FILL primary, HUG on a plain frame.
use crate::constraints::{Constraints, RectPt, SizePt, apply_constraints};
use crate::instance_resolution::check_id;
use crate::{ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    Horizontal,
    Vertical,
    /// Recognised, refused (`UnsupportedLayout`).
    Grid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapMode {
    NoWrap,
    Wrap,
}
/// Container axis sizing (`primary_axis_sizing_mode`, `counter_axis_sizing_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisSizing {
    Fixed,
    Auto,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimaryAlign {
    Min,
    Center,
    Max,
    SpaceBetween,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CounterAlign {
    Min,
    Center,
    Max,
    /// Recognised, refused: needs text baselines from Layout/Type.
    Baseline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignContent {
    Auto,
    SpaceBetween,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildAlign {
    Min,
    Center,
    Max,
    Stretch,
    Inherit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildSizing {
    Fixed,
    Hug,
    Fill,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Positioning {
    Auto,
    Absolute,
}

/// STU-DS-144 container fields (grid fields excluded; `layout_mode = NONE` is `layout: None`).
#[derive(Clone, Debug, PartialEq)]
pub struct AutoLayout {
    pub mode: LayoutMode,
    pub wrap: WrapMode,
    pub primary_axis_sizing: AxisSizing,
    pub counter_axis_sizing: AxisSizing,
    pub primary_align: PrimaryAlign,
    pub counter_align: CounterAlign,
    pub align_content: AlignContent,
    pub item_spacing: f64,
    /// `None` = use `item_spacing` between wrapped lines.
    pub counter_axis_spacing: Option<f64>,
    pub padding_top: f64,
    pub padding_right: f64,
    pub padding_bottom: f64,
    pub padding_left: f64,
    pub strokes_included_in_layout: bool,
    /// true = overlapping children render last-on-top; false = first-on-top (STU-DS-144).
    pub item_reverse_z_index: bool,
}
impl AutoLayout {
    /// Spec defaults (STU-DS-144) for the given flow direction.
    pub fn new(mode: LayoutMode) -> Self {
        Self {
            mode,
            wrap: WrapMode::NoWrap,
            primary_axis_sizing: AxisSizing::Auto,
            counter_axis_sizing: AxisSizing::Auto,
            primary_align: PrimaryAlign::Min,
            counter_align: CounterAlign::Min,
            align_content: AlignContent::Auto,
            item_spacing: 0.0,
            counter_axis_spacing: None,
            padding_top: 0.0,
            padding_right: 0.0,
            padding_bottom: 0.0,
            padding_left: 0.0,
            strokes_included_in_layout: false,
            item_reverse_z_index: false,
        }
    }
    pub fn horizontal() -> Self {
        Self::new(LayoutMode::Horizontal)
    }
    pub fn vertical() -> Self {
        Self::new(LayoutMode::Vertical)
    }
    /// Convenience writer for all four sides.
    pub fn with_padding(mut self, all: f64) -> Self {
        self.padding_top = all;
        self.padding_right = all;
        self.padding_bottom = all;
        self.padding_left = all;
        self
    }
}

/// STU-DS-149 clamps; `None` = unclamped.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SizeClamps {
    pub min_width: Option<f64>,
    pub max_width: Option<f64>,
    pub min_height: Option<f64>,
    pub max_height: Option<f64>,
}
/// Position and constraints of a child that is not placed by flow (absolute, or in a plain frame),
/// in the parent's local space at the parent's authored size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub constraints: Constraints,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutNode {
    /// `SLYR-*`
    pub node_id: String,
    /// Authored size: the size when fixed, the intrinsic content size of a hugging leaf, and the
    /// reference size for placing absolute children.
    pub width: f64,
    pub height: f64,
    pub sizing_horizontal: ChildSizing,
    pub sizing_vertical: ChildSizing,
    pub clamps: SizeClamps,
    /// `layout_grow`: exactly 0 or 1.
    pub grow: f64,
    pub align: ChildAlign,
    pub positioning: Positioning,
    pub placement: Option<Placement>,
    /// `None` = `layout_mode NONE`: children are placed by their constraints.
    pub layout: Option<AutoLayout>,
    pub children: Vec<LayoutNode>,
}
impl LayoutNode {
    pub fn new(node_id: &str, width: f64, height: f64) -> Self {
        Self {
            node_id: node_id.to_owned(),
            width,
            height,
            sizing_horizontal: ChildSizing::Fixed,
            sizing_vertical: ChildSizing::Fixed,
            clamps: SizeClamps::default(),
            grow: 0.0,
            align: ChildAlign::Inherit,
            positioning: Positioning::Auto,
            placement: None,
            layout: None,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
}
impl Default for LayoutLimits {
    fn default() -> Self {
        Self {
            max_nodes: 4096,
            max_depth: 32,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlacedBox {
    pub node_id: String,
    pub parent_id: Option<String>,
    /// Parent-local; the root is at (0, 0).
    pub rect: RectPt,
    /// 0 = painted first (bottom) among siblings.
    pub paint_rank: usize,
    /// false for absolute children and children of plain frames.
    pub in_flow: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AutoLayoutResult {
    /// Pre-order, children in child order.
    pub boxes: Vec<PlacedBox>,
}
impl AutoLayoutResult {
    pub fn get(&self, node_id: &str) -> Option<&PlacedBox> {
        self.boxes.iter().find(|b| b.node_id == node_id)
    }
}

// ----------------------------------------------------------------------------------------------
// Validation

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Root,
    InFlow { wrap: bool, parent_horizontal: bool },
    Placed,
}

fn in_range(v: f64, lo: f64, hi: f64, what: &str) -> R<()> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(E::InvalidLayout(what.to_owned()))
    }
}
fn axis_sizing_ok(s: ChildSizing, a: AxisSizing) -> bool {
    matches!(
        (s, a),
        (ChildSizing::Hug, AxisSizing::Auto)
            | (ChildSizing::Fixed | ChildSizing::Fill, AxisSizing::Fixed)
    )
}

fn validate(
    n: &LayoutNode,
    slot: Slot,
    depth: usize,
    ids: &mut BTreeSet<String>,
    limits: &LayoutLimits,
) -> R<()> {
    if depth > limits.max_depth || ids.len() >= limits.max_nodes {
        return Err(E::BudgetExceeded);
    }
    check_id(&n.node_id, "SLYR")?;
    if !ids.insert(n.node_id.clone()) {
        return Err(E::Duplicate(n.node_id.clone()));
    }
    let id = n.node_id.as_str();
    in_range(n.width, 0.0, 100_000.0, id)?;
    in_range(n.height, 0.0, 100_000.0, id)?;
    for (lo, hi) in [
        (n.clamps.min_width, n.clamps.max_width),
        (n.clamps.min_height, n.clamps.max_height),
    ] {
        for v in [lo, hi].into_iter().flatten() {
            in_range(v, 0.0, 100_000.0, id)?;
        }
        if let (Some(lo), Some(hi)) = (lo, hi)
            && lo > hi
        {
            return Err(E::InvalidLayout(format!("min_greater_than_max@{id}")));
        }
    }
    if n.grow != 0.0 && n.grow != 1.0 {
        return Err(E::InvalidLayout(format!("layout_grow@{id}")));
    }
    let (sh, sv) = (n.sizing_horizontal, n.sizing_vertical);
    match slot {
        Slot::Root if sh == ChildSizing::Fill || sv == ChildSizing::Fill => {
            return Err(E::InvalidLayout(format!("fill_on_root@{id}")));
        }
        Slot::Placed => {
            if sh != ChildSizing::Fixed || sv != ChildSizing::Fixed {
                return Err(E::InvalidLayout(format!("placed_child_not_fixed@{id}")));
            }
            if n.placement.is_none() {
                return Err(E::InvalidLayout(format!("placement_missing@{id}")));
            }
            if let Some(p) = n.placement
                && (!p.x.is_finite() || !p.y.is_finite())
            {
                return Err(E::InvalidLayout(format!("placement@{id}")));
            }
        }
        Slot::InFlow {
            wrap,
            parent_horizontal,
        } => {
            let (main, cross) = if parent_horizontal { (sh, sv) } else { (sv, sh) };
            if (main == ChildSizing::Fill) != (n.grow == 1.0)
                || (cross == ChildSizing::Fill) != (n.align == ChildAlign::Stretch)
            {
                return Err(E::InconsistentSizing(id.to_owned()));
            }
            if wrap && main == ChildSizing::Fill {
                return Err(E::UnsupportedLayout("fill_in_wrap".to_owned()));
            }
        }
        Slot::Root => {}
    }
    match &n.layout {
        None => {
            if !n.children.is_empty() && (sh == ChildSizing::Hug || sv == ChildSizing::Hug) {
                return Err(E::UnsupportedLayout("hug_without_auto_layout".to_owned()));
            }
            for c in &n.children {
                validate(c, Slot::Placed, depth + 1, ids, limits)?;
            }
        }
        Some(l) => {
            if l.mode == LayoutMode::Grid {
                return Err(E::UnsupportedLayout("grid".to_owned()));
            }
            if l.counter_align == CounterAlign::Baseline {
                return Err(E::UnsupportedLayout("baseline".to_owned()));
            }
            if l.strokes_included_in_layout {
                return Err(E::UnsupportedLayout("strokes_included_in_layout".to_owned()));
            }
            in_range(l.item_spacing, -10_000.0, 100_000.0, id)?;
            if let Some(v) = l.counter_axis_spacing {
                in_range(v, -10_000.0, 100_000.0, id)?;
            }
            for p in [l.padding_top, l.padding_right, l.padding_bottom, l.padding_left] {
                in_range(p, 0.0, 100_000.0, id)?;
            }
            let horizontal = l.mode == LayoutMode::Horizontal;
            let (sp, sc) = if horizontal { (sh, sv) } else { (sv, sh) };
            if !axis_sizing_ok(sp, l.primary_axis_sizing) || !axis_sizing_ok(sc, l.counter_axis_sizing)
            {
                return Err(E::InconsistentSizing(id.to_owned()));
            }
            let wrap = l.wrap == WrapMode::Wrap;
            if l.align_content == AlignContent::SpaceBetween && !wrap {
                return Err(E::InvalidLayout(format!("align_content_without_wrap@{id}")));
            }
            if l.primary_axis_sizing == AxisSizing::Auto
                && (wrap || l.primary_align == PrimaryAlign::SpaceBetween)
            {
                let what = if wrap { "wrap_with_hug_primary" } else { "space_between_with_hug" };
                return Err(E::UnsupportedLayout(what.to_owned()));
            }
            if wrap && l.counter_axis_sizing == AxisSizing::Auto && sp == ChildSizing::Fill {
                return Err(E::UnsupportedLayout("wrap_counter_hug_with_fill".to_owned()));
            }
            for c in &n.children {
                let child_slot = if c.positioning == Positioning::Absolute {
                    Slot::Placed
                } else {
                    Slot::InFlow {
                        wrap,
                        parent_horizontal: horizontal,
                    }
                };
                validate(c, child_slot, depth + 1, ids, limits)?;
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------------------------
// Measure and arrange

struct Ctx<'a> {
    cancel: &'a CancellationToken,
    boxes: Vec<PlacedBox>,
}
impl Ctx<'_> {
    fn tick(&self) -> R<()> {
        self.cancel.check().map_err(|_| E::Canceled)
    }
}

fn authored(n: &LayoutNode, horiz: bool) -> f64 {
    if horiz { n.width } else { n.height }
}
fn sizing(n: &LayoutNode, horiz: bool) -> ChildSizing {
    if horiz { n.sizing_horizontal } else { n.sizing_vertical }
}
fn clamp_axis(n: &LayoutNode, horiz: bool, v: f64) -> f64 {
    let (lo, hi) = if horiz {
        (n.clamps.min_width, n.clamps.max_width)
    } else {
        (n.clamps.min_height, n.clamps.max_height)
    };
    let v = lo.map_or(v, |m| v.max(m));
    hi.map_or(v, |m| v.min(m))
}
/// (main, cross) <-> (w, h) for a flow direction.
fn split(horizontal: bool, w: f64, h: f64) -> (f64, f64) {
    if horizontal { (w, h) } else { (h, w) }
}
fn pads(l: &AutoLayout) -> (f64, f64, f64, f64) {
    // (main_start, main_end, cross_start, cross_end) for the flow direction.
    if l.mode == LayoutMode::Horizontal {
        (l.padding_left, l.padding_right, l.padding_top, l.padding_bottom)
    } else {
        (l.padding_top, l.padding_bottom, l.padding_left, l.padding_right)
    }
}

/// Size along one axis when the node's sizing is HUG or FILL-as-content, or its clamped authored
/// size when FIXED.
fn base_size(n: &LayoutNode, horiz: bool, ctx: &Ctx) -> R<f64> {
    let v = match sizing(n, horiz) {
        ChildSizing::Fixed => authored(n, horiz),
        ChildSizing::Hug | ChildSizing::Fill => {
            let c = content_size(n, None, ctx)?;
            if horiz { c.w } else { c.h }
        }
    };
    Ok(clamp_axis(n, horiz, v))
}

/// (in-flow indices, main sizes, cross sizes, line cross size)
type PlannedLine = (Vec<usize>, Vec<f64>, Vec<f64>, f64);

struct Line {
    /// (index into the in-flow list, main size, cross size)
    items: Vec<(usize, f64, f64)>,
}
impl Line {
    fn cross(&self) -> f64 {
        self.items.iter().fold(0.0, |m, i| f64::max(m, i.2))
    }
}
fn plan_lines(inflow: &[&LayoutNode], l: &AutoLayout, avail_main: f64, ctx: &Ctx) -> R<Vec<Line>> {
    let horizontal = l.mode == LayoutMode::Horizontal;
    let mut lines: Vec<Line> = Vec::new();
    let mut cur = Line { items: Vec::new() };
    let mut len = 0.0;
    for (i, c) in inflow.iter().enumerate() {
        ctx.tick()?;
        let m = base_size(c, horizontal, ctx)?;
        let x = base_size(c, !horizontal, ctx)?;
        let next = if cur.items.is_empty() { m } else { len + l.item_spacing + m };
        if !cur.items.is_empty() && next > avail_main + 1e-9 {
            lines.push(std::mem::replace(&mut cur, Line { items: Vec::new() }));
            len = m;
        } else {
            len = next;
        }
        cur.items.push((i, m, x));
    }
    if !cur.items.is_empty() {
        lines.push(cur);
    }
    Ok(lines)
}

/// Size of the node if it hugs on both axes (leaf: authored size). `primary_outer` forces the
/// outer primary size of a wrapping container (root resize).
fn content_size(n: &LayoutNode, primary_outer: Option<f64>, ctx: &Ctx) -> R<SizePt> {
    ctx.tick()?;
    let Some(l) = &n.layout else {
        return Ok(SizePt { w: n.width, h: n.height });
    };
    let horizontal = l.mode == LayoutMode::Horizontal;
    let (ms, me, cs, ce) = pads(l);
    let inflow: Vec<&LayoutNode> = n
        .children
        .iter()
        .filter(|c| c.positioning == Positioning::Auto)
        .collect();
    let (main, cross) = if l.wrap == WrapMode::Wrap {
        let outer = clamp_axis(n, horizontal, primary_outer.unwrap_or(authored(n, horizontal)));
        let lines = plan_lines(&inflow, l, (outer - ms - me).max(0.0), ctx)?;
        let gap = l.counter_axis_spacing.unwrap_or(l.item_spacing);
        let total: f64 = lines.iter().map(Line::cross).sum();
        let between = lines.len().saturating_sub(1) as f64 * gap;
        (outer, total + between + cs + ce)
    } else {
        let mut main = ms + me;
        let mut cross = 0.0;
        for c in &inflow {
            main += base_size(c, horizontal, ctx)?;
            cross = f64::max(cross, base_size(c, !horizontal, ctx)?);
        }
        main += inflow.len().saturating_sub(1) as f64 * l.item_spacing;
        (main, cross + cs + ce)
    };
    let (w, h) = split(horizontal, main, cross);
    Ok(SizePt { w, h })
}

fn place_line(sizes: &[f64], avail: f64, align: PrimaryAlign, spacing: f64) -> Vec<f64> {
    let n = sizes.len();
    let total: f64 = sizes.iter().sum();
    let gaps = n.saturating_sub(1) as f64;
    let (start, gap) = match align {
        PrimaryAlign::SpaceBetween if n >= 2 => (0.0, ((avail - total) / gaps).max(0.0)),
        PrimaryAlign::SpaceBetween | PrimaryAlign::Min => (0.0, spacing),
        PrimaryAlign::Center => (((avail - total - spacing * gaps) / 2.0).max(0.0), spacing),
        PrimaryAlign::Max => ((avail - total - spacing * gaps).max(0.0), spacing),
    };
    let mut x = start;
    sizes
        .iter()
        .map(|s| {
            let at = x;
            x += s + gap;
            at
        })
        .collect()
}

fn arrange(
    n: &LayoutNode,
    rect: RectPt,
    parent_id: Option<&str>,
    rank: usize,
    in_flow: bool,
    ctx: &mut Ctx,
) -> R<()> {
    ctx.tick()?;
    ctx.boxes.push(PlacedBox {
        node_id: n.node_id.clone(),
        parent_id: parent_id.map(str::to_owned),
        rect,
        paint_rank: rank,
        in_flow,
    });
    let count = n.children.len();
    let mut flow_rects: Vec<Option<RectPt>> = vec![None; count];
    let mut reverse_z = true;
    if let Some(l) = &n.layout {
        reverse_z = l.item_reverse_z_index;
        let horizontal = l.mode == LayoutMode::Horizontal;
        let (ms, me, cs, ce) = pads(l);
        let (main_outer, cross_outer) = split(horizontal, rect.w, rect.h);
        let (inner_main, inner_cross) = (main_outer - ms - me, cross_outer - cs - ce);
        if inner_main < 0.0 || inner_cross < 0.0 {
            return Err(E::InvalidGeometry);
        }
        let idx: Vec<usize> = (0..count)
            .filter(|i| n.children[*i].positioning == Positioning::Auto)
            .collect();
        let inflow: Vec<&LayoutNode> = idx.iter().map(|i| &n.children[*i]).collect();
        // Lines: (child indices into `inflow`, final main sizes, final cross sizes, line cross size)
        let mut lines: Vec<PlannedLine> = Vec::new();
        if l.wrap == WrapMode::Wrap {
            for line in plan_lines(&inflow, l, inner_main, ctx)? {
                let line_cross = line.cross();
                let who = line.items.iter().map(|i| i.0).collect();
                let mains = line.items.iter().map(|i| i.1).collect();
                let crosses = line
                    .items
                    .iter()
                    .map(|i| {
                        let c = inflow[i.0];
                        if sizing(c, !horizontal) == ChildSizing::Fill {
                            clamp_axis(c, !horizontal, line_cross)
                        } else {
                            i.2
                        }
                    })
                    .collect();
                lines.push((who, mains, crosses, line_cross));
            }
        } else if !inflow.is_empty() {
            let mut sizes = Vec::new();
            for c in &inflow {
                sizes.push(base_size(c, horizontal, ctx)?);
            }
            if l.primary_axis_sizing == AxisSizing::Fixed {
                let gaps = inflow.len().saturating_sub(1) as f64 * l.item_spacing;
                let fixed: f64 = inflow
                    .iter()
                    .zip(&sizes)
                    .filter(|(c, _)| c.grow != 1.0)
                    .map(|(_, s)| s)
                    .sum();
                let mut rem = inner_main - fixed - gaps;
                let mut open: Vec<usize> = (0..inflow.len()).filter(|i| inflow[*i].grow == 1.0).collect();
                while !open.is_empty() {
                    let share = rem.max(0.0) / open.len() as f64;
                    let clamped: Vec<(usize, f64)> = open
                        .iter()
                        .map(|i| (*i, clamp_axis(inflow[*i], horizontal, share)))
                        .filter(|(_, c)| *c != share)
                        .collect();
                    if clamped.is_empty() {
                        for i in &open {
                            sizes[*i] = share;
                        }
                        break;
                    }
                    for (i, c) in clamped {
                        sizes[i] = c;
                        rem -= c;
                        open.retain(|j| *j != i);
                    }
                }
            }
            let mut xs = Vec::new();
            for (c, _) in inflow.iter().zip(&sizes) {
                let stretch = sizing(c, !horizontal) == ChildSizing::Fill;
                xs.push(if stretch {
                    clamp_axis(c, !horizontal, inner_cross)
                } else {
                    base_size(c, !horizontal, ctx)?
                });
            }
            lines.push(((0..inflow.len()).collect(), sizes, xs, inner_cross));
        }
        // Stack lines across the cross axis.
        let gap_c = l.counter_axis_spacing.unwrap_or(l.item_spacing);
        let line_total: f64 = lines.iter().map(|x| x.3).sum();
        let l_gaps = lines.len().saturating_sub(1) as f64;
        let between = if l.align_content == AlignContent::SpaceBetween && lines.len() >= 2 {
            ((inner_cross - line_total) / l_gaps).max(0.0)
        } else {
            gap_c
        };
        let mut cross_at = 0.0;
        for (who, sizes, xs, line_cross) in &lines {
            ctx.tick()?;
            let offsets = place_line(sizes, inner_main, l.primary_align, l.item_spacing);
            for (k, &fi) in who.iter().enumerate() {
                let c = inflow[fi];
                // A FILL cross size (STRETCH) already equals the line cross size.
                let align = match c.align {
                    ChildAlign::Inherit => match l.counter_align {
                        CounterAlign::Center => ChildAlign::Center,
                        CounterAlign::Max => ChildAlign::Max,
                        _ => ChildAlign::Min,
                    },
                    a => a,
                };
                let off = match align {
                    ChildAlign::Center => ((line_cross - xs[k]) / 2.0).max(0.0),
                    ChildAlign::Max => (line_cross - xs[k]).max(0.0),
                    _ => 0.0,
                };
                let (x, y) = split(horizontal, ms + offsets[k], cs + cross_at + off);
                let (w, h) = split(horizontal, sizes[k], xs[k]);
                flow_rects[idx[fi]] = Some(RectPt { x, y, w, h });
            }
            cross_at += line_cross + between;
        }
    }
    for (i, c) in n.children.iter().enumerate() {
        let r = if reverse_z { i } else { count - 1 - i };
        match flow_rects[i] {
            Some(cr) => arrange(c, cr, Some(&n.node_id), r, true, ctx)?,
            None => {
                let p = c.placement.ok_or_else(|| E::InvalidLayout(c.node_id.clone()))?;
                let authored = RectPt {
                    x: p.x,
                    y: p.y,
                    w: clamp_axis(c, true, c.width),
                    h: clamp_axis(c, false, c.height),
                };
                let from = SizePt { w: n.width, h: n.height };
                let to = SizePt { w: rect.w, h: rect.h };
                let cr = apply_constraints(authored, from, to, p.constraints)?;
                arrange(c, cr, Some(&n.node_id), r, false, ctx)?;
            }
        }
    }
    Ok(())
}

/// Evaluates the tree. `root_size` replaces the authored size of the root on its non-hugging axes
/// (a resize of the outermost frame); hugging axes are always computed from content.
pub fn solve_auto_layout(
    root: &LayoutNode,
    root_size: Option<SizePt>,
    limits: &LayoutLimits,
    cancel: &CancellationToken,
) -> R<AutoLayoutResult> {
    cancel.check().map_err(|_| E::Canceled)?;
    validate(root, Slot::Root, 0, &mut BTreeSet::new(), limits)?;
    if let Some(s) = root_size
        && (!s.w.is_finite() || !s.h.is_finite() || s.w < 0.0 || s.h < 0.0)
    {
        return Err(E::InvalidGeometry);
    }
    let mut ctx = Ctx {
        cancel,
        boxes: Vec::new(),
    };
    let wanted = |horiz: bool| {
        let o = root_size.map(|s| if horiz { s.w } else { s.h });
        o.unwrap_or(authored(root, horiz))
    };
    // A wrapping root needs its resized primary size to plan lines for a hugging counter axis.
    let primary_override = root.layout.as_ref().and_then(|l| {
        (l.wrap == WrapMode::Wrap).then(|| wanted(l.mode == LayoutMode::Horizontal))
    });
    let content = content_size(root, primary_override, &ctx)?;
    let axis = |horiz: bool, hug: f64| match sizing(root, horiz) {
        ChildSizing::Hug => clamp_axis(root, horiz, hug),
        _ => clamp_axis(root, horiz, wanted(horiz)),
    };
    let rect = RectPt {
        x: 0.0,
        y: 0.0,
        w: axis(true, content.w),
        h: axis(false, content.h),
    };
    arrange(root, rect, None, 0, true, &mut ctx)?;
    Ok(AutoLayoutResult { boxes: ctx.boxes })
}
