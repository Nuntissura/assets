//! GRID auto layout (lane SMB-FIG-GRID-1; STU-DS-145/146/147/148/150): placement, track sizing and
//! the three STU-DS-146 commands over Folio's `StudioGridLayout` / `StudioGridChild` records.
//!
//! Studio readings (the Figma grid algorithm is UNVERIFIED, D5). The track algorithm is modelled on
//! the W3C CSS Grid track sizing algorithm (css-grid-1 section 11, simplified) and every number is a
//! hand-checkable f64:
//! * Columns are the primary (horizontal) axis and rows the counter axis: the container's
//!   `primary_axis_*` fields act horizontally and `counter_axis_*` vertically, as for HORIZONTAL.
//! * Placement: anchored children (`grid_child`) take their anchors and spans. Under `ROW_AUTO_FLOW`
//!   the other in-flow children follow in document order, one cell each, row-major, from a cursor
//!   that only moves forward and skips occupied cells (css-grid-1 8.5, sparse packing). With
//!   `grid_auto_tracks` `ROWS` a full grid gains implicit HUG rows; with `NONE` that overflow is the
//!   typed refusal `grid_overflow`. Under `MANUAL` every in-flow child needs a `grid_child`.
//!   Anchored children may overlap.
//! * Track sizing per axis, independently (child sizes do not depend on the cell size: no text
//!   reflow here): FIXED takes its value; HUG takes the largest base size of the span-1 children
//!   in it, then children spanning several tracks, in increasing span order, add what they still
//!   need in equal shares to the HUG tracks they cover (children spanning a FLEX track are left out,
//!   css-grid-1 11.5); FLEX shares what the definite inner size leaves after the other tracks and
//!   the gaps, by factor (11.7), and is 0 when nothing is left. A hugging (AUTO) axis is indefinite:
//!   its FLEX tracks take `fr * factor` with `fr` the largest span-1 content / factor (11.7.1), and
//!   the container size is the padding plus the tracks plus the gaps.
//! * Tracks start at the padding and are not distributed further. A FILL child takes its cell
//!   area (clamped); a FIXED or HUG child keeps its base size and is aligned in the cell:
//!   horizontally by `primary_axis_align_items` (MIN/CENTER/MAX), vertically by its `layout_align`
//!   or, for INHERIT, `counter_axis_align_items`; a child larger than its cell is packed to the
//!   start (never a negative offset), as in the flow path.
//! * Refused as typed errors: `SPACE_BETWEEN` (`grid_space_between`), `layout_grow` 1 in a grid
//!   (`grid_layout_grow`), WRAP, a grid record that disagrees with `layout_mode`, track lists that
//!   do not match the counts, anchors or spans past the counts, an unanchored child under MANUAL.
//!   Absolute children keep their constraints (STU-DS-048), as in the flow path.
//! * Commands are pure: they return the successor design records (validated by Folio) or a typed
//!   refusal and never mint ids or touch the document; `append_child_at` needs the node to be a
//!   document child of the container already (re-parenting is a document edit). The reorder options
//!   (`GridTrackMove`: move the track at `from_index` to `to_index`) are a Studio reading; anchored
//!   children move with their tracks and a move that would split a spanning child is refused.
#![deny(clippy::arithmetic_side_effects)]
use super::{Ctx, LNode, base_size, clamp_axis};
use crate::constraints::RectPt;
use crate::design::Design;
use crate::{ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, ChildAlign, ChildPositioning, ChildSizing, CounterAxisAlign, GRID_COUNT_MAX,
    GRID_GAP_MAX, GridAutoTracks, GridItemsPositioning, GridTrackType, LayoutMode, LayoutWrap,
    NodeKind, NodeRef, PrimaryAxisAlign, StudioAutoLayout, StudioChildLayout, StudioDesignRecords,
    StudioGridChild, StudioGridLayout, StudioGridTrackSize, StudioNodeDesign, Unresolved,
    grid_track_ok, validate_design_records,
};
use std::collections::BTreeMap;

/// Cell visits (occupancy marks and cursor steps) one grid placement may make; `studio_declared`.
pub const GRID_CELL_BUDGET: usize = 1 << 24;

