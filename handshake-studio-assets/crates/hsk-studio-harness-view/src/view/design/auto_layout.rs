//! Auto layout in the window (lane SMB-FIG-GRID-2; [STU-DS-144..150], bound contextually per
//! [STU-DS-160], every control with a headless Viewer path per [STU-CMP-110]): flow (HORIZONTAL /
//! VERTICAL) and GRID containers on the selected frame (a `group` layer), grid tracks, gaps and
//! options, a child's grid anchor and span and its sizing, and the three STU-DS-146 grid commands
//! (`hsk_studio_components::grid_append_child_at`, `grid_reorder_rows`, `grid_reorder_columns`).
//!
//! Records: the Folio design records of the vector entry's `design` member (the auto-layout entry of
//! the frame and the child-layout entries of its children, beside the paint styles). Every command
//! (1) edits those records, (2) validates them with Folio (`validate_design_records`; a refusal
//! passes through typed and nothing changes), (3) re-solves the container with
//! `hsk_studio_components::solve_auto_layout` (solver refusals pass through typed), (4) applies the
//! solved rects of the container's children to their geometry and (5) commits the document change
//! and the entry (with its `design`) as ONE undo step (`Run::set_vector_paths_and_entry`, as the
//! vector edits do).
//!
//! Geometry (Studio readings; Folio layers carry no geometry, the solver reads authored rects):
//! * A vector layer's rect is the union bounds of its paths; a group's (a frame's) rect is the bounds
//!   of its frame background when it has one, else the union of its children's rects. A text layer
//!   in the container's subtree is refused `auto_layout_text_layer_unsupported:<id>` (its position
//!   lives in the text entry and one undo step carries one entry); any other layer without vector
//!   geometry (a pixel layer, an empty group) is refused `auto_layout_child_without_geometry:<id>`.
//! * Frame background: a group's first (bottom-most) child when it is a vector layer whose bounds
//!   contain every other child's bounds (the fill layer `.fig` import draws first). It is not a layout
//!   item: the solver takes it as absolute and stretched with the frame (STU-DS-048) unless its own
//!   records say otherwise; this overlay is solver input only and is not written to the records.
//! * Applying a solved rect: every direct child of the container is translated by its offset (a
//!   group with everything inside it); a size change (FILL, HUG, a stretched background) is applied
//!   only to a rectangle (a vector layer whose single path is one closed four-corner axis-aligned
//!   contour without handles) or to a frame whose background is such a rectangle (the background is
//!   resized, the rest translated); any other size change is the typed refusal
//!   `auto_layout_resize_unsupported:<kind>` (`vector_path`, `group`, or the layer kind): paths are
//!   never scaled silently. Nested containers move rigidly with their parent (only the container's
//!   direct children are re-placed).
//! * New records: a frame entering auto layout gets the STU-DS-144 defaults with both axis sizing
//!   modes FIXED (the frame keeps its size, as the `.fig` import maps it). Switching to GRID without a
//!   grid record writes the STU-DS-146 defaults with the counts sized to hold the in-flow children:
//!   `columns = ceil(sqrt(n))`, `rows = ceil(n / columns)` (Studio reading, UNVERIFIED against Figma,
//!   D5). Leaving GRID drops the children's `grid_child`; NONE drops the frame's auto-layout record
//!   and its children's child layouts (Folio: a child layout needs an auto-layout parent).
//!   `set_child_sizing` keeps STU-DS-148 `layout_align` STRETCH exactly when the vertical sizing is
//!   FILL (the solver's agreement rule).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::super::vector::arrange::{transform_anchor_path, transform_network};
use super::super::vector::{self, PathRecord, VectorEntry};
use hsk_studio_components::{
    Design, GridTrackMove, LayoutLimits, LocalContent, NodeLocal, RectPt, grid_append_child_at, grid_reorder_columns, grid_reorder_rows, solve_auto_layout,
};
use hsk_studio_folio::{
    self as folio, AxisSizingMode, ChildAlign, ChildPositioning, ChildSizing, ConstraintSchema, ConstraintType, GridAutoTracks, GridItemsPositioning,
    GridTrackType, LayoutMode, Parent, Payload, StudioAutoLayout, StudioChildLayout, StudioConstraint, StudioDesignRecords, StudioDocument,
    StudioGridChild, StudioGridLayout, StudioGridTrackSize, StudioNodeDesign,
};
use hsk_studio_nib::PathPoint;
use hsk_studio_nib::arrange::{self as nib_arrange, Bounds};
use hsk_studio_nib::raster::Affine;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Stable widget ids of the auto-layout section.
pub mod widget {
    pub const SECTION: &str = "design.auto_layout";
    pub const SPACING: &str = "design.auto_layout.spacing";
    pub const ROWS: &str = "design.auto_layout.grid.rows";
    pub const COLUMNS: &str = "design.auto_layout.grid.columns";
    pub const ROW_GAP: &str = "design.auto_layout.grid.row_gap";
    pub const COLUMN_GAP: &str = "design.auto_layout.grid.column_gap";
    pub const AUTO_TRACKS: &str = "design.auto_layout.grid.auto_tracks";
    pub const ITEMS_POSITIONING: &str = "design.auto_layout.grid.items_positioning";
    pub const CHILD_ANCHORED: &str = "design.auto_layout.child.anchored";
    pub const ERROR: &str = "design.auto_layout.error";
    /// `NONE`, `HORIZONTAL`, `VERTICAL`, `GRID`.
    pub fn mode(token: &str) -> String {
        format!("design.auto_layout.mode.{token}")
    }
    /// `top`, `right`, `bottom`, `left`.
    pub fn padding(side: &str) -> String {
        format!("design.auto_layout.padding.{side}")
    }
    /// `axis` = `row` | `column`; `field` = `type` | `value` | `earlier` | `later`.
    pub fn track(axis: &str, index: usize, field: &str) -> String {
        format!("design.auto_layout.grid.{axis}.{index}.{field}")
    }
    /// `row`, `column`, `row_span`, `column_span`.
    pub fn child(field: &str) -> String {
        format!("design.auto_layout.child.{field}")
    }
    /// `horizontal`, `vertical`.
    pub fn sizing(axis: &str) -> String {
        format!("design.auto_layout.child.sizing.{axis}")
    }
}

/// Track rows the section draws editors for (a longer list shows its count; the commands take any
/// list Folio accepts).
pub const SECTION_TRACKS: usize = 64;
/// Most layers one container subtree may hold (the solver's own default node budget).
pub const MAX_SUBTREE: usize = 4096;
const EPS: f64 = 1e-9;

