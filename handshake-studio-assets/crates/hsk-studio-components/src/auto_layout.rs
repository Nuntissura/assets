//! Auto layout (STU-DS-036..045, 144..150): a deterministic flexbox-style subset evaluated in f64
//! on Folio's records. The container fields are `StudioAutoLayout`, the child fields
//! `StudioChildLayout`, the clamps `StudioSizeClamps` and the constraints of absolutely positioned
//! children `StudioConstraint`, all read from the per-node design entries of the Folio record set;
//! the tree is the Folio document's (children of a node in document order). The only crate-local
//! input is the authored rectangle of each node (`NodeLocal::rect`; Folio layers carry no geometry),
//! with an artboard root falling back to its Folio bounds. Horizontal and vertical flow, wrap, gaps
//! (negative allowed), per-side padding, primary/counter alignment, fixed/hug/fill sizing with
//! min/max clamps, absolutely positioned children (placed by constraints, STU-DS-048) and arbitrary
//! nesting. Numbers are in the document's geometry unit (points for a `pt` document).
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
//!   `layout_grow`/`layout_align` vs FILL); Folio stores both and enforces neither agreement. Both
//!   must agree here; disagreement is `InconsistentSizing`. A node's effective sizing is its
//!   `StudioChildLayout` sizing when it has one, else the sizing implied by its own container modes
//!   (AUTO = hug, FIXED = fixed), else FIXED. Missing child fields default to `INHERIT`, grow 0,
//!   `AUTO` positioning.
//! * A hugging leaf (no auto layout) uses its authored width/height as intrinsic content size; text
//!   measurement belongs to Layout/Type and is not done here.
//! * GRID (lane SMB-FIG-GRID-1) is solved by the `grid` submodule (placement, CSS-Grid-modelled
//!   track sizing, the STU-DS-146 commands); its readings are in that module's notes.
//! * Typed refusals (`UnsupportedLayout`): BASELINE, `strokes_included_in_layout` (stroke
//!   accounting unspecified), SPACE_BETWEEN or WRAP with a hugging primary axis, FILL on the primary
//!   axis in a wrapping parent, wrapping counter-hug with a FILL primary, HUG on a plain frame.
mod grid;

pub use grid::{
    GRID_CELL_BUDGET, GridReorder, GridTrackEntry, GridTrackMove, grid_append_child_at,
    grid_reorder_columns, grid_reorder_rows,
};

use super::constraints::{RectPt, SizePt, apply_constraints};
use super::design::Design;
use crate::{ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AxisSizingMode, ChildAlign, ChildPositioning, ChildSizing, CounterAxisAlign,
    CounterAxisAlignContent, LayoutMode, LayoutWrap, PrimaryAxisAlign, StudioAutoLayout,
    StudioConstraint, StudioGridChild, StudioSizeClamps,
};

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
// The evaluated tree: a borrowed view assembled from the document and the record set

/// STU-DS-149 clamps; `None` = unclamped.
#[derive(Clone, Copy, Default)]
struct Clamps {
    min_w: Option<f64>,
    max_w: Option<f64>,
    min_h: Option<f64>,
    max_h: Option<f64>,
}
impl From<&StudioSizeClamps> for Clamps {
    fn from(k: &StudioSizeClamps) -> Self {
        Self {
            min_w: k.min_width,
            max_w: k.max_width,
            min_h: k.min_height,
            max_h: k.max_height,
        }
    }
}

struct LNode<'a> {
    id: &'a str,
    /// Authored rectangle: the size when fixed, the intrinsic content size of a hugging leaf, the
    /// reference size for placing absolute children, and the authored position of a placed child.
    rect: RectPt,
    sizing_horizontal: ChildSizing,
    sizing_vertical: ChildSizing,
    clamps: Clamps,
    grow: u8,
    align: ChildAlign,
    positioning: ChildPositioning,
    /// STU-DS-150 placement in a GRID parent.
    grid_child: Option<StudioGridChild>,
    constraint: StudioConstraint,
    /// `None` = `layout_mode NONE` or no entry: children are placed by their constraints.
    layout: Option<&'a StudioAutoLayout>,
    children: Vec<LNode<'a>>,
}

fn implied_sizing(l: &StudioAutoLayout) -> (ChildSizing, ChildSizing) {
    let hug = |a: AxisSizingMode| {
        if a == AxisSizingMode::Auto {
            ChildSizing::Hug
        } else {
            ChildSizing::Fixed
        }
    };
    let (primary, counter) = (
        hug(l.primary_axis_sizing_mode),
        hug(l.counter_axis_sizing_mode),
    );
    // GRID reads its primary axis horizontally, as HORIZONTAL does.
    if l.layout_mode != LayoutMode::Vertical {
        (primary, counter)
    } else {
        (counter, primary)
    }
}