/// The grid record of a GRID container, `None` for any other container.
pub(super) fn grid_of(l: &StudioAutoLayout) -> Option<&StudioGridLayout> {
    l.grid.as_ref().filter(|_| l.layout_mode == LayoutMode::Grid)
}

fn u(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

// ----------------------------------------------------------------------------------------------
// Validation

/// The container contract and every child's placement fields; called by the evaluator's validation
/// before anything is measured, so hostile counts, lists and spans are refused before allocation.
pub(super) fn validate_container(n: &LNode, l: &StudioAutoLayout) -> R<()> {
    let id = n.id;
    let g = match (l.layout_mode == LayoutMode::Grid, l.grid.as_ref()) {
        (true, Some(g)) => g,
        _ => return Err(E::InvalidLayout(format!("grid_fields@{id}"))),
    };
    if l.layout_wrap == LayoutWrap::Wrap {
        return Err(E::InvalidLayout(format!("grid_wrap@{id}")));
    }
    if l.primary_axis_align_items == PrimaryAxisAlign::SpaceBetween {
        return Err(E::UnsupportedLayout("grid_space_between".to_owned()));
    }
    for n in [g.grid_row_count, g.grid_column_count] {
        if !(1..=GRID_COUNT_MAX).contains(&n) {
            return Err(E::InvalidLayout(format!("grid_count@{id}")));
        }
    }
    for v in [g.grid_row_gap, g.grid_column_gap] {
        if !(v.is_finite() && (0.0..=GRID_GAP_MAX).contains(&v)) {
            return Err(E::InvalidLayout(format!("grid_gap@{id}")));
        }
    }
    for (list, n) in [
        (&g.grid_row_sizes, g.grid_row_count),
        (&g.grid_column_sizes, g.grid_column_count),
    ] {
        if list.len() != u(n) || !list.iter().all(grid_track_ok) {
            return Err(E::InvalidLayout(format!("grid_tracks@{id}")));
        }
    }
    let manual = g.grid_items_positioning == GridItemsPositioning::Manual;
    for c in &n.children {
        if c.positioning != ChildPositioning::Auto {
            continue;
        }
        match c.grid_child {
            Some(gc) => {
                area(&gc, g).ok_or_else(|| E::InvalidLayout(format!("grid_span@{}", c.id)))?;
            }
            None if manual => {
                return Err(E::InvalidLayout(format!("grid_child_unanchored@{}", c.id)));
            }
            None => {}
        }
    }
    Ok(())
}

/// Per-child rules in a grid: grow is a flow field; vertical FILL and STRETCH must agree.
pub(super) fn validate_child(n: &LNode) -> R<()> {
    if n.grow != 0 {
        return Err(E::InvalidLayout(format!("grid_layout_grow@{}", n.id)));
    }
    if (n.sizing_vertical == ChildSizing::Fill) != (n.align == ChildAlign::Stretch) {
        return Err(E::InconsistentSizing(n.id.to_owned()));
    }
    Ok(())
}

// ----------------------------------------------------------------------------------------------
// Placement

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    row: u32,
    col: u32,
    row_span: u32,
    col_span: u32,
}

/// The anchored area if anchors and spans fit the declared counts (checked, never wrapping).
fn area(gc: &StudioGridChild, g: &StudioGridLayout) -> Option<Area> {
    let fits = |anchor: u32, span: u32, count: u32| {
        span >= 1 && anchor.checked_add(span).is_some_and(|end| end <= count)
    };
    (fits(gc.grid_row_anchor_index, gc.grid_row_span, g.grid_row_count)
        && fits(
            gc.grid_column_anchor_index,
            gc.grid_column_span,
            g.grid_column_count,
        ))
    .then_some(Area {
        row: gc.grid_row_anchor_index,
        col: gc.grid_column_anchor_index,
        row_span: gc.grid_row_span,
        col_span: gc.grid_column_span,
    })
}

fn charge(ops: &mut usize) -> R<()> {
    *ops = ops.checked_add(1).ok_or(E::BudgetExceeded)?;
    if *ops > GRID_CELL_BUDGET {
        return Err(E::BudgetExceeded);
    }
    Ok(())
}