fn token(m: LayoutMode) -> &'static str {
    match m {
        LayoutMode::None => "NONE",
        LayoutMode::Horizontal => "HORIZONTAL",
        LayoutMode::Vertical => "VERTICAL",
        LayoutMode::Grid => "GRID",
    }
}

fn mode_of(t: &str) -> Option<LayoutMode> {
    Some(match t {
        "NONE" => LayoutMode::None,
        "HORIZONTAL" => LayoutMode::Horizontal,
        "VERTICAL" => LayoutMode::Vertical,
        "GRID" => LayoutMode::Grid,
        _ => return None,
    })
}

fn sizing_of(t: &str) -> Option<ChildSizing> {
    Some(match t {
        "FIXED" => ChildSizing::Fixed,
        "HUG" => ChildSizing::Hug,
        "FILL" => ChildSizing::Fill,
        _ => return None,
    })
}

fn sizing_token(s: ChildSizing) -> &'static str {
    match s {
        ChildSizing::Fixed => "FIXED",
        ChildSizing::Hug => "HUG",
        ChildSizing::Fill => "FILL",
    }
}

fn track_token(t: GridTrackType) -> &'static str {
    match t {
        GridTrackType::Fixed => "FIXED",
        GridTrackType::Flex => "FLEX",
        GridTrackType::Hug => "HUG",
    }
}

fn is_group(l: &folio::StudioLayer) -> bool {
    l.kind == "group" && matches!(l.payload, Payload::Group)
}

fn layer<'a>(doc: &'a StudioDocument, id: &str) -> Result<&'a folio::StudioLayer, String> {
    doc.layers.iter().find(|l| l.layer_id == id).ok_or_else(|| format!("auto_layout: no layer {id}"))
}

/// Child layers of `id` in document order (the order the solver lays them out).
fn children<'a>(doc: &'a StudioDocument, id: &str) -> Vec<&'a str> {
    doc.layers
        .iter()
        .filter(|l| matches!(&l.parent, Parent::Node(p) if p.node_id == id))
        .map(|l| l.layer_id.as_str())
        .collect()
}

fn parent_of<'a>(doc: &'a StudioDocument, id: &str) -> Option<&'a str> {
    doc.layers.iter().find(|l| l.layer_id == id).and_then(|l| match &l.parent {
        Parent::Node(p) => Some(p.node_id.as_str()),
        Parent::Root(()) => None,
    })
}

fn union(a: Option<Bounds>, b: Bounds) -> Bounds {
    a.map_or(b, |a| a.union(&b))
}

fn contains(outer: &Bounds, inner: &Bounds) -> bool {
    inner.min[0] >= outer.min[0] - EPS && inner.min[1] >= outer.min[1] - EPS && inner.max[0] <= outer.max[0] + EPS && inner.max[1] <= outer.max[1] + EPS
}

/// Read-only geometry of the open document's vector entry.
struct Geo<'a> {
    doc: &'a StudioDocument,
    entry: &'a VectorEntry,
}

impl Geo<'_> {
    /// Union bounds of a vector layer's own paths (`None`: not a vector layer or no geometry).
    fn paths(&self, id: &str) -> Result<Option<Bounds>, String> {
        let Payload::Vector { path_ids } = &layer(self.doc, id)?.payload else { return Ok(None) };
        let mut out = None;
        for p in path_ids {
            let record = self.entry.path(p).ok_or_else(|| format!("auto_layout: vector_path_unresolved {p}"))?;
            let owned = record.path.to_owned_path().map_err(|e| format!("auto_layout: {}", e.code()))?;
            if let Some(b) = nib_arrange::path_bounds(&owned) {
                out = Some(union(out, b));
            }
        }
        Ok(out)
    }

    /// The frame background of group `id` (module reading), if any.
    fn background(&self, id: &str) -> Result<Option<String>, String> {
        let kids = children(self.doc, id);
        let Some((first, rest)) = kids.split_first() else { return Ok(None) };
        let Some(bg) = self.paths(first)? else { return Ok(None) };
        for k in rest {
            if let Some(b) = self.bounds(k, 0)?
                && !contains(&bg, &b)
            {
                return Ok(None);
            }
        }
        Ok(Some((*first).to_owned()))
    }

    /// The rect of a layer (module reading); `None` without vector geometry.
    fn bounds(&self, id: &str, depth: usize) -> Result<Option<Bounds>, String> {
        if depth > 64 {
            return Err("auto_layout: nesting too deep".into());
        }
        let l = layer(self.doc, id)?;
        if !is_group(l) {
            return self.paths(id);
        }
        if let Some(bg) = self.background(id)? {
            return self.paths(&bg);
        }
        let mut out = None;
        for k in children(self.doc, id) {
            if let Some(b) = self.bounds(k, depth.saturating_add(1))? {
                out = Some(union(out, b));
            }
        }
        Ok(out)
    }

    fn rect(&self, id: &str) -> Result<Bounds, String> {
        if matches!(layer(self.doc, id)?.payload, Payload::Text { .. }) {
            // A text layer's position lives in the text entry; one undo step carries one entry.
            return Err(format!("auto_layout_text_layer_unsupported:{id}"));
        }
        self.bounds(id, 0)?.ok_or_else(|| format!("auto_layout_child_without_geometry:{id}"))
    }

    /// The single path of a rectangle layer (module reading), else `None`.
    fn rectangle(&self, id: &str) -> Result<Option<Bounds>, String> {
        let Payload::Vector { path_ids } = &layer(self.doc, id)?.payload else { return Ok(None) };
        let [only] = path_ids.as_slice() else { return Ok(None) };
        let record = self.entry.path(only).ok_or_else(|| format!("auto_layout: vector_path_unresolved {only}"))?;
        if record.network.is_some() {
            return Ok(None);
        }
        let [c] = record.path.contours.as_slice() else { return Ok(None) };
        if !c.closed || c.anchors.len() != 4 {
            return Ok(None);
        }
        let zero = |q: PathPoint| q.x == 0.0 && q.y == 0.0;
        if !c.anchors.iter().all(|a| zero(a.incoming) && zero(a.outgoing)) {
            return Ok(None);
        }
        let n = c.anchors.len();
        let axis_aligned = (0..n).all(|i| {
            let (a, b) = (c.anchors[i].position, c.anchors[i.checked_add(1).map_or(0, |j| j.checked_rem(n).unwrap_or(0))].position);
            (a.x - b.x).abs() <= EPS || (a.y - b.y).abs() <= EPS
        });
        if !axis_aligned {
            return Ok(None);
        }
        self.paths(id)
    }
}