fn build<'a>(
    design: &Design<'a>,
    id: &'a str,
    fallback: Option<RectPt>,
    depth: usize,
    count: &mut usize,
    limits: &LayoutLimits,
) -> R<LNode<'a>> {
    if depth > limits.max_depth || *count >= limits.max_nodes {
        return Err(E::BudgetExceeded);
    }
    *count += 1;
    let nd = design.node_design(id);
    let layout = nd
        .and_then(|d| d.auto_layout.as_ref())
        .filter(|a| a.layout_mode != LayoutMode::None);
    let child = nd.and_then(|d| d.child_layout.as_ref());
    let rect = design
        .local()
        .nodes
        .get(id)
        .and_then(|n| n.rect)
        .or(fallback)
        .ok_or_else(|| E::InvalidLayout(format!("rect_missing@{id}")))?;
    let (sizing_horizontal, sizing_vertical) = match (child, layout) {
        (Some(c), _) => (c.layout_sizing_horizontal, c.layout_sizing_vertical),
        (None, Some(l)) => implied_sizing(l),
        (None, None) => (ChildSizing::Fixed, ChildSizing::Fixed),
    };
    let mut children = Vec::new();
    for &c in design.children_of(id) {
        children.push(build(design, c, None, depth + 1, count, limits)?);
    }
    Ok(LNode {
        id,
        rect,
        sizing_horizontal,
        sizing_vertical,
        clamps: nd
            .and_then(|d| d.size_clamps.as_ref())
            .map(Clamps::from)
            .unwrap_or_default(),
        grow: child.map_or(0, |c| c.layout_grow),
        align: child.map_or(ChildAlign::Inherit, |c| c.layout_align),
        positioning: child.map_or(ChildPositioning::Auto, |c| c.layout_positioning),
        grid_child: child.and_then(|c| c.grid_child),
        constraint: nd
            .and_then(|d| d.constraints.clone())
            .unwrap_or_else(StudioConstraint::top_left),
        layout,
        children,
    })
}

// ----------------------------------------------------------------------------------------------
// Validation

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Root,
    InFlow { wrap: bool, parent_horizontal: bool },
    InGrid,
    Placed,
}

fn in_range(v: f64, lo: f64, hi: f64, what: &str) -> R<()> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(E::InvalidLayout(what.to_owned()))
    }
}
fn axis_sizing_ok(s: ChildSizing, a: AxisSizingMode) -> bool {
    matches!(
        (s, a),
        (ChildSizing::Hug, AxisSizingMode::Auto)
            | (
                ChildSizing::Fixed | ChildSizing::Fill,
                AxisSizingMode::Fixed
            )
    )
}