/// (child index, area) for every in-flow child, and the row count including implicit rows.
fn place(n: &LNode, g: &StudioGridLayout, ctx: &Ctx) -> R<(Vec<(usize, Area)>, u32)> {
    let mut rows = g.grid_row_count;
    let cols = u(g.grid_column_count);
    let mut placed = Vec::new();
    let mut flowing = Vec::new();
    for (i, c) in n.children.iter().enumerate() {
        if c.positioning != ChildPositioning::Auto {
            continue;
        }
        match c.grid_child {
            Some(gc) => placed.push((
                i,
                area(&gc, g).ok_or_else(|| E::InvalidLayout(format!("grid_span@{}", c.id)))?,
            )),
            None if g.grid_items_positioning == GridItemsPositioning::Manual => {
                return Err(E::InvalidLayout(format!("grid_child_unanchored@{}", c.id)));
            }
            None => flowing.push(i),
        }
    }
    if flowing.is_empty() {
        return Ok((placed, rows));
    }
    let cells = u(rows)
        .checked_mul(cols)
        .ok_or(E::BudgetExceeded)?;
    let mut occupied = vec![false; cells];
    let mut ops = 0usize;
    for (_, a) in &placed {
        ctx.tick()?;
        for r in a.row..a.row.saturating_add(a.row_span) {
            for c in a.col..a.col.saturating_add(a.col_span) {
                charge(&mut ops)?;
                let at = u(r).checked_mul(cols).and_then(|x| x.checked_add(u(c)));
                if let Some(slot) = at.and_then(|at| occupied.get_mut(at)) {
                    *slot = true;
                }
            }
        }
    }
    let mut cursor = 0usize;
    for i in flowing {
        ctx.tick()?;
        loop {
            charge(&mut ops)?;
            if cursor >= occupied.len() {
                if g.grid_auto_tracks != GridAutoTracks::Rows {
                    return Err(E::InvalidLayout(format!("grid_overflow@{}", n.id)));
                }
                rows = rows.checked_add(1).ok_or(E::BudgetExceeded)?;
                occupied.resize(occupied.len().saturating_add(cols), false);
            }
            if !occupied.get(cursor).copied().unwrap_or(true) {
                break;
            }
            cursor = cursor.checked_add(1).ok_or(E::BudgetExceeded)?;
        }
        if let Some(slot) = occupied.get_mut(cursor) {
            *slot = true;
        }
        let row = cursor.checked_div(cols).unwrap_or(0);
        let col = cursor.checked_rem(cols).unwrap_or(0);
        placed.push((
            i,
            Area {
                row: u32::try_from(row).map_err(|_| E::BudgetExceeded)?,
                col: u32::try_from(col).map_err(|_| E::BudgetExceeded)?,
                row_span: 1,
                col_span: 1,
            },
        ));
        cursor = cursor.checked_add(1).ok_or(E::BudgetExceeded)?;
    }
    Ok((placed, rows))
}

// ----------------------------------------------------------------------------------------------
// Track sizing

/// A span-`len` item starting at track `start` that needs `size` on this axis.
struct Need {
    start: usize,
    len: usize,
    size: f64,
}