/// Solver rects, absolute rects by layer id, and the frame backgrounds of a container subtree.
type Subtree = (LocalContent, BTreeMap<String, Bounds>, Vec<String>);

/// Every layer of the container's subtree with its rect relative to its parent's rect (the
/// container's own rect is absolute), and the frame backgrounds met on the way.
fn locals(geo: &Geo, container: &str) -> Result<Subtree, String> {
    let mut local = LocalContent::default();
    let mut abs = BTreeMap::new();
    let mut backgrounds = Vec::new();
    let mut stack: Vec<(String, Option<Bounds>, usize)> = vec![(container.to_owned(), None, 0)];
    while let Some((id, parent, depth)) = stack.pop() {
        if abs.len() >= MAX_SUBTREE || depth > 32 {
            return Err("auto_layout: container subtree over budget".into());
        }
        let b = geo.rect(&id)?;
        let at = parent.map_or([b.min[0], b.min[1]], |p| [b.min[0] - p.min[0], b.min[1] - p.min[1]]);
        local.nodes.insert(id.clone(), NodeLocal { rect: Some(RectPt { x: at[0], y: at[1], w: b.width(), h: b.height() }), ..NodeLocal::default() });
        abs.insert(id.clone(), b);
        if is_group(layer(geo.doc, &id)?) {
            if let Some(bg) = geo.background(&id)? {
                backgrounds.push(bg);
            }
            for k in children(geo.doc, &id) {
                stack.push((k.to_owned(), Some(b), depth.saturating_add(1)));
            }
        }
    }
    Ok((local, abs, backgrounds))
}

fn node_design(id: &str) -> Result<StudioNodeDesign, String> {
    serde_json::from_value(json!({"node": {"node_kind": "layer", "node_id": id}})).map_err(|e| format!("auto_layout: {e}"))
}

fn entry_of<'a>(records: &'a mut StudioDesignRecords, id: &str) -> Result<&'a mut StudioNodeDesign, String> {
    let at = match records.nodes.iter().position(|d| d.node.node_id == id) {
        Some(i) => i,
        None => {
            records.nodes.push(node_design(id)?);
            records.nodes.len().saturating_sub(1)
        }
    };
    records.nodes.get_mut(at).ok_or_else(|| "auto_layout: design entry".into())
}

/// Drops node entries that carry nothing.
fn prune(records: &mut StudioDesignRecords) {
    records.nodes.retain(|d| {
        d.auto_layout.is_some()
            || d.child_layout.is_some()
            || d.size_clamps.is_some()
            || d.constraints.is_some()
            || !d.reactions.is_empty()
            || !d.component_property_references.is_empty()
            || !d.explicit_variable_modes.is_empty()
            || !d.style_refs.is_empty()
    });
}

fn default_child() -> StudioChildLayout {
    StudioChildLayout {
        layout_align: ChildAlign::Inherit,
        layout_grow: 0,
        layout_positioning: ChildPositioning::Auto,
        layout_sizing_horizontal: ChildSizing::Fixed,
        layout_sizing_vertical: ChildSizing::Fixed,
        grid_child: None,
    }
}

fn ceil_div(a: u32, b: u32) -> u32 {
    a.checked_div(b).unwrap_or(0).saturating_add(u32::from(a.checked_rem(b).unwrap_or(0) != 0))
}

/// `ceil(sqrt(n))` for the default grid of `n` children (n >= 1).
fn ceil_sqrt(n: u32) -> u32 {
    let mut c = 1u32;
    while c.saturating_mul(c) < n {
        c = c.saturating_add(1);
    }
    c
}

/// The section's view of the selected layer.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoLayoutView {
    pub node: String,
    /// The selected layer is a frame (a group layer): the container controls apply.
    pub frame: bool,
    pub auto_layout: Option<StudioAutoLayout>,
    pub child_layout: Option<StudioChildLayout>,
    /// The parent's auto-layout record when the parent is an auto-layout container.
    pub parent: Option<(String, StudioAutoLayout)>,
}

impl AutoLayoutView {
    pub fn json(&self) -> Value {
        json!({
            "node": self.node,
            "frame": self.frame,
            "auto_layout": self.auto_layout,
            "child_layout": self.child_layout,
            "parent": self.parent.as_ref().map(|(id, a)| json!({"node": id, "layout_mode": token(a.layout_mode), "grid": a.grid})),
        })
    }
}

impl Viewer {
    fn al_doc(&self) -> Result<StudioDocument, String> {
        self.run.snapshot().map(|s| s.document().clone()).ok_or_else(|| "auto_layout: nothing open".to_string())
    }

    /// The design records of the vector entry (empty records of the document when it has none).
    fn al_records(&self, doc: &StudioDocument) -> Result<(VectorEntry, StudioDesignRecords), String> {
        let entry = self.vector_entry()?;
        let records = entry.design.clone().unwrap_or_else(|| StudioDesignRecords::new(doc.document_id.clone()));
        Ok((entry, records))
    }

    /// The section's view of `layer_id`; `None` when the section is not drawn for it (the document's
    /// vector entry has no design records, or the layer is neither a frame nor a child of an
    /// auto-layout container).
    pub fn auto_layout_view(&self, layer_id: &str) -> Option<AutoLayoutView> {
        let doc = self.run.snapshot()?.document();
        let entry = self.vector_entry().ok()?;
        let records = entry.design.as_ref()?;
        let l = doc.layers.iter().find(|l| l.layer_id == layer_id)?;
        let find = |id: &str| records.nodes.iter().find(|d| d.node.node_id == id);
        let parent = parent_of(doc, layer_id).and_then(|p| find(p).and_then(|d| d.auto_layout.clone()).filter(|a| a.layout_mode != LayoutMode::None).map(|a| (p.to_owned(), a)));
        let frame = is_group(l);
        if !frame && parent.is_none() {
            return None;
        }
        Some(AutoLayoutView {
            node: layer_id.to_owned(),
            frame,
            auto_layout: find(layer_id).and_then(|d| d.auto_layout.clone()),
            child_layout: find(layer_id).and_then(|d| d.child_layout.clone()),
            parent,
        })
    }

    /// `state_json()["design"]["auto_layout"]`: the selected layer's view and the last result line.
    pub fn auto_layout_json(&self) -> Value {
        let view = self.selected.as_deref().and_then(|l| self.auto_layout_view(l));
        let last = self.status.iter().rev().find(|s| s.starts_with("auto_layout")).cloned();
        json!({"selected": view.map(|v| v.json()), "last": last})
    }