fn validate(n: &LNode, slot: Slot) -> R<()> {
    let id = n.id;
    in_range(n.rect.w, 0.0, 100_000.0, id)?;
    in_range(n.rect.h, 0.0, 100_000.0, id)?;
    for (lo, hi) in [
        (n.clamps.min_w, n.clamps.max_w),
        (n.clamps.min_h, n.clamps.max_h),
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
    if n.grow > 1 {
        return Err(E::InvalidLayout(format!("layout_grow@{id}")));
    }
    let (sh, sv) = (n.sizing_horizontal, n.sizing_vertical);
    if n.grid_child.is_some() && slot != Slot::InGrid {
        return Err(E::InvalidLayout(format!("grid_child_outside_grid@{id}")));
    }
    match slot {
        Slot::Root if sh == ChildSizing::Fill || sv == ChildSizing::Fill => {
            return Err(E::InvalidLayout(format!("fill_on_root@{id}")));
        }
        Slot::Placed => {
            if sh != ChildSizing::Fixed || sv != ChildSizing::Fixed {
                return Err(E::InvalidLayout(format!("placed_child_not_fixed@{id}")));
            }
            if !n.rect.x.is_finite() || !n.rect.y.is_finite() {
                return Err(E::InvalidLayout(format!("placement@{id}")));
            }
        }
        Slot::InFlow {
            wrap,
            parent_horizontal,
        } => {
            let (main, cross) = if parent_horizontal {
                (sh, sv)
            } else {
                (sv, sh)
            };
            if (main == ChildSizing::Fill) != (n.grow == 1)
                || (cross == ChildSizing::Fill) != (n.align == ChildAlign::Stretch)
            {
                return Err(E::InconsistentSizing(id.to_owned()));
            }
            if wrap && main == ChildSizing::Fill {
                return Err(E::UnsupportedLayout("fill_in_wrap".to_owned()));
            }
        }
        Slot::InGrid => grid::validate_child(n)?,
        Slot::Root => {}
    }
    match n.layout {
        None => {
            if !n.children.is_empty() && (sh == ChildSizing::Hug || sv == ChildSizing::Hug) {
                return Err(E::UnsupportedLayout("hug_without_auto_layout".to_owned()));
            }
            for c in &n.children {
                validate(c, Slot::Placed)?;
            }
        }
        Some(l) => {
            let is_grid = l.layout_mode == LayoutMode::Grid;
            if is_grid != l.grid.is_some() {
                return Err(E::InvalidLayout(format!("grid_fields@{id}")));
            }
            if l.counter_axis_align_items == CounterAxisAlign::Baseline {
                return Err(E::UnsupportedLayout("baseline".to_owned()));
            }
            if l.strokes_included_in_layout {
                return Err(E::UnsupportedLayout(
                    "strokes_included_in_layout".to_owned(),
                ));
            }
            in_range(l.item_spacing, -10_000.0, 100_000.0, id)?;
            if let Some(v) = l.counter_axis_spacing {
                in_range(v, -10_000.0, 100_000.0, id)?;
            }
            for p in [
                l.padding_top,
                l.padding_right,
                l.padding_bottom,
                l.padding_left,
            ] {
                in_range(p, 0.0, 100_000.0, id)?;
            }
            let horizontal = l.layout_mode != LayoutMode::Vertical;
            let (sp, sc) = if horizontal { (sh, sv) } else { (sv, sh) };
            if !axis_sizing_ok(sp, l.primary_axis_sizing_mode)
                || !axis_sizing_ok(sc, l.counter_axis_sizing_mode)
            {
                return Err(E::InconsistentSizing(id.to_owned()));
            }
            let wrap = l.layout_wrap == LayoutWrap::Wrap;
            if l.counter_axis_align_content == CounterAxisAlignContent::SpaceBetween && !wrap {
                return Err(E::InvalidLayout(format!("align_content_without_wrap@{id}")));
            }
            if is_grid {
                grid::validate_container(n, l)?;
                for c in &n.children {
                    let child_slot = if c.positioning == ChildPositioning::Absolute {
                        Slot::Placed
                    } else {
                        Slot::InGrid
                    };
                    validate(c, child_slot)?;
                }
                return Ok(());
            }
            if l.primary_axis_sizing_mode == AxisSizingMode::Auto
                && (wrap || l.primary_axis_align_items == PrimaryAxisAlign::SpaceBetween)
            {
                let what = if wrap {
                    "wrap_with_hug_primary"
                } else {
                    "space_between_with_hug"
                };
                return Err(E::UnsupportedLayout(what.to_owned()));
            }
            if wrap && l.counter_axis_sizing_mode == AxisSizingMode::Auto && sp == ChildSizing::Fill
            {
                return Err(E::UnsupportedLayout(
                    "wrap_counter_hug_with_fill".to_owned(),
                ));
            }
            for c in &n.children {
                let child_slot = if c.positioning == ChildPositioning::Absolute {
                    Slot::Placed
                } else {
                    Slot::InFlow {
                        wrap,
                        parent_horizontal: horizontal,
                    }
                };
                validate(c, child_slot)?;
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

fn authored(n: &LNode, horiz: bool) -> f64 {
    if horiz { n.rect.w } else { n.rect.h }
}
fn sizing(n: &LNode, horiz: bool) -> ChildSizing {
    if horiz {
        n.sizing_horizontal
    } else {
        n.sizing_vertical
    }
}
fn clamp_axis(n: &LNode, horiz: bool, v: f64) -> f64 {
    let (lo, hi) = if horiz {
        (n.clamps.min_w, n.clamps.max_w)
    } else {
        (n.clamps.min_h, n.clamps.max_h)
    };
    let v = lo.map_or(v, |m| v.max(m));
    hi.map_or(v, |m| v.min(m))
}
/// (main, cross) <-> (w, h) for a flow direction.
fn split(horizontal: bool, w: f64, h: f64) -> (f64, f64) {
    if horizontal { (w, h) } else { (h, w) }
}
fn pads(l: &StudioAutoLayout) -> (f64, f64, f64, f64) {
    // (main_start, main_end, cross_start, cross_end) for the flow direction.
    if l.layout_mode == LayoutMode::Horizontal {
        (
            l.padding_left,
            l.padding_right,
            l.padding_top,
            l.padding_bottom,
        )
    } else {
        (
            l.padding_top,
            l.padding_bottom,
            l.padding_left,
            l.padding_right,
        )
    }
}

/// Size along one axis when the node's sizing is HUG or FILL-as-content, or its clamped authored
/// size when FIXED.
fn base_size(n: &LNode, horiz: bool, ctx: &Ctx) -> R<f64> {
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
fn plan_lines(inflow: &[&LNode], l: &StudioAutoLayout, avail_main: f64, ctx: &Ctx) -> R<Vec<Line>> {
    let horizontal = l.layout_mode == LayoutMode::Horizontal;
    let mut lines: Vec<Line> = Vec::new();
    let mut cur = Line { items: Vec::new() };
    let mut len = 0.0;
    for (i, c) in inflow.iter().enumerate() {
        ctx.tick()?;
        let m = base_size(c, horizontal, ctx)?;
        let x = base_size(c, !horizontal, ctx)?;
        let next = if cur.items.is_empty() {
            m
        } else {
            len + l.item_spacing + m
        };
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
fn content_size(n: &LNode, primary_outer: Option<f64>, ctx: &Ctx) -> R<SizePt> {
    ctx.tick()?;
    let Some(l) = n.layout else {
        return Ok(SizePt {
            w: n.rect.w,
            h: n.rect.h,
        });
    };
    if let Some(g) = grid::grid_of(l) {
        return grid::content_size(n, l, g, ctx);
    }
    let horizontal = l.layout_mode == LayoutMode::Horizontal;
    let (ms, me, cs, ce) = pads(l);
    let inflow: Vec<&LNode> = n
        .children
        .iter()
        .filter(|c| c.positioning == ChildPositioning::Auto)
        .collect();
    let (main, cross) = if l.layout_wrap == LayoutWrap::Wrap {
        let outer = clamp_axis(
            n,
            horizontal,
            primary_outer.unwrap_or(authored(n, horizontal)),
        );
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

fn place_line(sizes: &[f64], avail: f64, align: PrimaryAxisAlign, spacing: f64) -> Vec<f64> {
    let n = sizes.len();
    let total: f64 = sizes.iter().sum();
    let gaps = n.saturating_sub(1) as f64;
    let (start, gap) = match align {
        PrimaryAxisAlign::SpaceBetween if n >= 2 => (0.0, ((avail - total) / gaps).max(0.0)),
        PrimaryAxisAlign::SpaceBetween | PrimaryAxisAlign::Min => (0.0, spacing),
        PrimaryAxisAlign::Center => (((avail - total - spacing * gaps) / 2.0).max(0.0), spacing),
        PrimaryAxisAlign::Max => ((avail - total - spacing * gaps).max(0.0), spacing),
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
    n: &LNode,
    rect: RectPt,
    parent_id: Option<&str>,
    rank: usize,
    in_flow: bool,
    ctx: &mut Ctx,
) -> R<()> {
    ctx.tick()?;
    ctx.boxes.push(PlacedBox {
        node_id: n.id.to_owned(),
        parent_id: parent_id.map(str::to_owned),
        rect,
        paint_rank: rank,
        in_flow,
    });
    let count = n.children.len();
    let mut flow_rects: Vec<Option<RectPt>> = vec![None; count];
    let mut reverse_z = true;
    if let Some((l, g)) = n.layout.and_then(|l| grid::grid_of(l).map(|g| (l, g))) {
        reverse_z = l.item_reverse_z_index;
        flow_rects = grid::arrange(n, l, g, rect, ctx)?;
    } else if let Some(l) = n.layout {
        reverse_z = l.item_reverse_z_index;
        let horizontal = l.layout_mode == LayoutMode::Horizontal;
        let (ms, me, cs, ce) = pads(l);
        let (main_outer, cross_outer) = split(horizontal, rect.w, rect.h);
        let (inner_main, inner_cross) = (main_outer - ms - me, cross_outer - cs - ce);
        if inner_main < 0.0 || inner_cross < 0.0 {
            return Err(E::InvalidGeometry);
        }
        let idx: Vec<usize> = (0..count)
            .filter(|i| n.children[*i].positioning == ChildPositioning::Auto)
            .collect();
        let inflow: Vec<&LNode> = idx.iter().map(|i| &n.children[*i]).collect();
        // Lines: (child indices into `inflow`, final main sizes, final cross sizes, line cross size)
        let mut lines: Vec<PlannedLine> = Vec::new();
        if l.layout_wrap == LayoutWrap::Wrap {
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
            if l.primary_axis_sizing_mode == AxisSizingMode::Fixed {
                let gaps = inflow.len().saturating_sub(1) as f64 * l.item_spacing;
                let fixed: f64 = inflow
                    .iter()
                    .zip(&sizes)
                    .filter(|(c, _)| c.grow != 1)
                    .map(|(_, s)| s)
                    .sum();
                let mut rem = inner_main - fixed - gaps;
                let mut open: Vec<usize> =
                    (0..inflow.len()).filter(|i| inflow[*i].grow == 1).collect();
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
        let between = if l.counter_axis_align_content == CounterAxisAlignContent::SpaceBetween
            && lines.len() >= 2
        {
            ((inner_cross - line_total) / l_gaps).max(0.0)
        } else {
            gap_c
        };
        let mut cross_at = 0.0;
        for (who, sizes, xs, line_cross) in &lines {
            ctx.tick()?;
            let offsets = place_line(
                sizes,
                inner_main,
                l.primary_axis_align_items,
                l.item_spacing,
            );
            for (k, &fi) in who.iter().enumerate() {
                let c = inflow[fi];
                // A FILL cross size (STRETCH) already equals the line cross size.
                let align = match c.align {
                    ChildAlign::Inherit => match l.counter_axis_align_items {
                        CounterAxisAlign::Center => ChildAlign::Center,
                        CounterAxisAlign::Max => ChildAlign::Max,
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
            Some(cr) => arrange(c, cr, Some(n.id), r, true, ctx)?,
            None => {
                let placed = RectPt {
                    x: c.rect.x,
                    y: c.rect.y,
                    w: clamp_axis(c, true, c.rect.w),
                    h: clamp_axis(c, false, c.rect.h),
                };
                let from = SizePt {
                    w: n.rect.w,
                    h: n.rect.h,
                };
                let to = SizePt {
                    w: rect.w,
                    h: rect.h,
                };
                let cr = apply_constraints(placed, from, to, &c.constraint)?;
                arrange(c, cr, Some(n.id), r, false, ctx)?;
            }
        }
    }
    Ok(())
}

/// Evaluates the tree under node `root_id` (a group layer or an artboard of the document).
/// `root_size` replaces the authored size of the root on its non-hugging axes (a resize of the
/// outermost frame); hugging axes are always computed from content.
pub fn solve_auto_layout(
    design: &Design,
    root_id: &str,
    root_size: Option<SizePt>,
    limits: &LayoutLimits,
    cancel: &CancellationToken,
) -> R<AutoLayoutResult> {
    cancel.check().map_err(|_| E::Canceled)?;
    let doc = design.document();
    let (id, fallback) = if let Some(l) = design.layer(root_id) {
        (l.layer_id.as_str(), None)
    } else if let Some(a) = doc.artboards.iter().find(|a| a.artboard_id == root_id) {
        let (w, h) = (a.bounds.width.value, a.bounds.height.value);
        (
            a.artboard_id.as_str(),
            Some(RectPt {
                x: 0.0,
                y: 0.0,
                w,
                h,
            }),
        )
    } else if doc.page_spreads.iter().any(|s| s.page_spread_id == root_id) {
        return Err(E::InvalidLayout(format!("page_spread_root@{root_id}")));
    } else {
        return Err(E::NoSuchNode(root_id.to_owned()));
    };
    let root = build(design, id, fallback, 0, &mut 0, limits)?;
    validate(&root, Slot::Root)?;
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
        o.unwrap_or(authored(&root, horiz))
    };
    // A wrapping root needs its resized primary size to plan lines for a hugging counter axis.
    let primary_override = root.layout.and_then(|l| {
        (l.layout_wrap == LayoutWrap::Wrap).then(|| wanted(l.layout_mode == LayoutMode::Horizontal))
    });
    let content = content_size(&root, primary_override, &ctx)?;
    let axis = |horiz: bool, hug: f64| match sizing(&root, horiz) {
        ChildSizing::Hug => clamp_axis(&root, horiz, hug),
        _ => clamp_axis(&root, horiz, wanted(horiz)),
    };
    let rect = RectPt {
        x: 0.0,
        y: 0.0,
        w: axis(true, content.w),
        h: axis(false, content.h),
    };
    arrange(&root, rect, None, 0, true, &mut ctx)?;
    Ok(AutoLayoutResult { boxes: ctx.boxes })
}