/// Track sizes for one axis. `defs` are the declared tracks followed by implicit HUG tracks
/// (`None`); `avail` is the definite inner size, or `None` for a hugging axis.
fn size_tracks(
    defs: &[Option<StudioGridTrackSize>],
    gap: f64,
    avail: Option<f64>,
    needs: &[Need],
) -> Vec<f64> {
    let kind = |k: usize| defs.get(k).copied().flatten();
    let is = |k: usize, t: GridTrackType| {
        kind(k).map_or(t == GridTrackType::Hug, |d| d.track_type == t)
    };
    let value = |k: usize| kind(k).and_then(|d| d.value).unwrap_or(0.0);
    let n = defs.len();
    let mut size = vec![0.0; n];
    let mut content = vec![0.0f64; n];
    for need in needs.iter().filter(|x| x.len == 1) {
        if let Some(c) = content.get_mut(need.start) {
            *c = c.max(need.size);
        }
    }
    for k in 0..n {
        size[k] = if is(k, GridTrackType::Fixed) {
            value(k)
        } else if is(k, GridTrackType::Hug) {
            content[k]
        } else {
            0.0
        };
    }
    // Spanning children over HUG tracks, smallest span first (stable: document order within).
    let mut spanning: Vec<&Need> = needs
        .iter()
        .filter(|x| x.len > 1)
        .filter(|x| {
            let r = x.start..x.start.saturating_add(x.len).min(n);
            !r.clone().any(|k| is(k, GridTrackType::Flex)) && r.clone().any(|k| is(k, GridTrackType::Hug))
        })
        .collect();
    spanning.sort_by_key(|x| x.len);
    for need in spanning {
        let r = need.start..need.start.saturating_add(need.len).min(n);
        let gaps = gap * r.len().saturating_sub(1) as f64;
        let covered: f64 = size[r.clone()].iter().sum::<f64>() + gaps;
        let missing = need.size - covered;
        if missing > 0.0 {
            let hugs: Vec<usize> = r.filter(|k| is(*k, GridTrackType::Hug)).collect();
            let share = missing / hugs.len() as f64;
            for k in hugs {
                size[k] += share;
            }
        }
    }
    let flex: Vec<usize> = (0..n).filter(|k| is(*k, GridTrackType::Flex)).collect();
    if !flex.is_empty() {
        let factor_sum: f64 = flex.iter().map(|k| value(*k)).sum();
        match avail {
            Some(a) => {
                let used: f64 = (0..n)
                    .filter(|k| !is(*k, GridTrackType::Flex))
                    .map(|k| size[k])
                    .sum::<f64>()
                    + gap * n.saturating_sub(1) as f64;
                let left = (a - used).max(0.0);
                for k in flex {
                    size[k] = if factor_sum > 0.0 {
                        left * value(k) / factor_sum
                    } else {
                        0.0
                    };
                }
            }
            None => {
                let fr = flex
                    .iter()
                    .filter(|k| value(**k) > 0.0)
                    .fold(0.0f64, |m, k| m.max(content[*k] / value(*k)));
                for k in flex {
                    size[k] = fr * value(k);
                }
            }
        }
    }
    size
}

fn offsets(sizes: &[f64], start: f64, gap: f64) -> Vec<f64> {
    let mut at = start;
    sizes
        .iter()
        .map(|s| {
            let here = at;
            at += s + gap;
            here
        })
        .collect()
}

fn span_len(sizes: &[f64], start: u32, len: u32, gap: f64) -> f64 {
    let s = u(start);
    let r = s..s.saturating_add(u(len)).min(sizes.len());
    sizes[r.clone()].iter().sum::<f64>() + gap * r.len().saturating_sub(1) as f64
}

struct Solved {
    placed: Vec<(usize, Area)>,
    cols: Vec<f64>,
    rows: Vec<f64>,
}

/// Places the children and sizes both axes; `avail` is the definite inner (w, h) per axis.
fn solve(
    n: &LNode,
    g: &StudioGridLayout,
    avail: (Option<f64>, Option<f64>),
    ctx: &Ctx,
) -> R<Solved> {
    let (placed, row_count) = place(n, g, ctx)?;
    let mut col_needs = Vec::with_capacity(placed.len());
    let mut row_needs = Vec::with_capacity(placed.len());
    for (i, a) in &placed {
        ctx.tick()?;
        let c = &n.children[*i];
        col_needs.push(Need {
            start: u(a.col),
            len: u(a.col_span),
            size: base_size(c, true, ctx)?,
        });
        row_needs.push(Need {
            start: u(a.row),
            len: u(a.row_span),
            size: base_size(c, false, ctx)?,
        });
    }
    let col_defs: Vec<Option<StudioGridTrackSize>> =
        g.grid_column_sizes.iter().copied().map(Some).collect();
    let mut row_defs: Vec<Option<StudioGridTrackSize>> =
        g.grid_row_sizes.iter().copied().map(Some).collect();
    row_defs.resize(u(row_count), None);
    Ok(Solved {
        cols: size_tracks(&col_defs, g.grid_column_gap, avail.0, &col_needs),
        rows: size_tracks(&row_defs, g.grid_row_gap, avail.1, &row_needs),
        placed,
    })
}