    fn al_report<T>(&mut self, r: Result<T, String>) -> Result<T, String> {
        if let Err(e) = &r {
            self.say(format!("auto_layout refused: {e}"));
        }
        r
    }

    /// STU-DS-144 `layout_mode` of frame `node` (NONE, HORIZONTAL, VERTICAL, GRID); ONE undo step.
    pub fn set_auto_layout_mode(&mut self, node: &str, mode: LayoutMode) -> Result<(), String> {
        let r = (|| {
            let doc = self.al_doc()?;
            let (entry, mut records) = self.al_records(&doc)?;
            if !is_group(layer(&doc, node)?) {
                return Err(format!("auto_layout_not_a_frame:{node}"));
            }
            let geo = Geo { doc: &doc, entry: &entry };
            let bg = geo.background(node)?;
            let kids: Vec<&str> = children(&doc, node).into_iter().filter(|k| Some(*k) != bg.as_deref()).collect();
            let in_flow = u32::try_from(kids.iter().filter(|k| records.nodes.iter().find(|d| d.node.node_id == **k).and_then(|d| d.child_layout.as_ref()).is_none_or(|c| c.layout_positioning == ChildPositioning::Auto)).count()).map_err(|_| "auto_layout: children")?;
            for k in children(&doc, node) {
                if let Some(d) = records.nodes.iter_mut().find(|d| d.node.node_id == k) {
                    if mode == LayoutMode::None {
                        d.child_layout = None;
                    } else if mode != LayoutMode::Grid
                        && let Some(c) = d.child_layout.as_mut()
                    {
                        c.grid_child = None;
                    }
                }
            }
            if mode == LayoutMode::None {
                entry_of(&mut records, node)?.auto_layout = None;
            } else {
                let e = entry_of(&mut records, node)?;
                let a = e.auto_layout.get_or_insert_with(|| StudioAutoLayout {
                    primary_axis_sizing_mode: AxisSizingMode::Fixed,
                    counter_axis_sizing_mode: AxisSizingMode::Fixed,
                    ..StudioAutoLayout::none()
                });
                a.layout_mode = mode;
                if mode == LayoutMode::Grid {
                    if a.grid.is_none() {
                        let n = in_flow.max(1);
                        let columns = ceil_sqrt(n);
                        let rows = ceil_div(n, columns);
                        let hug = |k: u32| vec![StudioGridTrackSize::hug(); usize::try_from(k).unwrap_or(1)];
                        a.grid = Some(StudioGridLayout { grid_row_count: rows, grid_column_count: columns, grid_row_sizes: hug(rows), grid_column_sizes: hug(columns), ..StudioGridLayout::new() });
                    }
                } else {
                    a.grid = None;
                }
            }
            prune(&mut records);
            self.al_commit("auto_layout.mode", node, records)
        })();
        self.al_report(r)
    }

    /// Flow spacing and padding (top, right, bottom, left) of auto-layout frame `node`.
    pub fn set_auto_layout_spacing(&mut self, node: &str, item_spacing: f64, padding: [f64; 4]) -> Result<(), String> {
        let r = self.al_edit_container("auto_layout.spacing", node, |a| {
            a.item_spacing = item_spacing;
            [a.padding_top, a.padding_right, a.padding_bottom, a.padding_left] = padding;
            Ok(())
        });
        self.al_report(r)
    }

    /// STU-DS-146 track counts and sizes of GRID frame `node` (the counts are the list lengths);
    /// anchored children keep their anchors (a span past a new count is Folio's refusal).
    pub fn set_grid_tracks(&mut self, node: &str, rows: &[StudioGridTrackSize], columns: &[StudioGridTrackSize]) -> Result<(), String> {
        let r = self.al_edit_grid("auto_layout.grid_tracks", node, |g| {
            g.grid_row_count = u32::try_from(rows.len()).map_err(|_| "auto_layout: rows".to_string())?;
            g.grid_column_count = u32::try_from(columns.len()).map_err(|_| "auto_layout: columns".to_string())?;
            g.grid_row_sizes = rows.to_vec();
            g.grid_column_sizes = columns.to_vec();
            Ok(())
        });
        self.al_report(r)
    }

    /// STU-DS-145 grid gaps of GRID frame `node`.
    pub fn set_grid_gaps(&mut self, node: &str, row_gap: f64, column_gap: f64) -> Result<(), String> {
        let r = self.al_edit_grid("auto_layout.grid_gaps", node, |g| {
            g.grid_row_gap = row_gap;
            g.grid_column_gap = column_gap;
            Ok(())
        });
        self.al_report(r)
    }

    /// STU-DS-146 `grid_auto_tracks` and `grid_items_positioning` of GRID frame `node`.
    pub fn set_grid_options(&mut self, node: &str, auto_tracks: GridAutoTracks, positioning: GridItemsPositioning) -> Result<(), String> {
        let r = self.al_edit_grid("auto_layout.grid_options", node, |g| {
            g.grid_auto_tracks = auto_tracks;
            g.grid_items_positioning = positioning;
            Ok(())
        });
        self.al_report(r)
    }

    /// STU-DS-150 placement of `node` in its GRID parent (`None`: auto-flowed).
    pub fn set_grid_child(&mut self, node: &str, placement: Option<StudioGridChild>) -> Result<(), String> {
        let r = (|| {
            let doc = self.al_doc()?;
            let (_, mut records) = self.al_records(&doc)?;
            let parent = parent_of(&doc, node).ok_or_else(|| format!("auto_layout_no_parent:{node}"))?.to_owned();
            entry_of(&mut records, node)?.child_layout.get_or_insert_with(default_child).grid_child = placement;
            self.al_commit("auto_layout.grid_child", &parent, records)
        })();
        self.al_report(r)
    }

    /// STU-DS-148 sizing of child `node` (`layout_align` STRETCH exactly when vertical is FILL).
    pub fn set_child_sizing(&mut self, node: &str, horizontal: ChildSizing, vertical: ChildSizing) -> Result<(), String> {
        let r = (|| {
            let doc = self.al_doc()?;
            let (_, mut records) = self.al_records(&doc)?;
            let parent = parent_of(&doc, node).ok_or_else(|| format!("auto_layout_no_parent:{node}"))?.to_owned();
            let c = entry_of(&mut records, node)?.child_layout.get_or_insert_with(default_child);
            c.layout_sizing_horizontal = horizontal;
            c.layout_sizing_vertical = vertical;
            if vertical == ChildSizing::Fill {
                c.layout_align = ChildAlign::Stretch;
            } else if c.layout_align == ChildAlign::Stretch {
                c.layout_align = ChildAlign::Inherit;
            }
            self.al_commit("auto_layout.child_sizing", &parent, records)
        })();
        self.al_report(r)
    }

    /// STU-DS-146 `grid.append_child_at` (components command): anchors child `node` of GRID frame
    /// `container` at (`row`, `column`), keeping its spans.
    pub fn grid_append_child_at(&mut self, container: &str, node: &str, row: u32, column: u32) -> Result<(), String> {
        let r = (|| {
            let doc = self.al_doc()?;
            let (_, records) = self.al_records(&doc)?;
            let local = LocalContent::default();
            let design = Design::new(&doc, &records, &local).map_err(|e| format!("{}: {e}", e.code()))?;
            let next = grid_append_child_at(&design, container, node, row, column).map_err(|e| format!("{}: {e}", e.code()))?;
            self.al_commit("auto_layout.grid_append_child_at", container, next)
        })();
        self.al_report(r)
    }

    /// STU-DS-146 `grid.reorder_rows` (components command): moves the row at `from_index` to
    /// `to_index`; anchored children follow their rows.
    pub fn grid_reorder_rows(&mut self, container: &str, options: GridTrackMove) -> Result<(), String> {
        self.al_reorder(container, true, options.from_index, options.to_index)
    }

    /// STU-DS-146 `grid.reorder_columns` (components command), as [`Viewer::grid_reorder_rows`].
    pub fn grid_reorder_columns(&mut self, container: &str, options: GridTrackMove) -> Result<(), String> {
        self.al_reorder(container, false, options.from_index, options.to_index)
    }

    fn al_reorder(&mut self, container: &str, rows: bool, from_index: u32, to_index: u32) -> Result<(), String> {
        let r = (|| {
            let doc = self.al_doc()?;
            let (_, records) = self.al_records(&doc)?;
            let local = LocalContent::default();
            let design = Design::new(&doc, &records, &local).map_err(|e| format!("{}: {e}", e.code()))?;
            let m = GridTrackMove { from_index, to_index };
            let next = if rows { grid_reorder_rows(&design, container, &m) } else { grid_reorder_columns(&design, container, &m) }.map_err(|e| format!("{}: {e}", e.code()))?;
            self.al_commit(if rows { "auto_layout.grid_reorder_rows" } else { "auto_layout.grid_reorder_columns" }, container, next.records)
        })();
        self.al_report(r)
    }

    fn al_edit_container(&mut self, op: &str, node: &str, edit: impl FnOnce(&mut StudioAutoLayout) -> Result<(), String>) -> Result<(), String> {
        let doc = self.al_doc()?;
        let (_, mut records) = self.al_records(&doc)?;
        let a = records
            .nodes
            .iter_mut()
            .find(|d| d.node.node_id == node)
            .and_then(|d| d.auto_layout.as_mut())
            .ok_or_else(|| format!("auto_layout_not_a_container:{node}"))?;
        edit(a)?;
        self.al_commit(op, node, records)
    }

    fn al_edit_grid(&mut self, op: &str, node: &str, edit: impl FnOnce(&mut StudioGridLayout) -> Result<(), String>) -> Result<(), String> {
        self.al_edit_container(op, node, |a| match (a.layout_mode, a.grid.as_mut()) {
            (LayoutMode::Grid, Some(g)) => edit(g),
            _ => Err(format!("auto_layout_not_a_grid:{node}")),
        })
    }

    /// Validates `records` (Folio), solves `container`, applies the solved rects of its children and
    /// commits the moved paths and the entry (with `records`) as ONE undo step.
    fn al_commit(&mut self, op: &str, container: &str, records: StudioDesignRecords) -> Result<(), String> {
        if !vector::entry_persistable() {
            return Err(format!("{op}: vector_entry_not_persistable"));
        }
        let doc = self.al_doc()?;
        let (mut entry, _) = self.al_records(&doc)?;
        folio::validate_design_records(&records, &doc, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| format!("folio {:?} {}", d.code, d.target))?;
        if !is_group(layer(&doc, container)?) {
            return Err(format!("auto_layout_not_a_frame:{container}"));
        }
        let geo = Geo { doc: &doc, entry: &entry };
        let (local, abs, backgrounds) = locals(&geo, container)?;
        // Solver input: frame backgrounds are absolute and stretched unless their records say otherwise.
        let mut solve_records = records.clone();
        for bg in &backgrounds {
            let d = entry_of(&mut solve_records, bg)?;
            if d.child_layout.is_none() {
                d.child_layout = Some(StudioChildLayout { layout_positioning: ChildPositioning::Absolute, ..default_child() });
                d.constraints.get_or_insert(StudioConstraint { schema_id: ConstraintSchema::V1, horizontal: ConstraintType::Stretch, vertical: ConstraintType::Stretch });
            }
        }
        let design = Design::new(&doc, &solve_records, &local).map_err(|e| format!("{}: {e}", e.code()))?;
        let solved = solve_auto_layout(&design, container, None, &LayoutLimits::default(), self.cancel).map_err(|e| format!("{}: {e}", e.code()))?;
        let origin = abs.get(container).map(|b| b.min).ok_or("auto_layout: container rect")?;
        // One affine per vector layer to rewrite.
        let mut moves: BTreeMap<String, Affine> = BTreeMap::new();
        for child in children(&doc, container) {
            let (Some(b), Some(a)) = (solved.get(child), abs.get(child)) else { continue };
            let target = Bounds { min: [origin[0] + b.rect.x, origin[1] + b.rect.y], max: [origin[0] + b.rect.x + b.rect.w, origin[1] + b.rect.y + b.rect.h] };
            let (dx, dy) = (target.min[0] - a.min[0], target.min[1] - a.min[1]);
            let resized = (target.width() - a.width()).abs() > EPS || (target.height() - a.height()).abs() > EPS;
            if !resized && dx.abs() <= EPS && dy.abs() <= EPS {
                continue;
            }
            let l = layer(&doc, child)?;
            let translate: Affine = [1.0, 0.0, 0.0, 1.0, dx, dy];
            if is_group(l) {
                let bg = geo.background(child)?;
                if resized {
                    let rect = match &bg {
                        Some(bg) => geo.rectangle(bg)?,
                        None => None,
                    };
                    let (Some(bg), Some(old)) = (bg.as_ref(), rect) else {
                        return Err("auto_layout_resize_unsupported:group".into());
                    };
                    moves.insert(bg.clone(), fit(&old, &target)?);
                }
                let mut stack = vec![child.to_owned()];
                while let Some(id) = stack.pop() {
                    for k in children(&doc, &id) {
                        if Some(k) != bg.as_deref() || !resized {
                            stack.push(k.to_owned());
                        }
                    }
                    if matches!(layer(&doc, &id)?.payload, Payload::Vector { .. }) {
                        moves.entry(id).or_insert(translate);
                    }
                }
            } else if matches!(l.payload, Payload::Vector { .. }) {
                if resized {
                    let old = geo.rectangle(child)?.ok_or("auto_layout_resize_unsupported:vector_path")?;
                    moves.insert(child.to_owned(), fit(&old, &target)?);
                } else {
                    moves.insert(child.to_owned(), translate);
                }
            } else {
                return Err(format!("auto_layout_resize_unsupported:{}", l.kind));
            }
        }
        let writes = rewrite(&doc, &mut entry, &moves)?;
        entry.design = Some(records);
        entry.validate(&doc, self.cancel)?;
        let bytes = entry.to_bytes(&doc.document_id)?;
        // The entry must read back (the decode checks the design member's shape).
        VectorEntry::decode(&bytes, &doc, self.cancel)?;
        let ok = self.run.set_vector_paths_and_entry(op, &writes, Some((vector::ENTRY, bytes)));
        self.after_edit(ok, format!("{op} {container}")).map(|_| ())
    }