fn total(sizes: &[f64], gap: f64) -> f64 {
    sizes.iter().sum::<f64>() + gap * sizes.len().saturating_sub(1) as f64
}

/// Hug size of a GRID container: padding plus tracks plus gaps on both axes.
pub(super) fn content_size(
    n: &LNode,
    l: &StudioAutoLayout,
    g: &StudioGridLayout,
    ctx: &Ctx,
) -> R<crate::constraints::SizePt> {
    let s = solve(n, g, (None, None), ctx)?;
    Ok(crate::constraints::SizePt {
        w: l.padding_left + l.padding_right + total(&s.cols, g.grid_column_gap),
        h: l.padding_top + l.padding_bottom + total(&s.rows, g.grid_row_gap),
    })
}

/// Cell rectangles (parent-local) of the in-flow children of a GRID container laid out in `rect`.
pub(super) fn arrange(
    n: &LNode,
    l: &StudioAutoLayout,
    g: &StudioGridLayout,
    rect: RectPt,
    ctx: &Ctx,
) -> R<Vec<Option<RectPt>>> {
    let inner_w = rect.w - l.padding_left - l.padding_right;
    let inner_h = rect.h - l.padding_top - l.padding_bottom;
    if inner_w < 0.0 || inner_h < 0.0 {
        return Err(E::InvalidGeometry);
    }
    let definite = |s: ChildSizing, v: f64| (s != ChildSizing::Hug).then_some(v);
    let s = solve(
        n,
        g,
        (
            definite(n.sizing_horizontal, inner_w),
            definite(n.sizing_vertical, inner_h),
        ),
        ctx,
    )?;
    let xs = offsets(&s.cols, l.padding_left, g.grid_column_gap);
    let ys = offsets(&s.rows, l.padding_top, g.grid_row_gap);
    let mut out = vec![None; n.children.len()];
    for (i, a) in &s.placed {
        ctx.tick()?;
        let c = &n.children[*i];
        let cell_w = span_len(&s.cols, a.col, a.col_span, g.grid_column_gap);
        let cell_h = span_len(&s.rows, a.row, a.row_span, g.grid_row_gap);
        let w = if c.sizing_horizontal == ChildSizing::Fill {
            clamp_axis(c, true, cell_w)
        } else {
            base_size(c, true, ctx)?
        };
        let h = if c.sizing_vertical == ChildSizing::Fill {
            clamp_axis(c, false, cell_h)
        } else {
            base_size(c, false, ctx)?
        };
        let dx = match l.primary_axis_align_items {
            PrimaryAxisAlign::Center => ((cell_w - w) / 2.0).max(0.0),
            PrimaryAxisAlign::Max => (cell_w - w).max(0.0),
            _ => 0.0,
        };
        let vertical = match c.align {
            ChildAlign::Inherit => match l.counter_axis_align_items {
                CounterAxisAlign::Center => ChildAlign::Center,
                CounterAxisAlign::Max => ChildAlign::Max,
                _ => ChildAlign::Min,
            },
            a => a,
        };
        let dy = match vertical {
            ChildAlign::Center => ((cell_h - h) / 2.0).max(0.0),
            ChildAlign::Max => (cell_h - h).max(0.0),
            _ => 0.0,
        };
        let (x0, y0) = (
            xs.get(u(a.col)).copied().unwrap_or(l.padding_left),
            ys.get(u(a.row)).copied().unwrap_or(l.padding_top),
        );
        if let Some(slot) = out.get_mut(*i) {
            *slot = Some(RectPt {
                x: x0 + dx,
                y: y0 + dy,
                w,
                h,
            });
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------------------------------
// Commands (STU-DS-146)

/// `grid.reorder_rows` / `grid.reorder_columns` options: move the track at `from_index` to
/// `to_index` (the others keep their order). Studio reading; the spec names no option fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridTrackMove {
    pub from_index: u32,
    pub to_index: u32,
}

/// One track of the reordered axis: its new index, the index it had before, and its size record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridTrackEntry {
    pub index: u32,
    pub previous_index: u32,
    pub size: StudioGridTrackSize,
}

/// A reorder result: the successor records and the resulting ordered track entries (STU-DS-146
/// requires the reorder commands to return them).
#[derive(Clone, Debug, PartialEq)]
pub struct GridReorder {
    pub records: StudioDesignRecords,
    pub tracks: Vec<GridTrackEntry>,
}

fn grid_container<'a>(design: &Design<'a>, container: &str) -> R<&'a StudioGridLayout> {
    if design.layer(container).is_none() {
        return Err(E::NoSuchNode(container.to_owned()));
    }
    design
        .node_design(container)
        .and_then(|d| d.auto_layout.as_ref())
        .and_then(grid_of)
        .ok_or_else(|| E::InvalidLayout(format!("grid_not_a_grid@{container}")))
}