    /// Applies one control of the section (widget id and value text); the section and agents drive
    /// the commands through it. Values are parsed, never clamped; bounds are Folio's.
    pub fn auto_layout_control(&mut self, widget_id: &str, value: &str) -> Result<(), String> {
        let node = self.selected.clone().ok_or("auto_layout: nothing selected")?;
        let view = self.auto_layout_view(&node).ok_or("auto_layout: no section for the selected layer")?;
        let num = |v: &str| v.trim().parse::<f64>().map_err(|_| format!("auto_layout: {widget_id} needs a number, got `{v}`"));
        let int = |v: &str| v.trim().parse::<u32>().map_err(|_| format!("auto_layout: {widget_id} needs a whole number, got `{v}`"));
        (|| {
            if let Some(t) = widget_id.strip_prefix("design.auto_layout.mode.") {
                return self.set_auto_layout_mode(&node, mode_of(t).ok_or_else(|| format!("auto_layout: mode {t}"))?);
            }
            if let Some(axis) = widget_id.strip_prefix("design.auto_layout.child.sizing.") {
                let c = view.child_layout.clone().unwrap_or_else(default_child);
                let s = sizing_of(value.trim()).ok_or_else(|| format!("auto_layout: sizing {value}"))?;
                return match axis {
                    "horizontal" => self.set_child_sizing(&node, s, c.layout_sizing_vertical),
                    "vertical" => self.set_child_sizing(&node, c.layout_sizing_horizontal, s),
                    _ => Err(format!("auto_layout: {widget_id}")),
                };
            }
            if widget_id == widget::CHILD_ANCHORED {
                let on = value.trim() == "true";
                let place = on.then_some(StudioGridChild { grid_row_anchor_index: 0, grid_column_anchor_index: 0, grid_row_span: 1, grid_column_span: 1 });
                return self.set_grid_child(&node, place);
            }
            if let Some(field) = widget_id.strip_prefix("design.auto_layout.child.") {
                let mut g = view.child_layout.as_ref().and_then(|c| c.grid_child).ok_or("auto_layout: the child is not anchored")?;
                let v = int(value)?;
                match field {
                    "row" => g.grid_row_anchor_index = v,
                    "column" => g.grid_column_anchor_index = v,
                    "row_span" => g.grid_row_span = v,
                    "column_span" => g.grid_column_span = v,
                    _ => return Err(format!("auto_layout: {widget_id}")),
                }
                return self.set_grid_child(&node, Some(g));
            }
            let a = view.auto_layout.clone().ok_or("auto_layout: the frame has no auto layout")?;
            if widget_id == widget::SPACING {
                return self.set_auto_layout_spacing(&node, num(value)?, [a.padding_top, a.padding_right, a.padding_bottom, a.padding_left]);
            }
            if let Some(side) = widget_id.strip_prefix("design.auto_layout.padding.") {
                let mut p = [a.padding_top, a.padding_right, a.padding_bottom, a.padding_left];
                let i = ["top", "right", "bottom", "left"].iter().position(|s| *s == side).ok_or_else(|| format!("auto_layout: {widget_id}"))?;
                p[i] = num(value)?;
                return self.set_auto_layout_spacing(&node, a.item_spacing, p);
            }
            let g = a.grid.clone().ok_or("auto_layout: the frame is not a grid")?;
            match widget_id {
                widget::ROWS | widget::COLUMNS => {
                    let v = int(value)?;
                    if v > folio::GRID_COUNT_MAX {
                        // Past Folio's bound the count goes into the record as typed, without a track
                        // list (never allocated from the typed number): Folio refuses it.
                        let r = self.al_edit_grid("auto_layout.grid_tracks", &node, |x| {
                            if widget_id == widget::ROWS {
                                x.grid_row_count = v;
                            } else {
                                x.grid_column_count = v;
                            }
                            Ok(())
                        });
                        return self.al_report(r);
                    }
                    let n = usize::try_from(v).map_err(|_| "auto_layout: count")?;
                    let resize = |list: &[StudioGridTrackSize]| {
                        let mut l = list.to_vec();
                        l.resize(n, StudioGridTrackSize::hug());
                        l
                    };
                    if widget_id == widget::ROWS {
                        self.set_grid_tracks(&node, &resize(&g.grid_row_sizes), &g.grid_column_sizes)
                    } else {
                        self.set_grid_tracks(&node, &g.grid_row_sizes, &resize(&g.grid_column_sizes))
                    }
                }
                widget::ROW_GAP => self.set_grid_gaps(&node, num(value)?, g.grid_column_gap),
                widget::COLUMN_GAP => self.set_grid_gaps(&node, g.grid_row_gap, num(value)?),
                widget::AUTO_TRACKS => {
                    let t = match value.trim() {
                        "NONE" => GridAutoTracks::None,
                        "ROWS" => GridAutoTracks::Rows,
                        v => return Err(format!("auto_layout: auto tracks {v}")),
                    };
                    self.set_grid_options(&node, t, g.grid_items_positioning)
                }
                widget::ITEMS_POSITIONING => {
                    let p = match value.trim() {
                        "MANUAL" => GridItemsPositioning::Manual,
                        "ROW_AUTO_FLOW" => GridItemsPositioning::RowAutoFlow,
                        v => return Err(format!("auto_layout: items positioning {v}")),
                    };
                    self.set_grid_options(&node, g.grid_auto_tracks, p)
                }
                _ => {
                    let rest = widget_id.strip_prefix("design.auto_layout.grid.").ok_or_else(|| format!("auto_layout: unknown control {widget_id}"))?;
                    let mut parts = rest.split('.');
                    let (axis, index, field) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                    let rows = match axis {
                        "row" => true,
                        "column" => false,
                        _ => return Err(format!("auto_layout: unknown control {widget_id}")),
                    };
                    let i: u32 = index.parse().map_err(|_| format!("auto_layout: {widget_id}"))?;
                    let list = if rows { &g.grid_row_sizes } else { &g.grid_column_sizes };
                    let at = usize::try_from(i).map_err(|_| "auto_layout: index")?;
                    let current = *list.get(at).ok_or_else(|| format!("auto_layout: no track {i}"))?;
                    let changed = match field {
                        "type" => match value.trim() {
                            "FIXED" => StudioGridTrackSize::fixed(current.value.unwrap_or(0.0)),
                            "FLEX" => StudioGridTrackSize::flex(current.value.unwrap_or(1.0)),
                            "HUG" => StudioGridTrackSize::hug(),
                            v => return Err(format!("auto_layout: track type {v}")),
                        },
                        "value" => StudioGridTrackSize { value: Some(num(value)?), ..current },
                        "earlier" | "later" => {
                            let to = if field == "earlier" { i.checked_sub(1) } else { i.checked_add(1) }.ok_or("auto_layout: no track there")?;
                            return self.al_reorder(&node, rows, i, to);
                        }
                        _ => return Err(format!("auto_layout: unknown control {widget_id}")),
                    };
                    let mut l = list.clone();
                    if let Some(slot) = l.get_mut(at) {
                        *slot = changed;
                    }
                    if rows { self.set_grid_tracks(&node, &l, &g.grid_column_sizes) } else { self.set_grid_tracks(&node, &g.grid_row_sizes, &l) }
                }
            }
        })()
    }

    /// The auto-layout section of the Properties panel for the selected layer (drawn only for a frame
    /// or a child of an auto-layout container in a document with design records).
    pub(in crate::view) fn auto_layout_ui(&mut self, ui: &mut egui::Ui) {
        let Some(node) = self.selected.clone() else { return };
        let Some(view) = self.auto_layout_view(&node) else { return };
        let mut changes: Vec<(String, String)> = Vec::new();
        let mut drawn: Vec<String> = Vec::new();
        ui.separator();
        ui.push_id(super::super::widget::id(widget::SECTION), |ui| {
            ui.strong("Auto layout");
            if view.frame {
                let current = view.auto_layout.as_ref().map_or(LayoutMode::None, |a| a.layout_mode);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Mode");
                    for m in [LayoutMode::None, LayoutMode::Horizontal, LayoutMode::Vertical, LayoutMode::Grid] {
                        let id = widget::mode(token(m));
                        if ui.push_id(super::super::widget::id(&id), |ui| ui.selectable_label(m == current, token(m))).inner.clicked() && m != current {
                            changes.push((id.clone(), String::new()));
                        }
                        drawn.push(id);
                    }
                });
                if let Some(a) = &view.auto_layout {
                    if a.layout_mode.is_flow() {
                        field(ui, &mut drawn, &mut changes, widget::SPACING, "Spacing", a.item_spacing);
                        for (side, v) in [("top", a.padding_top), ("right", a.padding_right), ("bottom", a.padding_bottom), ("left", a.padding_left)] {
                            field(ui, &mut drawn, &mut changes, &widget::padding(side), &format!("Padding {side}"), v);
                        }
                    }
                    if let Some(g) = &a.grid {
                        field(ui, &mut drawn, &mut changes, widget::ROWS, "Rows", f64::from(g.grid_row_count));
                        field(ui, &mut drawn, &mut changes, widget::COLUMNS, "Columns", f64::from(g.grid_column_count));
                        for (axis, list) in [("row", &g.grid_row_sizes), ("column", &g.grid_column_sizes)] {
                            for (i, t) in list.iter().enumerate().take(SECTION_TRACKS) {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!("{axis} {i}"));
                                    for kind in [GridTrackType::Fixed, GridTrackType::Flex, GridTrackType::Hug] {
                                        let id = format!("{}.{}", widget::track(axis, i, "type"), track_token(kind));
                                        if ui.push_id(super::super::widget::id(&id), |ui| ui.selectable_label(t.track_type == kind, track_token(kind))).inner.clicked() && t.track_type != kind {
                                            changes.push((widget::track(axis, i, "type"), track_token(kind).into()));
                                        }
                                        drawn.push(id);
                                    }
                                    drawn.push(widget::track(axis, i, "type"));
                                    if let Some(v) = t.value {
                                        field(ui, &mut drawn, &mut changes, &widget::track(axis, i, "value"), "", v);
                                    }
                                    for (dir, label) in [("earlier", "<"), ("later", ">")] {
                                        let id = widget::track(axis, i, dir);
                                        if ui.push_id(super::super::widget::id(&id), |ui| ui.small_button(label)).inner.on_hover_text(format!("Reorder: move this {axis} {dir}")).clicked() {
                                            changes.push((id.clone(), String::new()));
                                        }
                                        drawn.push(id);
                                    }
                                });
                            }
                            if list.len() > SECTION_TRACKS {
                                ui.label(format!("{} more {axis}s", list.len().saturating_sub(SECTION_TRACKS)));
                            }
                        }
                        field(ui, &mut drawn, &mut changes, widget::ROW_GAP, "Row gap", g.grid_row_gap);
                        field(ui, &mut drawn, &mut changes, widget::COLUMN_GAP, "Column gap", g.grid_column_gap);
                        choice(ui, &mut drawn, &mut changes, widget::AUTO_TRACKS, "Auto tracks", &["NONE", "ROWS"], if g.grid_auto_tracks == GridAutoTracks::Rows { "ROWS" } else { "NONE" });
                        choice(
                            ui,
                            &mut drawn,
                            &mut changes,
                            widget::ITEMS_POSITIONING,
                            "Items",
                            &["ROW_AUTO_FLOW", "MANUAL"],
                            if g.grid_items_positioning == GridItemsPositioning::Manual { "MANUAL" } else { "ROW_AUTO_FLOW" },
                        );
                    }
                }
            }
            if let Some((_, parent)) = &view.parent {
                let c = view.child_layout.clone().unwrap_or_else(default_child);
                ui.label("In parent");
                choice(ui, &mut drawn, &mut changes, &widget::sizing("horizontal"), "Width", &["FIXED", "HUG", "FILL"], sizing_token(c.layout_sizing_horizontal));
                choice(ui, &mut drawn, &mut changes, &widget::sizing("vertical"), "Height", &["FIXED", "HUG", "FILL"], sizing_token(c.layout_sizing_vertical));
                if parent.layout_mode == LayoutMode::Grid {
                    let mut anchored = c.grid_child.is_some();
                    if ui.push_id(super::super::widget::id(widget::CHILD_ANCHORED), |ui| ui.checkbox(&mut anchored, "Anchored")).inner.changed() {
                        changes.push((widget::CHILD_ANCHORED.into(), anchored.to_string()));
                    }
                    drawn.push(widget::CHILD_ANCHORED.into());
                    if let Some(g) = c.grid_child {
                        for (f, label, v) in [("row", "Row", g.grid_row_anchor_index), ("column", "Column", g.grid_column_anchor_index), ("row_span", "Row span", g.grid_row_span), ("column_span", "Column span", g.grid_column_span)] {
                            field(ui, &mut drawn, &mut changes, &widget::child(f), label, f64::from(v));
                        }
                    }
                }
            }
            // The last refusal of this section, inline (Folio's and the solver's typed errors).
            if let Some(e) = ui.data(|d| d.get_temp::<String>(super::super::widget::id(widget::ERROR))) {
                ui.colored_label(egui::Color32::RED, e);
                drawn.push(widget::ERROR.into());
            }
        });
        drawn.push(widget::SECTION.into());
        self.drawn.extend(drawn);
        for (id, value) in changes {
            let r = self.auto_layout_control(&id, &value);
            let err_id = super::super::widget::id(widget::ERROR);
            ui.data_mut(|d| match r {
                Ok(()) => d.remove::<String>(err_id),
                Err(e) => d.insert_temp(err_id, e),
            });
        }
    }
}