fn accept(design: &Design, records: StudioDesignRecords) -> R<StudioDesignRecords> {
    validate_design_records(
        &records,
        design.document(),
        Budget::default(),
        &CancellationToken::default(),
        &Unresolved,
    )
    .map_err(E::from)?;
    Ok(records)
}

fn overlaps(a: &Area, b: &Area) -> bool {
    let apart = |s0: u32, l0: u32, s1: u32, l1: u32| {
        s0.saturating_add(l0) <= s1 || s1.saturating_add(l1) <= s0
    };
    !(apart(a.row, a.row_span, b.row, b.row_span) || apart(a.col, a.col_span, b.col, b.col_span))
}

/// STU-DS-146 `grid.append_child_at(node, row_index, column_index)`: anchors `node` (a document
/// child of the GRID `container`) at the cell, keeping its spans (1 x 1 when it had none). Refused:
/// not a GRID container, not a child, out of range (anchor + span past the counts), and under
/// `MANUAL` a cell already covered by another anchored child. Returns the successor records.
pub fn grid_append_child_at(
    design: &Design,
    container: &str,
    node: &str,
    row: u32,
    column: u32,
) -> R<StudioDesignRecords> {
    let g = grid_container(design, container)?;
    if !design.children_of(container).contains(&node) {
        return Err(E::NoSuchNode(node.to_owned()));
    }
    let child_of = |id: &str| {
        design
            .node_design(id)
            .and_then(|d| d.child_layout.as_ref())
    };
    let old = child_of(node);
    let spans = old
        .and_then(|k| k.grid_child)
        .map_or((1, 1), |gc| (gc.grid_row_span, gc.grid_column_span));
    let gc = StudioGridChild {
        grid_row_anchor_index: row,
        grid_column_anchor_index: column,
        grid_row_span: spans.0,
        grid_column_span: spans.1,
    };
    let target = area(&gc, g).ok_or_else(|| E::InvalidLayout(format!("grid_out_of_range@{node}")))?;
    if g.grid_items_positioning == GridItemsPositioning::Manual {
        for other in design.children_of(container) {
            if *other == node {
                continue;
            }
            let taken = child_of(other)
                .filter(|k| k.layout_positioning == ChildPositioning::Auto)
                .and_then(|k| k.grid_child)
                .and_then(|o| area(&o, g));
            if taken.is_some_and(|a| overlaps(&a, &target)) {
                return Err(E::InvalidLayout(format!("grid_cell_occupied@{node}")));
            }
        }
    }
    let mut records = design.records().clone();
    let entry = match records.nodes.iter().position(|d| d.node.node_id == node) {
        Some(i) => &mut records.nodes[i],
        None => {
            records.nodes.push(StudioNodeDesign {
                node: NodeRef {
                    node_kind: NodeKind::Layer,
                    node_id: node.to_owned(),
                },
                auto_layout: None,
                child_layout: None,
                size_clamps: None,
                constraints: None,
                reactions: Vec::new(),
                component_property_references: BTreeMap::new(),
                explicit_variable_modes: BTreeMap::new(),
                style_refs: BTreeMap::new(),
            });
            records.nodes.last_mut().ok_or(E::InvalidGeometry)?
        }
    };
    let child = entry.child_layout.get_or_insert(StudioChildLayout {
        layout_align: ChildAlign::Inherit,
        layout_grow: 0,
        layout_positioning: ChildPositioning::Auto,
        layout_sizing_horizontal: ChildSizing::Fixed,
        layout_sizing_vertical: ChildSizing::Fixed,
        grid_child: None,
    });
    child.grid_child = Some(gc);
    accept(design, records)
}