/// A number field committed on focus loss (the text is passed on unparsed; nothing is clamped).
fn field(ui: &mut egui::Ui, drawn: &mut Vec<String>, changes: &mut Vec<(String, String)>, name: &str, label: &str, current: f64) {
    let id = super::super::widget::id(name);
    let shown = current.to_string();
    let focused = ui.memory(|m| m.has_focus(id));
    let mut buf = ui.data(|d| d.get_temp::<String>(id)).filter(|_| focused).unwrap_or_else(|| shown.clone());
    let r = ui
        .horizontal(|ui| {
            if !label.is_empty() {
                ui.label(label);
            }
            ui.add(egui::TextEdit::singleline(&mut buf).id(id).desired_width(64.0))
        })
        .inner;
    ui.data_mut(|d| d.insert_temp(id, buf.clone()));
    if r.lost_focus() && buf != shown {
        changes.push((name.to_owned(), buf));
    }
    drawn.push(name.to_owned());
}

/// A row of selectable tokens; a click passes the token on.
fn choice(ui: &mut egui::Ui, drawn: &mut Vec<String>, changes: &mut Vec<(String, String)>, name: &str, label: &str, tokens: &[&str], current: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        for t in tokens {
            let id = format!("{name}.{t}");
            if ui.push_id(super::super::widget::id(&id), |ui| ui.selectable_label(*t == current, *t)).inner.clicked() && *t != current {
                changes.push((name.to_owned(), (*t).to_owned()));
            }
            drawn.push(id);
        }
    });
    drawn.push(name.to_owned());
}

/// The affine taking rectangle `from` onto `to` (axis scale and translation).
fn fit(from: &Bounds, to: &Bounds) -> Result<Affine, String> {
    let (w, h) = (from.width(), from.height());
    if w <= EPS || h <= EPS || to.width() < 0.0 || to.height() < 0.0 {
        return Err("auto_layout_resize_unsupported:degenerate".into());
    }
    let (sx, sy) = (to.width() / w, to.height() / h);
    Ok([sx, 0.0, 0.0, sy, to.min[0] - from.min[0] * sx, to.min[1] - from.min[1] * sy])
}

/// A fresh `SVPT` id (the document id's time fields; never one the entry holds).
fn mint_path_id(doc: &StudioDocument, entry: &VectorEntry) -> String {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let mut n = u64::try_from(entry.paths.len()).unwrap_or(0);
    loop {
        let id = format!("SVPT-{high}-{low}-7ede-a200-{n:012x}");
        if entry.path(&id).is_none() {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// Rewrites every listed vector layer's paths under its affine into fresh records (mesh-envelope
/// points too, as the arrange tools do); returns the layers' new path id lists.
fn rewrite(doc: &StudioDocument, entry: &mut VectorEntry, moves: &BTreeMap<String, Affine>) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut writes = Vec::with_capacity(moves.len());
    let mut seen = BTreeSet::new();
    for (layer_id, m) in moves {
        if !seen.insert(layer_id.as_str()) || m.iter().any(|v| !v.is_finite()) {
            return Err("auto_layout: transforms".into());
        }
        let Payload::Vector { path_ids } = &layer(doc, layer_id)?.payload else {
            return Err(format!("auto_layout: {layer_id} is not a vector layer"));
        };
        let mut ids = Vec::with_capacity(path_ids.len());
        for id in path_ids {
            let record = entry.path(id).cloned().ok_or_else(|| format!("auto_layout: vector_path_unresolved {id}"))?;
            if entry.paths.len() >= vector::MAX_PATHS {
                return Err("auto_layout: vector_entry_over_budget".into());
            }
            let new_id = mint_path_id(doc, entry);
            let path = transform_anchor_path(&record.path, m);
            path.validate().map_err(|e| format!("auto_layout: {}", e.code()))?;
            let network = record.network.as_ref().map(|n| {
                let mut t = transform_network(n, m);
                t.path_id.clone_from(&new_id);
                t
            });
            entry.paths.push(PathRecord { path_id: new_id.clone(), path, style: record.style, network, width: record.width.clone() });
            ids.push(new_id);
        }
        for e in entry.records.envelopes.iter_mut().filter(|e| &e.layer_id == layer_id) {
            if let folio::StudioEnvelopeShape::Mesh { points, .. } = &mut e.envelope {
                for q in points.iter_mut() {
                    *q = [m[0] * q[0] + m[2] * q[1] + m[4], m[1] * q[0] + m[3] * q[1] + m[5]];
                }
            }
        }
        writes.push((layer_id.clone(), ids));
    }
    Ok(writes)
}