/// New index of old track `k` after moving `from` to `to`.
fn moved(k: u32, from: u32, to: u32) -> u32 {
    if k == from {
        to
    } else if from < to && k > from && k <= to {
        k.saturating_sub(1)
    } else if to < from && k >= to && k < from {
        k.saturating_add(1)
    } else {
        k
    }
}

fn reorder(design: &Design, container: &str, m: &GridTrackMove, rows: bool) -> R<GridReorder> {
    let g = grid_container(design, container)?;
    let count = if rows {
        g.grid_row_count
    } else {
        g.grid_column_count
    };
    if m.from_index >= count || m.to_index >= count {
        return Err(E::InvalidLayout(format!("grid_out_of_range@{container}")));
    }
    let list = if rows {
        &g.grid_row_sizes
    } else {
        &g.grid_column_sizes
    };
    if list.len() != u(count) {
        return Err(E::InvalidLayout(format!("grid_tracks@{container}")));
    }
    let mut records = design.records().clone();
    for child in design.children_of(container) {
        let Some(entry) = records.nodes.iter_mut().find(|d| d.node.node_id == *child) else {
            continue;
        };
        let Some(gc) = entry.child_layout.as_mut().and_then(|k| k.grid_child.as_mut()) else {
            continue;
        };
        let (anchor, span) = if rows {
            (&mut gc.grid_row_anchor_index, gc.grid_row_span)
        } else {
            (&mut gc.grid_column_anchor_index, gc.grid_column_span)
        };
        let first = moved(*anchor, m.from_index, m.to_index);
        for k in 1..span {
            let old = anchor.saturating_add(k);
            if moved(old, m.from_index, m.to_index) != first.saturating_add(k) {
                return Err(E::InvalidLayout(format!("grid_reorder_splits_span@{child}")));
            }
        }
        *anchor = first;
    }
    let mut order: Vec<(u32, StudioGridTrackSize)> = Vec::with_capacity(list.len());
    for (k, t) in list.iter().enumerate() {
        order.push((u32::try_from(k).map_err(|_| E::BudgetExceeded)?, *t));
    }
    let item = order.remove(u(m.from_index));
    order.insert(u(m.to_index), item);
    let tracks: Vec<GridTrackEntry> = order
        .iter()
        .zip(0u32..)
        .map(|((previous_index, size), index)| GridTrackEntry {
            index,
            previous_index: *previous_index,
            size: *size,
        })
        .collect();
    let target = records
        .nodes
        .iter_mut()
        .find(|d| d.node.node_id == container)
        .and_then(|d| d.auto_layout.as_mut())
        .and_then(|a| a.grid.as_mut())
        .ok_or_else(|| E::InvalidLayout(format!("grid_not_a_grid@{container}")))?;
    let sizes = tracks.iter().map(|t| t.size).collect();
    if rows {
        target.grid_row_sizes = sizes;
    } else {
        target.grid_column_sizes = sizes;
    }
    Ok(GridReorder {
        records: accept(design, records)?,
        tracks,
    })
}

/// STU-DS-146 `grid.reorder_rows(options)`: the successor records and the resulting ordered row
/// tracks. Anchored children move with their rows; refused: not a GRID container, an index out of
/// range, or a move that would split a child spanning several rows.
pub fn grid_reorder_rows(design: &Design, container: &str, options: &GridTrackMove) -> R<GridReorder> {
    reorder(design, container, options, true)
}

/// STU-DS-146 `grid.reorder_columns(options)`: as [`grid_reorder_rows`] for columns.
pub fn grid_reorder_columns(
    design: &Design,
    container: &str,
    options: &GridTrackMove,
) -> R<GridReorder> {
    reorder(design, container, options, false)
}
